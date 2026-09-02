use super::app::{App, Content, DiffCell, RowKind, UnifiedLine, ViewMode};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

const BACKGROUND: Color = Color::Rgb(30, 30, 30);
const PANEL: Color = Color::Rgb(37, 37, 38);
const BORDER: Color = Color::Rgb(62, 62, 66);
const FOREGROUND: Color = Color::Rgb(212, 212, 212);
const MUTED: Color = Color::Rgb(128, 128, 128);
const BLUE: Color = Color::Rgb(0, 122, 204);
const GREEN: Color = Color::Rgb(115, 201, 145);
const RED: Color = Color::Rgb(244, 71, 71);
const YELLOW: Color = Color::Rgb(204, 167, 0);

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND)),
        frame.area(),
    );
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());
    render_title(frame, app, outer[0]);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.sidebar_percent),
            Constraint::Percentage(100 - app.sidebar_percent),
        ])
        .split(outer[1]);
    render_explorer(frame, app, body[0]);
    render_editor(frame, app, body[1]);
    render_footer(frame, app, outer[2]);
}

fn render_title(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " artifact-diff ",
            Style::default()
                .fg(Color::White)
                .bg(BLUE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}  →  {}", app.manifest.old.name, app.manifest.new.name),
            Style::default().fg(FOREGROUND),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().bg(PANEL)),
        area,
    );
}

fn render_explorer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(3),
        ])
        .split(area);

    let stats = &app.manifest.stats;
    let status = Line::from(vec![
        filter_span(
            app,
            0,
            format!("1 M:{} R:{}", stats.modified, stats.renamed),
            YELLOW,
        ),
        Span::raw("  "),
        filter_span(app, 1, format!("2 A:{}", stats.added), GREEN),
        Span::raw("  "),
        filter_span(app, 2, format!("3 D:{}", stats.deleted), RED),
        Span::raw("  "),
        filter_span(app, 3, format!("4 U:{}", stats.unchanged), MUTED),
    ]);
    frame.render_widget(
        Paragraph::new(status)
            .block(panel(" FILTERS "))
            .wrap(Wrap { trim: true }),
        sections[0],
    );

    let search_style = if app.searching {
        Style::default().fg(Color::White).bg(Color::Rgb(60, 60, 60))
    } else {
        Style::default().fg(MUTED)
    };
    let query = if app.query.is_empty() && !app.searching {
        "/ search paths".to_owned()
    } else {
        format!("/ {}{}", app.query, if app.searching { "▏" } else { "" })
    };
    frame.render_widget(Paragraph::new(query).style(search_style), sections[1]);

    let nodes = app.visible_nodes();
    let lines: Vec<Line<'static>> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let selected = index == app.selected;
            let mut spans = vec![Span::raw("  ".repeat(node.depth))];
            if node.directory {
                spans.push(Span::styled(
                    if node.expanded { "▾ " } else { "▸ " },
                    Style::default().fg(MUTED),
                ));
                spans.push(Span::styled(
                    node.label.clone(),
                    Style::default().fg(Color::Rgb(220, 220, 170)),
                ));
            } else {
                spans.push(Span::styled("  ", Style::default().fg(MUTED)));
                spans.push(Span::styled(
                    node.label.clone(),
                    Style::default().fg(FOREGROUND),
                ));
                if let Some(entry) = node.entry.and_then(|entry| app.manifest.entries.get(entry)) {
                    spans.push(Span::styled(
                        format!("  {}", status_letter(&entry.status)),
                        Style::default()
                            .fg(status_color(&entry.status))
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }
            let style = if selected {
                Style::default().bg(Color::Rgb(4, 57, 94))
            } else {
                Style::default()
            };
            Line::from(spans).style(style)
        })
        .collect();
    let height = sections[2].height.saturating_sub(2) as usize;
    let scroll = app.tree_scroll(height) as u16;
    let explorer = Paragraph::new(if lines.is_empty() {
        vec![Line::styled(
            "No matching entries",
            Style::default().fg(MUTED),
        )]
    } else {
        lines
    })
    .block(panel(" EXPLORER "))
    .scroll((scroll, 0));
    frame.render_widget(explorer, sections[2]);
}

fn render_editor(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);
    let selected = app.selected_entry();
    let mode = match app.mode {
        ViewMode::SideBySide => "SIDE BY SIDE",
        ViewMode::Unified => "UNIFIED",
    };
    let metadata = selected.map_or_else(
        || vec![Line::raw("No file selected")],
        |entry| {
            let old_path = entry.old_path.as_deref().unwrap_or("—");
            let new_path = entry.new_path.as_deref().unwrap_or("—");
            let path = if entry.renamed {
                format!(" {old_path}  →  {new_path}")
            } else {
                format!(" {}", entry.path)
            };
            vec![
                Line::raw(format!(
                    "{path}   {} / {}   {mode}",
                    entry.status.to_uppercase(),
                    entry.kind
                )),
                Line::styled(
                    format!(
                        " before: {}   →   after: {}",
                        format_size(entry.old_size),
                        format_size(entry.new_size)
                    ),
                    Style::default().fg(MUTED),
                ),
            ]
        },
    );
    frame.render_widget(
        Paragraph::new(metadata)
            .style(Style::default().fg(FOREGROUND).bg(PANEL))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(border_style()),
            ),
        parts[0],
    );

    match &app.content {
        Content::SideBySide(rows) => render_side_by_side(frame, app, rows, parts[1]),
        Content::Unified(lines) => render_unified(frame, app, lines, parts[1]),
        Content::Message(message) => frame.render_widget(
            Paragraph::new(message.as_str())
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false })
                .block(panel(" DIFF ")),
            parts[1],
        ),
    }
}

fn render_side_by_side(frame: &mut Frame<'_>, app: &App, rows: &[super::app::DiffRow], area: Rect) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.diff_percent),
            Constraint::Percentage(100 - app.diff_percent),
        ])
        .split(area);
    let old_lines = rows
        .iter()
        .map(|row| cell_line(&row.old))
        .collect::<Vec<_>>();
    let new_lines = rows
        .iter()
        .map(|row| cell_line(&row.new))
        .collect::<Vec<_>>();
    let scroll = (app.vertical_scroll, app.horizontal_scroll);
    frame.render_widget(
        Paragraph::new(old_lines)
            .block(panel(format!(" {} ", app.manifest.old.name)))
            .scroll(scroll),
        panes[0],
    );
    frame.render_widget(
        Paragraph::new(new_lines)
            .block(panel(format!(" {} ", app.manifest.new.name)))
            .scroll(scroll),
        panes[1],
    );
}

fn render_unified(frame: &mut Frame<'_>, app: &App, lines: &[UnifiedLine], area: Rect) {
    let lines = lines
        .iter()
        .map(|line| Line::styled(line.text.clone(), row_style(line.kind)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(" UNIFIED DIFF "))
            .scroll((app.vertical_scroll, app.horizontal_scroll)),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let default_text = if app.analyzing {
        " JADX is analyzing the selected JAR… "
    } else if app.has_parent() {
        " Backspace parent  q quit  / search  wheel/PgUp/PgDn/J/K scroll  Tab view  1–4 filters "
    } else {
        " q quit  / search  Enter JAR diff  Space folder  wheel/PgUp/PgDn/J/K scroll  Tab view  1–4 filters "
    };
    let text = app.error.as_deref().unwrap_or(default_text);
    let style = if app.error.is_some() {
        Style::default().fg(Color::White).bg(RED)
    } else {
        Style::default().fg(FOREGROUND).bg(BLUE)
    };
    frame.render_widget(Paragraph::new(text).style(style), area);
}

fn cell_line(cell: &DiffCell) -> Line<'static> {
    let number = cell
        .number
        .map_or_else(|| "     ".to_owned(), |number| format!("{number:>4} "));
    Line::from(vec![
        Span::styled(number, Style::default().fg(MUTED)),
        Span::styled(cell.text.clone(), row_style(cell.kind)),
    ])
    .style(row_background(cell.kind))
}

fn row_style(kind: RowKind) -> Style {
    match kind {
        RowKind::Added => Style::default().fg(GREEN),
        RowKind::Deleted => Style::default().fg(RED),
        RowKind::Modified => Style::default().fg(Color::Rgb(230, 230, 170)),
        RowKind::Header => Style::default().fg(Color::Rgb(86, 156, 214)),
        RowKind::Hunk => Style::default().fg(Color::Rgb(197, 134, 192)),
        RowKind::Equal => Style::default().fg(FOREGROUND),
    }
}

fn row_background(kind: RowKind) -> Style {
    match kind {
        RowKind::Added => Style::default().bg(Color::Rgb(22, 52, 32)),
        RowKind::Deleted => Style::default().bg(Color::Rgb(66, 28, 28)),
        RowKind::Modified => Style::default().bg(Color::Rgb(64, 58, 30)),
        _ => Style::default(),
    }
}

fn filter_span(app: &App, index: usize, text: String, color: Color) -> Span<'static> {
    let style = if app.filter_enabled(index) {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(MUTED)
            .add_modifier(Modifier::CROSSED_OUT)
    };
    Span::styled(text, style)
}

fn panel<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style())
        .title(title)
        .style(Style::default().fg(FOREGROUND).bg(BACKGROUND))
}

fn border_style() -> Style {
    Style::default().fg(BORDER)
}

fn status_letter(status: &str) -> &str {
    match status {
        "modified" => "M",
        "added" => "A",
        "deleted" => "D",
        "unchanged" => "U",
        "renamed" => "R",
        _ => "?",
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "modified" => YELLOW,
        "added" => GREEN,
        "deleted" => RED,
        "renamed" => Color::Rgb(86, 156, 214),
        _ => MUTED,
    }
}

fn format_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return "—".to_owned();
    };
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
