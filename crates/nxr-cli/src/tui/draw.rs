//! Ratatui frame rendering for the DAG watch.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};

use super::state::{NodePhase, WatchState};

/// Draw the full watch surface into `frame`.
pub fn draw_watch(frame: &mut Frame<'_>, state: &WatchState, title: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, chunks[0], state, title);
    draw_body(frame, chunks[1], state);
    draw_footer(frame, chunks[2], state);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &WatchState, title: &str) {
    let run_id = state.run_id.as_deref().unwrap_or("-");
    let status = if state.run_complete {
        match state.success {
            Some(true) => "done (ok)",
            Some(false) => "done (failed)",
            None => "done",
        }
    } else {
        "running"
    };
    let counts = state.phase_counts();
    let follow = if state.auto_follow {
        "follow on"
    } else {
        "follow off"
    };
    let line1 = Line::from(vec![
        Span::styled(
            format!("root={}  ", state.root),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("run={run_id}  ")),
        Span::styled(
            status,
            if state.run_complete {
                match state.success {
                    Some(true) => Style::default().fg(Color::Green),
                    Some(false) => Style::default().fg(Color::Red),
                    None => Style::default(),
                }
            } else {
                Style::default().fg(Color::Cyan)
            },
        ),
    ]);
    let line2 = Line::from(format!(
        "● {}  ✓ {}  ✗ {}  ○ {}  ·  {follow}",
        counts.running, counts.ok, counts.failed, counts.queued
    ));
    let paragraph =
        Paragraph::new(vec![line1, line2]).block(Block::default().borders(Borders::ALL).title(
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        ));
    frame.render_widget(paragraph, area);
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, state: &WatchState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    draw_node_table(frame, chunks[0], state);
    draw_log_tail(frame, chunks[1], state);
}

fn draw_node_table(frame: &mut Frame<'_>, area: Rect, state: &WatchState) {
    let header = Row::new(vec!["", "task", "status", "duration"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row<'_>> = state
        .node_order
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let entry = state.nodes.get(node).expect("ordered node");
            let marker = if index == state.selected { ">" } else { " " };
            let duration = entry
                .display_duration_ms()
                .map(|ms| nxr_task::format_duration(std::time::Duration::from_millis(ms)))
                .unwrap_or_else(|| "-".to_owned());
            Row::new(vec![
                marker.to_owned(),
                node.clone(),
                format!("{} {}", entry.phase.glyph(), entry.phase.label()),
                duration,
            ])
            .style(phase_style(entry.phase))
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(14),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("nodes"));

    frame.render_widget(table, area);
}

fn draw_log_tail(frame: &mut Frame<'_>, area: Rect, state: &WatchState) {
    let selected = state.selected_node().unwrap_or("-");
    let (stdout, stderr) = state
        .nodes
        .get(selected)
        .map(|node| (node.stdout_tail.as_str(), node.stderr_tail.as_str()))
        .unwrap_or(("", ""));

    let max_lines = area.height.saturating_sub(2).max(1) as usize;
    let mut lines: Vec<Line<'_>> = Vec::new();
    if let Some(message) = &state.diagnostic {
        lines.push(Line::from(Span::styled(
            format!("diag: {message}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    if !stdout.is_empty() {
        lines.push(Line::from(Span::styled(
            "--- stdout ---",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.extend(
            visible_tail_lines(stdout, max_lines)
                .into_iter()
                .map(Line::from),
        );
    }
    if !stderr.is_empty() {
        lines.push(Line::from(Span::styled(
            "--- stderr ---",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        lines.extend(
            visible_tail_lines(stderr, max_lines)
                .into_iter()
                .map(|line| Line::from(vec![Span::styled(line, Style::default().fg(Color::Red))])),
        );
    }
    if lines.is_empty() {
        lines.push(Line::from("(no output yet)"));
    } else if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("log: {selected}"));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, state: &WatchState) {
    let hint = if state.run_complete {
        "q quit  ↑/↓ select  f follow  mouse select/copy ok"
    } else {
        "↑/↓ select  f follow running  q quit when done  mouse select/copy ok"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

fn visible_tail_lines(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if lines.len() <= max {
        lines
    } else {
        lines[lines.len() - max..].to_vec()
    }
}

fn phase_style(phase: NodePhase) -> Style {
    let color = match phase {
        NodePhase::Queued => Color::DarkGray,
        NodePhase::Running => Color::Cyan,
        NodePhase::Succeeded => Color::Green,
        NodePhase::Failed | NodePhase::TimedOut => Color::Red,
        NodePhase::Skipped | NodePhase::Cancelled => Color::Yellow,
    };
    Style::default().fg(color)
}
