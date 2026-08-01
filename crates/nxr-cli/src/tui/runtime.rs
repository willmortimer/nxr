//! Terminal lifecycle and event loop for the DAG watch.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::draw::draw_watch;
use super::session::{AttachSession, hydrate_log_tails};
use super::state::WatchState;

/// Guard that restores the terminal on drop.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enter the alternate screen and raw mode.
    ///
    /// Mouse capture is intentionally **not** enabled so the host terminal can
    /// select and copy text (tmux/zellij scrollback / OSC 52 remain available).
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the terminal cannot be configured.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        Ok(Self { active: true })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

/// Replay loop for `nxr attach`.
///
/// # Errors
///
/// Returns [`io::Error`] on terminal or event I/O failures.
pub fn run_attach_replay(
    session: &AttachSession,
    title: &str,
    follow_running: bool,
) -> io::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut state = session.to_watch_state();
    hydrate_log_tails(session, &mut state).map_err(|error| io::Error::other(error.to_string()))?;

    let mut quit = false;
    while !quit {
        if follow_running {
            hydrate_log_tails(session, &mut state)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        poll_navigation(&mut state)?;
        terminal.draw(|frame| draw_watch(frame, &state, title))?;
        if user_requested_quit()? {
            quit = true;
        } else {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    Ok(())
}

fn poll_navigation(state: &mut WatchState) -> io::Result<()> {
    while event::poll(Duration::from_millis(0))? {
        if let CrosstermEvent::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
                KeyCode::Char('f') => state.enable_follow_running(),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn user_requested_quit() -> io::Result<bool> {
    if !event::poll(Duration::from_millis(0))? {
        return Ok(false);
    }
    if let CrosstermEvent::Key(key) = event::read()?
        && key.kind == KeyEventKind::Press
    {
        return Ok(matches!(
            key.code,
            KeyCode::Char('q')
                | KeyCode::Esc
                | KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL)
        ));
    }
    Ok(false)
}
