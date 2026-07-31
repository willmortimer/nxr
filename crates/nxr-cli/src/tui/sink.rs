//! [`EventSink`] implementation that drives the Ratatui DAG watch.

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers};
use nxr_core::RunTargetKind;
use nxr_task::{Event, EventSink};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::draw::draw_watch;
use super::runtime::TerminalGuard;
use super::session::AttachSession;
use super::state::WatchState;

/// Live TUI renderer fed by the task event stream.
pub struct TuiEventSink {
    state: WatchState,
    session: Option<AttachSession>,
    target_kind: RunTargetKind,
    target: String,
    log_dir: Option<std::path::PathBuf>,
    title: String,
    terminal: Option<TerminalGuard>,
    ratatui: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    last_draw: Instant,
    finished: bool,
}

impl TuiEventSink {
    /// Begin a live TUI session for the given run metadata.
    #[must_use]
    pub fn new(
        target_kind: RunTargetKind,
        target: impl Into<String>,
        log_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            state: WatchState::default(),
            session: None,
            target_kind,
            target: target.into(),
            log_dir,
            title: "nxr task".to_owned(),
            terminal: None,
            ratatui: None,
            last_draw: Instant::now() - Duration::from_secs(1),
            finished: false,
        }
    }

    fn ensure_session(&mut self, run_id: &str) -> io::Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let session = AttachSession::write_running(
            run_id,
            self.target_kind,
            &self.target,
            self.log_dir.as_deref(),
        )
        .map_err(io::Error::other)?;
        self.session = Some(session);
        Ok(())
    }

    fn ensure_terminal(&mut self) -> io::Result<()> {
        if self.terminal.is_some() {
            return Ok(());
        }
        let guard = TerminalGuard::enter()?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        self.terminal = Some(guard);
        self.ratatui = Some(terminal);
        Ok(())
    }

    fn poll_navigation(&mut self) -> io::Result<()> {
        while event::poll(Duration::from_millis(0))? {
            if let CrosstermEvent::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.state.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.state.move_selection(1),
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if self.finished {
                            return Err(io::Error::new(io::ErrorKind::Interrupted, "tui quit"));
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.finished {
                            return Err(io::Error::new(io::ErrorKind::Interrupted, "tui quit"));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn redraw(&mut self) -> io::Result<()> {
        if self.last_draw.elapsed() < Duration::from_millis(50) {
            return Ok(());
        }
        self.ensure_terminal()?;
        if let Some(terminal) = &mut self.ratatui {
            terminal.draw(|frame| draw_watch(frame, &self.state, &self.title))?;
        }
        if let Some(session) = &mut self.session {
            let _ = session.sync_from_watch(&self.state, self.finished, self.state.success);
        }
        self.last_draw = Instant::now();
        Ok(())
    }

    fn wait_for_quit(&mut self) -> io::Result<()> {
        loop {
            self.poll_navigation()?;
            self.redraw()?;
            if event::poll(Duration::from_millis(80))?
                && let CrosstermEvent::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && matches!(
                    key.code,
                    KeyCode::Char('q')
                        | KeyCode::Esc
                        | KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL)
                )
            {
                break;
            }
        }
        if let Some(session) = &mut self.session {
            let _ = session.sync_from_watch(&self.state, true, self.state.success);
        }
        Ok(())
    }
}

impl EventSink for TuiEventSink {
    fn emit(&mut self, event: Event) {
        if let Event::PlanCreated {
            run_id: Some(run_id),
            ..
        } = &event
            && let Err(error) = self.ensure_session(run_id)
        {
            eprintln!("nxr: attach sidecar write failed: {error}");
        }

        self.state.apply(&event);

        if let Err(error) = self.poll_navigation() {
            eprintln!("nxr: tui input failed: {error}");
        }
        if let Err(error) = self.redraw() {
            eprintln!("nxr: tui draw failed: {error}");
        }

        if matches!(event, Event::RunCompleted { .. }) {
            self.finished = true;
            if let Err(error) = self.wait_for_quit()
                && error.kind() != io::ErrorKind::Interrupted
            {
                eprintln!("nxr: tui watch failed: {error}");
            }
            self.terminal = None;
            self.ratatui = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_updates_state_without_terminal() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        super::super::session::set_test_attach_runs_dir(Some(temp.path().to_path_buf()));
        let mut sink = TuiEventSink::new(RunTargetKind::Task, "ci", None);
        sink.emit(Event::PlanCreated {
            root: "ci".to_owned(),
            roots: None,
            node_count: 1,
            run_id: Some("run-dead".to_owned()),
        });
        sink.emit(Event::node_queued("fmt"));
        assert_eq!(sink.state.root, "ci");
        assert!(sink.state.nodes.contains_key("fmt"));
        assert!(sink.session.is_some());
        super::super::session::set_test_attach_runs_dir(None);
    }
}
