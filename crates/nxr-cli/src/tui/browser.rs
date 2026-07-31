//! Lazygit-style browser for apps, tasks, and workspace scripts.
//!
//! Focus starts on the **Tabs** bar. Enter opens the catalog for that tab;
//! Enter in the catalog runs the selection. Mouse capture is left off so the
//! terminal can select/copy text.

use std::io;

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers};
use nxr_core::sanitize::sanitize_terminal_text;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};

use super::runtime::TerminalGuard;

/// One row in the browser list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserItem {
    pub name: String,
    pub detail: Option<String>,
}

/// Active catalog tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTab {
    Apps,
    Tasks,
    Scripts,
}

impl BrowserTab {
    const ALL: [Self; 3] = [Self::Apps, Self::Tasks, Self::Scripts];

    const fn index(self) -> usize {
        match self {
            Self::Apps => 0,
            Self::Tasks => 1,
            Self::Scripts => 2,
        }
    }

    const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Tasks,
            2 => Self::Scripts,
            _ => Self::Apps,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Apps => "Apps",
            Self::Tasks => "Tasks",
            Self::Scripts => "Scripts",
        }
    }
}

/// Which pane receives navigation keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserFocus {
    Tabs,
    Catalog,
}

/// Outcome of the interactive browser loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserOutcome {
    Quit,
    Launch(BrowserLaunch),
}

/// Selected entry to run after the browser closes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserLaunch {
    App(String),
    Task(String),
    Script(String),
}

/// Mutable browser view state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserState {
    pub apps: Vec<BrowserItem>,
    pub tasks: Vec<BrowserItem>,
    pub scripts: Vec<BrowserItem>,
    pub tab: BrowserTab,
    pub selected: usize,
    pub focus: BrowserFocus,
}

impl BrowserState {
    /// Build browser rows from discovered catalog entries.
    #[must_use]
    pub fn from_catalog(
        apps: impl IntoIterator<Item = (String, Option<String>)>,
        tasks: impl IntoIterator<Item = (String, Option<String>)>,
        scripts: impl IntoIterator<Item = (String, Option<String>)>,
    ) -> Self {
        Self {
            apps: sanitize_items(apps),
            tasks: sanitize_items(tasks),
            scripts: sanitize_items(scripts),
            tab: BrowserTab::Apps,
            selected: 0,
            focus: BrowserFocus::Tabs,
        }
    }

    fn items_for_tab(&self) -> &[BrowserItem] {
        match self.tab {
            BrowserTab::Apps => &self.apps,
            BrowserTab::Tasks => &self.tasks,
            BrowserTab::Scripts => &self.scripts,
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.items_for_tab().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    /// Move the highlight within the active tab.
    pub fn move_selection(&mut self, delta: i32) {
        let len = self.items_for_tab().len();
        if len == 0 {
            return;
        }
        let next = self.selected as i32 + delta;
        let bounded = next.clamp(0, len as i32 - 1);
        self.selected = bounded as usize;
    }

    /// Switch to another tab, preserving selection when possible.
    pub fn set_tab(&mut self, tab: BrowserTab) {
        self.tab = tab;
        self.clamp_selection();
    }

    fn next_tab(&mut self, delta: isize) {
        let current = self.tab.index() as isize;
        let next = (current + delta).rem_euclid(BrowserTab::ALL.len() as isize);
        self.set_tab(BrowserTab::from_index(next as usize));
    }

    /// Enter the catalog for the current tab (does not launch).
    pub fn enter_catalog(&mut self) {
        self.focus = BrowserFocus::Catalog;
        self.clamp_selection();
    }

    fn launch_selection(&self) -> Option<BrowserLaunch> {
        let item = self.items_for_tab().get(self.selected)?;
        Some(match self.tab {
            BrowserTab::Apps => BrowserLaunch::App(item.name.clone()),
            BrowserTab::Tasks => BrowserLaunch::Task(item.name.clone()),
            BrowserTab::Scripts => BrowserLaunch::Script(item.name.clone()),
        })
    }
}

fn sanitize_items(items: impl IntoIterator<Item = (String, Option<String>)>) -> Vec<BrowserItem> {
    items
        .into_iter()
        .map(|(name, detail)| BrowserItem {
            name: sanitize_terminal_text(&name),
            detail: detail.map(|text| sanitize_terminal_text(&text)),
        })
        .collect()
}

/// Run the browser until the user quits or launches a selection.
///
/// # Errors
///
/// Returns [`io::Error`] when the terminal cannot be configured or read.
pub fn run_browser(mut state: BrowserState) -> io::Result<BrowserOutcome> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    let mut list_state = ListState::default();

    loop {
        state.clamp_selection();
        list_state.select(if state.items_for_tab().is_empty() {
            None
        } else {
            Some(state.selected)
        });

        terminal.draw(|frame| draw_browser(frame, &state, &mut list_state))?;

        if let Some(outcome) = poll_browser_input(&mut state)? {
            return Ok(outcome);
        }
    }
}

fn poll_browser_input(state: &mut BrowserState) -> io::Result<Option<BrowserOutcome>> {
    if !event::poll(std::time::Duration::from_millis(120))? {
        return Ok(None);
    }

    let CrosstermEvent::Key(key) = event::read()? else {
        return Ok(None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }

    match key.code {
        KeyCode::Char('q') => return Ok(Some(BrowserOutcome::Quit)),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Ok(Some(BrowserOutcome::Quit));
        }
        KeyCode::Esc => match state.focus {
            BrowserFocus::Catalog => state.focus = BrowserFocus::Tabs,
            BrowserFocus::Tabs => return Ok(Some(BrowserOutcome::Quit)),
        },
        KeyCode::Char('1') => {
            state.set_tab(BrowserTab::Apps);
            state.focus = BrowserFocus::Tabs;
        }
        KeyCode::Char('2') => {
            state.set_tab(BrowserTab::Tasks);
            state.focus = BrowserFocus::Tabs;
        }
        KeyCode::Char('3') => {
            state.set_tab(BrowserTab::Scripts);
            state.focus = BrowserFocus::Tabs;
        }
        KeyCode::Tab => match state.focus {
            BrowserFocus::Tabs => state.enter_catalog(),
            BrowserFocus::Catalog => state.focus = BrowserFocus::Tabs,
        },
        KeyCode::BackTab => {
            state.focus = BrowserFocus::Tabs;
            state.next_tab(-1);
        }
        other => match state.focus {
            BrowserFocus::Tabs => match other {
                KeyCode::Left | KeyCode::Char('h') => state.next_tab(-1),
                KeyCode::Right | KeyCode::Char('l') => state.next_tab(1),
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter => state.enter_catalog(),
                _ => {}
            },
            BrowserFocus::Catalog => match other {
                KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
                KeyCode::Left | KeyCode::Char('h') => state.focus = BrowserFocus::Tabs,
                KeyCode::Enter => {
                    if let Some(launch) = state.launch_selection() {
                        return Ok(Some(BrowserOutcome::Launch(launch)));
                    }
                }
                _ => {}
            },
        },
    }
    Ok(None)
}

fn draw_browser(frame: &mut Frame<'_>, state: &BrowserState, list_state: &mut ListState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_tabs(frame, chunks[0], state);
    draw_list(frame, chunks[1], state, list_state);
    draw_footer(frame, chunks[2], state);
}

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, state: &BrowserState) {
    let titles = BrowserTab::ALL
        .map(|tab| {
            let count = match tab {
                BrowserTab::Apps => state.apps.len(),
                BrowserTab::Tasks => state.tasks.len(),
                BrowserTab::Scripts => state.scripts.len(),
            };
            Line::from(format!("{} ({count})", tab.label()))
        })
        .to_vec();
    let focused = matches!(state.focus, BrowserFocus::Tabs);
    let title = if focused { "nxr ui · tabs" } else { "nxr ui" };
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            title,
            if focused {
                Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            },
        )))
        .select(state.tab.index())
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED));
    frame.render_widget(tabs, area);
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, state: &BrowserState, list_state: &mut ListState) {
    let focused = matches!(state.focus, BrowserFocus::Catalog);
    let title = if focused {
        format!("{} · catalog", state.tab.label())
    } else {
        state.tab.label().to_owned()
    };
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        if focused {
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default()
        },
    ));

    let items = state.items_for_tab();
    if items.is_empty() {
        let empty = Paragraph::new(format!(
            "No {} discovered.",
            state.tab.label().to_lowercase()
        ))
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let rows: Vec<ListItem<'_>> = items
        .iter()
        .map(|item| {
            let line = match &item.detail {
                Some(detail) => Line::from(vec![
                    Span::raw(&item.name),
                    Span::raw("  "),
                    Span::styled(detail.clone(), Style::default().add_modifier(Modifier::DIM)),
                ]),
                None => Line::from(item.name.clone()),
            };
            ListItem::new(line)
        })
        .collect();

    let highlight = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let list = List::new(rows)
        .block(block)
        .highlight_style(highlight)
        .highlight_symbol(if focused { "> " } else { "  " });
    frame.render_stateful_widget(list, area, list_state);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, state: &BrowserState) {
    let hint = match state.focus {
        BrowserFocus::Tabs => {
            "←/→ tab · Enter/↓ open catalog · 1/2/3 jump · q quit · mouse select/copy ok"
        }
        BrowserFocus::Catalog if state.items_for_tab().is_empty() => {
            "Esc/← back to tabs · q quit · mouse select/copy ok"
        }
        BrowserFocus::Catalog => {
            "↑/↓ navigate · Enter run · Esc/← tabs · q quit · mouse select/copy ok"
        }
    };
    let footer = Paragraph::new(hint);
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_selection_stays_within_bounds() {
        let mut state = BrowserState::from_catalog(
            [("a".to_owned(), None), ("b".to_owned(), None)],
            std::iter::empty::<(String, Option<String>)>(),
            std::iter::empty(),
        );
        state.enter_catalog();
        state.move_selection(5);
        assert_eq!(state.selected, 1);
        state.move_selection(-10);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn tab_switch_clamps_selection() {
        let mut state = BrowserState::from_catalog(
            [("only".to_owned(), None)],
            [("t1".to_owned(), None), ("t2".to_owned(), None)],
            std::iter::empty(),
        );
        state.selected = 0;
        state.set_tab(BrowserTab::Tasks);
        assert_eq!(state.selected, 0);
        state.selected = 5;
        state.set_tab(BrowserTab::Apps);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn enter_catalog_does_not_launch() {
        let mut state = BrowserState::from_catalog(
            [("app".to_owned(), None)],
            std::iter::empty::<(String, Option<String>)>(),
            std::iter::empty(),
        );
        assert_eq!(state.focus, BrowserFocus::Tabs);
        state.enter_catalog();
        assert_eq!(state.focus, BrowserFocus::Catalog);
    }

    #[test]
    fn launch_selection_maps_tab_to_target() {
        let state = BrowserState::from_catalog(
            [("app".to_owned(), None)],
            [("task".to_owned(), None)],
            [("script".to_owned(), None)],
        );
        assert_eq!(
            state.launch_selection(),
            Some(BrowserLaunch::App("app".to_owned()))
        );

        let mut tasks = state.clone();
        tasks.set_tab(BrowserTab::Tasks);
        assert_eq!(
            tasks.launch_selection(),
            Some(BrowserLaunch::Task("task".to_owned()))
        );

        let mut scripts = state;
        scripts.set_tab(BrowserTab::Scripts);
        assert_eq!(
            scripts.launch_selection(),
            Some(BrowserLaunch::Script("script".to_owned()))
        );
    }

    #[test]
    fn sanitize_strips_control_sequences_from_labels() {
        let state = BrowserState::from_catalog(
            [(
                "\u{1b}[31mapp\u{1b}[0m".to_owned(),
                Some("desc\u{7}".to_owned()),
            )],
            std::iter::empty::<(String, Option<String>)>(),
            std::iter::empty(),
        );
        assert_eq!(state.apps[0].name, "app");
        assert_eq!(state.apps[0].detail.as_deref(), Some("desc"));
    }
}
