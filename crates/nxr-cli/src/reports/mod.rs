//! Opt-in report writers after task runs (JUnit, SARIF, coverage, benchmark).

mod benchmark;
mod coverage;
mod junit;
mod sarif;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nxr_task::{Event, EventSink, NodeOutcome};

pub use benchmark::write_benchmark_report;
pub use coverage::write_coverage_report;
pub use junit::write_junit_report;
pub use sarif::write_sarif_report;

/// Supported post-run report kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportKind {
    Junit,
    Sarif,
    Coverage,
    Benchmark,
}

/// Output paths for opt-in report writers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReportPaths {
    pub junit: Option<PathBuf>,
    pub sarif: Option<PathBuf>,
    pub coverage: Option<PathBuf>,
    pub benchmark: Option<PathBuf>,
}

impl ReportPaths {
    /// Whether any report path was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.junit.is_none()
            && self.sarif.is_none()
            && self.coverage.is_none()
            && self.benchmark.is_none()
    }

    /// Set a path for `kind`, replacing any previous value.
    pub fn set(&mut self, kind: ReportKind, path: PathBuf) {
        match kind {
            ReportKind::Junit => self.junit = Some(path),
            ReportKind::Sarif => self.sarif = Some(path),
            ReportKind::Coverage => self.coverage = Some(path),
            ReportKind::Benchmark => self.benchmark = Some(path),
        }
    }

    /// Merge `other` into `self`; `other` wins for each set field.
    #[allow(dead_code)]
    pub fn merge(&mut self, other: Self) {
        if let Some(path) = other.junit {
            self.junit = Some(path);
        }
        if let Some(path) = other.sarif {
            self.sarif = Some(path);
        }
        if let Some(path) = other.coverage {
            self.coverage = Some(path);
        }
        if let Some(path) = other.benchmark {
            self.benchmark = Some(path);
        }
    }
}

/// Parse `--report kind=path` (case-insensitive kind).
///
/// # Errors
///
/// Returns [`ReportParseError`] when the spec is malformed.
pub fn parse_report_spec(spec: &str) -> Result<(ReportKind, PathBuf), ReportParseError> {
    let (kind, path) = spec
        .split_once('=')
        .ok_or(ReportParseError::MissingEquals)?;
    if path.is_empty() {
        return Err(ReportParseError::EmptyPath);
    }
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "junit" => ReportKind::Junit,
        "sarif" => ReportKind::Sarif,
        "coverage" => ReportKind::Coverage,
        "benchmark" => ReportKind::Benchmark,
        other => return Err(ReportParseError::UnknownKind(other.to_owned())),
    };
    Ok((kind, PathBuf::from(path)))
}

/// Errors parsing `--report` specifications.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReportParseError {
    #[error("report spec must be KIND=PATH (missing '=')")]
    MissingEquals,
    #[error("report path must not be empty")]
    EmptyPath,
    #[error("unknown report kind: {0}")]
    UnknownKind(String),
}

/// Errors writing report files.
#[derive(Debug, thiserror::Error)]
pub enum ReportWriteError {
    #[error("failed to create parent directory for {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize report: {0}")]
    Serialize(String),
}

/// One task node outcome captured from run events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeReport {
    pub name: String,
    pub code: Option<i32>,
    pub status: Option<NodeOutcome>,
    pub duration_ms: Option<u64>,
    pub reason: Option<String>,
}

/// Write every configured report from captured node outcomes.
///
/// # Errors
///
/// Returns [`ReportWriteError`] when a file cannot be written.
pub fn write_all_reports(
    paths: &ReportPaths,
    nodes: &[NodeReport],
    run_duration_ms: Option<u64>,
) -> Result<(), ReportWriteError> {
    if let Some(path) = &paths.junit {
        write_junit_report(path, nodes, run_duration_ms)?;
    }
    if let Some(path) = &paths.sarif {
        write_sarif_report(path, nodes)?;
    }
    if let Some(path) = &paths.coverage {
        write_coverage_report(path)?;
    }
    if let Some(path) = &paths.benchmark {
        write_benchmark_report(path)?;
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<(), ReportWriteError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| ReportWriteError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    Ok(())
}

pub(crate) fn write_text(path: &Path, contents: &str) -> Result<(), ReportWriteError> {
    ensure_parent(path)?;
    fs::write(path, contents).map_err(|source| ReportWriteError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn write_json_value(
    path: &Path,
    value: &serde_json::Value,
) -> Result<(), ReportWriteError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|error| ReportWriteError::Serialize(error.to_string()))?;
    write_text(path, &(rendered + "\n"))
}

/// Wraps an [`EventSink`] and writes configured reports on [`Event::RunCompleted`].
pub struct ReportCollector<S> {
    inner: S,
    paths: ReportPaths,
    nodes: Vec<NodeReport>,
    run_duration_ms: Option<u64>,
    write_error: Option<String>,
}

impl<S> ReportCollector<S> {
    /// Create a collector that forwards events to `inner`.
    #[must_use]
    pub fn new(inner: S, paths: ReportPaths) -> Self {
        Self {
            inner,
            paths,
            nodes: Vec::new(),
            run_duration_ms: None,
            write_error: None,
        }
    }

    /// Borrow the inner sink.
    #[allow(dead_code)]
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Recover the inner sink.
    #[allow(dead_code)]
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Report write error observed during [`Event::RunCompleted`], if any.
    #[must_use]
    pub fn write_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }
}

impl<S: EventSink> EventSink for ReportCollector<S> {
    fn emit(&mut self, event: Event) {
        match &event {
            Event::NodeExited {
                node,
                code,
                status,
                duration_ms,
                reason,
                ..
            } => {
                self.nodes.push(NodeReport {
                    name: node.clone(),
                    code: *code,
                    status: *status,
                    duration_ms: *duration_ms,
                    reason: reason.clone(),
                });
            }
            Event::RunCompleted { duration_ms, .. } => {
                self.run_duration_ms = *duration_ms;
                if let Err(error) =
                    write_all_reports(&self.paths, &self.nodes, self.run_duration_ms)
                {
                    self.write_error = Some(error.to_string());
                }
            }
            _ => {}
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report_spec_accepts_known_kinds() {
        let (kind, path) = parse_report_spec("junit=out/results.xml").expect("parse");
        assert_eq!(kind, ReportKind::Junit);
        assert_eq!(path, PathBuf::from("out/results.xml"));
    }

    #[test]
    fn parse_report_spec_rejects_unknown_kind() {
        assert_eq!(
            parse_report_spec("xml=out.xml"),
            Err(ReportParseError::UnknownKind("xml".to_owned()))
        );
    }
}
