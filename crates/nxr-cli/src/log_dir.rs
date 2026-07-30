//! Tee child stdout/stderr chunks into a per-node log directory.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use nxr_task::{Event, EventSink, OutputPayload};

/// Wraps an [`EventSink`] and appends stdout/stderr chunks under `dir`.
pub struct LogDirTee<S> {
    inner: S,
    dir: Option<PathBuf>,
    files: BTreeMap<(String, StreamKind), File>,
    write_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum StreamKind {
    Stdout,
    Stderr,
}

impl StreamKind {
    const fn as_suffix(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

impl<S> LogDirTee<S> {
    /// Create a tee. When `dir` is `None`, events are forwarded only.
    #[must_use]
    pub fn new(inner: S, dir: Option<PathBuf>) -> Self {
        Self {
            inner,
            dir,
            files: BTreeMap::new(),
            write_error: None,
        }
    }

    /// First log-dir I/O error observed, if any.
    #[must_use]
    pub fn write_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }

    fn ensure_dir(&mut self) -> Result<(), io::Error> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        fs::create_dir_all(dir)
    }

    fn file_for(&mut self, node: &str, kind: StreamKind) -> Result<&mut File, io::Error> {
        let key = (node.to_owned(), kind);
        if !self.files.contains_key(&key) {
            self.ensure_dir()?;
            let Some(dir) = &self.dir else {
                return Err(io::Error::other("log dir unset"));
            };
            let path = dir.join(format!("{}.{}", sanitize_node_name(node), kind.as_suffix()));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            self.files.insert(key.clone(), file);
        }
        Ok(self
            .files
            .get_mut(&key)
            .expect("just inserted or previously present"))
    }

    fn write_chunk(&mut self, node: &str, kind: StreamKind, payload: &OutputPayload) {
        if self.dir.is_none() || self.write_error.is_some() {
            return;
        }
        let bytes = payload.as_bytes();
        if let Err(error) = self
            .file_for(node, kind)
            .and_then(|file| file.write_all(bytes).and_then(|()| file.flush()))
        {
            self.write_error = Some(format!("log-dir write failed: {error}"));
        }
    }
}

impl<S: EventSink> EventSink for LogDirTee<S> {
    fn emit(&mut self, event: Event) {
        match &event {
            Event::StdoutChunk { node, payload } => {
                self.write_chunk(node, StreamKind::Stdout, payload);
            }
            Event::StderrChunk { node, payload } => {
                self.write_chunk(node, StreamKind::Stderr, payload);
            }
            Event::PlanCreated { .. } => {
                if let Err(error) = self.ensure_dir() {
                    self.write_error = Some(format!("log-dir create failed: {error}"));
                }
            }
            _ => {}
        }
        self.inner.emit(event);
    }
}

fn sanitize_node_name(node: &str) -> String {
    node.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '_',
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' => c,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxr_task::{NullSink, OutputPayload};

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize_node_name("a/b:c"), "a_b_c");
    }

    #[test]
    fn tee_writes_per_node_files() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut sink = LogDirTee::new(NullSink, Some(dir.path().to_path_buf()));
        sink.emit(Event::plan_created("root".to_owned(), None, 1));
        sink.emit(Event::StdoutChunk {
            node: "leaf".to_owned(),
            payload: OutputPayload::utf8("hello\n"),
        });
        assert!(sink.write_error().is_none());
        let contents = fs::read_to_string(dir.path().join("leaf.stdout")).expect("read");
        assert_eq!(contents, "hello\n");
    }
}
