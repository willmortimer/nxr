//! Non-blocking multiplexed reads from many child stdout/stderr pipes.
//!
//! Unix builds use a single `mio` poll loop (kqueue/epoll). Other platforms
//! fall back to one reader thread per pipe.

use std::collections::HashMap;
use std::io;
use std::process::{ChildStderr, ChildStdout};
use std::time::Duration;

/// Which standard stream produced a [`PipeChunk`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PipeStream {
    /// Child stdout.
    Stdout,
    /// Child stderr.
    Stderr,
}

/// One read from a supervised child's piped output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeChunk {
    /// Compact node id assigned by [`PipeMultiplexer::intern_node`].
    pub node: u32,
    pub stream: PipeStream,
    /// Raw bytes (UTF-8 decoding is the consumer's responsibility).
    pub bytes: Vec<u8>,
}

/// Multiplexes many piped children without one OS thread per fd.
#[derive(Debug)]
pub struct PipeMultiplexer {
    #[cfg(unix)]
    inner: unix::Inner,
    #[cfg(not(unix))]
    inner: fallback::Inner,
    node_names: Vec<String>,
    node_ids: HashMap<String, u32>,
}

impl Default for PipeMultiplexer {
    fn default() -> Self {
        Self::new()
    }
}

impl PipeMultiplexer {
    /// Create an empty multiplexer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(unix)]
            inner: unix::Inner::new(),
            #[cfg(not(unix))]
            inner: fallback::Inner::new(),
            node_names: Vec::new(),
            node_ids: HashMap::new(),
        }
    }

    /// Assign or reuse a compact id for `node_id`.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct node ids are interned.
    pub fn intern_node(&mut self, node_id: impl Into<String>) -> u32 {
        let node_id = node_id.into();
        if let Some(&id) = self.node_ids.get(&node_id) {
            return id;
        }
        let id = u32::try_from(self.node_names.len()).expect("node id overflow");
        self.node_names.push(node_id.clone());
        self.node_ids.insert(node_id, id);
        id
    }

    /// Resolve a compact id back to the caller-facing node label.
    #[must_use]
    pub fn node_label(&self, node: u32) -> &str {
        let Ok(index) = usize::try_from(node) else {
            return "<unknown>";
        };
        self.node_names
            .get(index)
            .map_or("<unknown>", String::as_str)
    }

    /// Register a stdout pipe for `node`.
    ///
    /// # Errors
    ///
    /// Propagates poll registration failures.
    pub fn register_stdout(&mut self, node: u32, pipe: ChildStdout) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.inner.register_stdout(node, pipe)
        }
        #[cfg(not(unix))]
        {
            self.inner.register_stdout(node, pipe)
        }
    }

    /// Register a stderr pipe for `node`.
    ///
    /// # Errors
    ///
    /// Propagates poll registration failures.
    pub fn register_stderr(&mut self, node: u32, pipe: ChildStderr) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.inner.register_stderr(node, pipe)
        }
        #[cfg(not(unix))]
        {
            self.inner.register_stderr(node, pipe)
        }
    }

    /// Drop every registration for `node` (typically after the child exits).
    pub fn remove_node(&mut self, node: u32) {
        self.inner.remove_node(node);
    }

    /// Whether `node` still has registered stdout/stderr pipes.
    #[must_use]
    pub fn has_pipes(&self, node: u32) -> bool {
        self.inner.has_pipes(node)
    }

    /// Wait up to `timeout` for readable pipes and deliver chunks to `on_chunk`.
    ///
    /// # Errors
    ///
    /// Propagates poll or read errors other than `WouldBlock` / `Interrupted`.
    pub fn poll<F>(&mut self, timeout: Duration, mut on_chunk: F) -> io::Result<()>
    where
        F: FnMut(PipeChunk),
    {
        self.inner.poll(timeout, &mut on_chunk)
    }
}

#[cfg(unix)]
mod unix {
    use super::{PipeChunk, PipeStream};
    use mio::event::{Event, Source};
    use mio::{Events, Interest, Poll, Registry, Token};
    use std::collections::HashMap;
    use std::io::{self, Read};
    use std::os::fd::AsFd;
    use std::os::unix::io::AsRawFd;
    use std::process::{ChildStderr, ChildStdout};
    use std::time::Duration;

    use mio::unix::SourceFd;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    /// Reusable read buffer size (within the 16–64 KiB guidance).
    const READ_BUFFER_SIZE: usize = 32 * 1024;
    /// Max bytes to read from one fd per poll wake so peers get a turn.
    const READ_FAIRNESS_BUDGET: usize = 1024 * 1024;

    enum PipeReader {
        Stdout(ChildStdout),
        Stderr(ChildStderr),
    }

    impl Read for PipeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self {
                Self::Stdout(pipe) => pipe.read(buf),
                Self::Stderr(pipe) => pipe.read(buf),
            }
        }
    }

    impl PipeReader {
        fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
            match self {
                Self::Stdout(pipe) => pipe.as_raw_fd(),
                Self::Stderr(pipe) => pipe.as_raw_fd(),
            }
        }
    }

    struct PipeSource {
        node: u32,
        stream: PipeStream,
        reader: PipeReader,
    }

    impl std::fmt::Debug for PipeSource {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PipeSource")
                .field("node", &self.node)
                .field("stream", &self.stream)
                .finish_non_exhaustive()
        }
    }

    impl Source for PipeSource {
        fn register(
            &mut self,
            registry: &Registry,
            token: Token,
            interests: Interest,
        ) -> io::Result<()> {
            SourceFd(&self.reader.as_raw_fd()).register(registry, token, interests)
        }

        fn reregister(
            &mut self,
            registry: &Registry,
            token: Token,
            interests: Interest,
        ) -> io::Result<()> {
            SourceFd(&self.reader.as_raw_fd()).reregister(registry, token, interests)
        }

        fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
            SourceFd(&self.reader.as_raw_fd()).deregister(registry)
        }
    }

    #[derive(Debug)]
    pub(super) struct Inner {
        poll: Poll,
        events: Events,
        sources: Vec<Option<PipeSource>>,
        free_tokens: Vec<usize>,
        node_tokens: HashMap<u32, Vec<usize>>,
        buffer: Vec<u8>,
    }

    impl Inner {
        pub(super) fn new() -> Self {
            Self {
                poll: Poll::new().expect("mio Poll::new"),
                events: Events::with_capacity(64),
                sources: Vec::new(),
                free_tokens: Vec::new(),
                node_tokens: HashMap::new(),
                buffer: vec![0_u8; READ_BUFFER_SIZE],
            }
        }

        pub(super) fn register_stdout(&mut self, node: u32, pipe: ChildStdout) -> io::Result<()> {
            set_nonblocking(&pipe)?;
            self.register(node, PipeStream::Stdout, PipeReader::Stdout(pipe))
        }

        pub(super) fn register_stderr(&mut self, node: u32, pipe: ChildStderr) -> io::Result<()> {
            set_nonblocking(&pipe)?;
            self.register(node, PipeStream::Stderr, PipeReader::Stderr(pipe))
        }

        fn register(
            &mut self,
            node: u32,
            stream: PipeStream,
            reader: PipeReader,
        ) -> io::Result<()> {
            let token_index = self.free_tokens.pop().unwrap_or_else(|| {
                let index = self.sources.len();
                self.sources.push(None);
                index
            });
            let token = Token(token_index);
            let mut source = PipeSource {
                node,
                stream,
                reader,
            };
            self.poll
                .registry()
                .register(&mut source, token, Interest::READABLE)?;
            self.sources[token_index] = Some(source);
            self.node_tokens.entry(node).or_default().push(token_index);
            Ok(())
        }

        pub(super) fn remove_node(&mut self, node: u32) {
            let Some(tokens) = self.node_tokens.remove(&node) else {
                return;
            };
            for token_index in tokens {
                self.close_source(token_index);
            }
        }

        pub(super) fn has_pipes(&self, node: u32) -> bool {
            self.node_tokens.contains_key(&node)
        }

        pub(super) fn poll<F>(&mut self, timeout: Duration, on_chunk: &mut F) -> io::Result<()>
        where
            F: FnMut(PipeChunk),
        {
            self.poll
                .poll(&mut self.events, Some(timeout))
                .map_err(io::Error::other)?;

            let events: Vec<Event> = self.events.iter().cloned().collect();
            for event in events {
                if event.is_readable() || event.is_read_closed() {
                    self.read_ready(event.token().0, on_chunk)?;
                }
            }
            Ok(())
        }

        fn read_ready<F>(&mut self, token_index: usize, on_chunk: &mut F) -> io::Result<()>
        where
            F: FnMut(PipeChunk),
        {
            let mut budget = READ_FAIRNESS_BUDGET;
            loop {
                if budget == 0 {
                    break;
                }

                let Some(source) = self.sources.get_mut(token_index).and_then(Option::as_mut)
                else {
                    return Ok(());
                };
                let node = source.node;
                let stream = source.stream;
                match source.reader.read(&mut self.buffer[..]) {
                    Ok(0) => {
                        self.close_source(token_index);
                        break;
                    }
                    Ok(count) => {
                        budget = budget.saturating_sub(count);
                        on_chunk(PipeChunk {
                            node,
                            stream,
                            bytes: self.buffer[..count].to_vec(),
                        });
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        break;
                    }
                    Err(error) => {
                        self.close_source(token_index);
                        return Err(error);
                    }
                }
            }
            Ok(())
        }

        fn close_source(&mut self, token_index: usize) {
            if let Some(mut source) = self.sources[token_index].take() {
                let _ = source.deregister(self.poll.registry());
            }
            self.free_tokens.push(token_index);
            self.node_tokens
                .values_mut()
                .for_each(|tokens| tokens.retain(|index| *index != token_index));
            self.node_tokens.retain(|_, tokens| !tokens.is_empty());
        }
    }

    fn set_nonblocking(pipe: &impl AsFd) -> io::Result<()> {
        let borrowed = pipe.as_fd();
        let flags = fcntl(borrowed, FcntlArg::F_GETFL).map_err(io::Error::other)?;
        let new_flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(borrowed, FcntlArg::F_SETFL(new_flags)).map_err(io::Error::other)?;
        Ok(())
    }
}

#[cfg(not(unix))]
mod fallback {
    use super::{PipeChunk, PipeStream};
    use std::collections::HashMap;
    use std::io::{self, Read};
    use std::process::{ChildStderr, ChildStdout};
    use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
    use std::thread;
    use std::time::Duration;

    struct ReaderThread {
        join: Option<thread::JoinHandle<()>>,
    }

    impl ReaderThread {
        fn spawn(
            node: u32,
            stream: PipeStream,
            mut reader: impl Read + Send + 'static,
            tx: Sender<PipeChunk>,
        ) -> Self {
            let join = thread::spawn(move || {
                let mut buffer = [0_u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            if tx
                                .send(PipeChunk {
                                    node,
                                    stream,
                                    bytes: buffer[..count].to_vec(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            });
            Self { join: Some(join) }
        }
    }

    impl Drop for ReaderThread {
        fn drop(&mut self) {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct Inner {
        tx: Sender<PipeChunk>,
        rx: Receiver<PipeChunk>,
        readers: Vec<ReaderThread>,
        node_readers: HashMap<u32, Vec<usize>>,
    }

    impl Inner {
        pub(super) fn new() -> Self {
            let (tx, rx) = mpsc::channel();
            Self {
                tx,
                rx,
                readers: Vec::new(),
                node_readers: HashMap::new(),
            }
        }

        pub(super) fn register_stdout(&mut self, node: u32, pipe: ChildStdout) -> io::Result<()> {
            self.register_reader(node, PipeStream::Stdout, pipe)
        }

        pub(super) fn register_stderr(&mut self, node: u32, pipe: ChildStderr) -> io::Result<()> {
            self.register_reader(node, PipeStream::Stderr, pipe)
        }

        fn register_reader(
            &mut self,
            node: u32,
            stream: PipeStream,
            pipe: impl Read + Send + 'static,
        ) -> io::Result<()> {
            let index = self.readers.len();
            self.readers
                .push(ReaderThread::spawn(node, stream, pipe, self.tx.clone()));
            self.node_readers.entry(node).or_default().push(index);
            Ok(())
        }

        pub(super) fn remove_node(&mut self, node: u32) {
            if let Some(indices) = self.node_readers.remove(&node) {
                for index in indices.into_iter().rev() {
                    let _ = self.readers.remove(index);
                }
            }
        }

        pub(super) fn has_pipes(&self, node: u32) -> bool {
            self.node_readers.contains_key(&node)
        }

        pub(super) fn poll<F>(&mut self, timeout: Duration, on_chunk: &mut F) -> io::Result<()>
        where
            F: FnMut(PipeChunk),
        {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                match self.rx.try_recv() {
                    Ok(chunk) => on_chunk(chunk),
                    Err(TryRecvError::Empty) => {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PipeMultiplexer, PipeStream};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[test]
    fn multiplexes_stdout_and_stderr_from_child() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf out; printf err 1>&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sh");

        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");

        let mut mux = PipeMultiplexer::new();
        let node = mux.intern_node("demo");
        mux.register_stdout(node, stdout).expect("register stdout");
        mux.register_stderr(node, stderr).expect("register stderr");

        let mut chunks = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            mux.poll(Duration::from_millis(20), |chunk| chunks.push(chunk))
                .expect("poll");
            if chunks.len() >= 2 {
                break;
            }
        }
        let _ = child.wait();

        assert_eq!(mux.node_label(node), "demo");
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.stream == PipeStream::Stdout && chunk.bytes == b"out"),
            "stdout chunk missing: {chunks:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.stream == PipeStream::Stderr && chunk.bytes == b"err"),
            "stderr chunk missing: {chunks:?}"
        );
    }

    #[test]
    fn many_children_share_one_poll_loop() {
        let child_count = 16usize;
        let mut children = Vec::with_capacity(child_count);
        let mut mux = PipeMultiplexer::new();

        for index in 0..child_count {
            let mut child = Command::new("/bin/sh")
                .args(["-c", &format!("printf node-{index}")])
                .stdout(Stdio::piped())
                .stdin(Stdio::null())
                .spawn()
                .expect("spawn sh");
            let stdout = child.stdout.take().expect("stdout");
            let node = mux.intern_node(format!("node-{index}"));
            mux.register_stdout(node, stdout).expect("register");
            children.push(child);
        }

        let mut seen = std::collections::BTreeSet::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen.len() < child_count && std::time::Instant::now() < deadline {
            let mut pending = Vec::new();
            mux.poll(Duration::from_millis(20), |chunk| pending.push(chunk))
                .expect("poll");
            for chunk in pending {
                if chunk.stream == PipeStream::Stdout {
                    let label = mux.node_label(chunk.node).to_owned();
                    seen.insert((label, chunk.bytes));
                }
            }
        }

        for mut child in children {
            let _ = child.wait();
        }

        assert_eq!(seen.len(), child_count);
    }

    #[test]
    fn drains_more_than_one_buffer_per_poll_wake() {
        const READ_BUFFER_SIZE: usize = 32 * 1024;
        let payload_size = READ_BUFFER_SIZE * 2 + 100;
        // Prefer coreutils over perl — hermetic Nix sandboxes often lack perl.
        // `2>/dev/null` keeps this portable (GNU `status=none` is not on BSD dd).
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                &format!("dd if=/dev/zero bs={payload_size} count=1 2>/dev/null | tr '\\0' 'x'"),
            ])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sh");

        let stdout = child.stdout.take().expect("stdout");
        let mut mux = PipeMultiplexer::new();
        let node = mux.intern_node("large");
        mux.register_stdout(node, stdout).expect("register stdout");

        let mut chunk_sizes = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            mux.poll(Duration::from_millis(20), |chunk| {
                chunk_sizes.push(chunk.bytes.len());
            })
            .expect("poll");
            if chunk_sizes.iter().sum::<usize>() >= payload_size {
                break;
            }
        }
        let _ = child.wait();

        assert_eq!(
            chunk_sizes.iter().sum::<usize>(),
            payload_size,
            "incomplete drain: {chunk_sizes:?}"
        );
        assert!(
            chunk_sizes.len() > 1,
            "expected multiple reads per poll wake: {chunk_sizes:?}"
        );
    }

    #[test]
    fn poll_returns_without_blocking_when_child_has_no_output() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 2"])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sh");

        let stdout = child.stdout.take().expect("stdout");
        let mut mux = PipeMultiplexer::new();
        let node = mux.intern_node("quiet");
        mux.register_stdout(node, stdout).expect("register stdout");

        let start = std::time::Instant::now();
        mux.poll(Duration::ZERO, |_| panic!("unexpected chunk"))
            .expect("poll");
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "poll blocked for {:?}",
            start.elapsed()
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn rapid_exit_child_tail_is_drained() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf trailing; exit 0"])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sh");

        let stdout = child.stdout.take().expect("stdout");
        let status = child.wait().expect("wait");
        assert!(status.success());

        let mut mux = PipeMultiplexer::new();
        let node = mux.intern_node("rapid");
        mux.register_stdout(node, stdout).expect("register stdout");

        let mut bytes = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while mux.has_pipes(node) && std::time::Instant::now() < deadline {
            mux.poll(Duration::from_millis(20), |chunk| {
                bytes.extend_from_slice(&chunk.bytes)
            })
            .expect("poll");
        }

        assert_eq!(bytes, b"trailing");
    }

    #[test]
    fn has_pipes_tracks_registration_and_eof() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf done"])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sh");

        let stdout = child.stdout.take().expect("stdout");
        let mut mux = PipeMultiplexer::new();
        let node = mux.intern_node("demo");
        mux.register_stdout(node, stdout).expect("register stdout");
        assert!(mux.has_pipes(node));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while mux.has_pipes(node) && std::time::Instant::now() < deadline {
            mux.poll(Duration::from_millis(20), |_| {}).expect("poll");
        }
        let _ = child.wait();

        assert!(!mux.has_pipes(node));
    }
}
