use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, ServiceStatus, Tab, ToolKind};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, app, outer[0]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(0)])
        .split(outer[1]);

    match app.tab {
        Tab::Services => {
            draw_services(f, app, main[0]);
            draw_logs(f, app, main[1]);
        }
        Tab::Tools => {
            draw_tools(f, app, main[0]);
            draw_tool_detail(f, app, main[1]);
        }
    }

    draw_help(f, app, outer[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let running = app.running_count();
    let total = app.services.len();

    let tabs = Tabs::new(vec!["Services", "Tools"])
        .select(app.tab as usize)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");

    // Render tabs and status on the same line
    let header_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(16)])
        .split(area);

    f.render_widget(tabs, header_layout[0]);

    let status = Paragraph::new(Line::from(vec![Span::styled(
        format!(" {}/{} running ", running, total),
        Style::default().fg(if running > 0 {
            Color::Green
        } else {
            Color::DarkGray
        }),
    )]));
    f.render_widget(status, header_layout[1]);
}

fn draw_services(f: &mut Frame, app: &App, area: Rect) {
    let spinner_frame = SPINNER[app.tick as usize % SPINNER.len()];

    let items: Vec<ListItem> = app
        .services
        .iter()
        .enumerate()
        .map(|(i, service)| {
            let (status_icon, status_color) = match service.status {
                ServiceStatus::Running => ("●", Color::Green),
                ServiceStatus::Starting => (spinner_frame, Color::Yellow),
                ServiceStatus::Stopping => (spinner_frame, Color::Red),
                ServiceStatus::Stopped => ("○", Color::DarkGray),
            };

            let port_str = service
                .config
                .port
                .map(|p| format!(":{}", p))
                .unwrap_or_default();

            let line = Line::from(vec![
                Span::styled(
                    format!(" [{}] ", service.config.key_char().to_ascii_uppercase()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("{} ", status_icon), Style::default().fg(status_color)),
                Span::styled(
                    format!("{}{}", service.config.name, port_str),
                    Style::default().fg(Color::White),
                ),
            ]);

            let item = ListItem::new(line);
            if i == app.selected {
                item.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Services ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(list, area);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let service = &app.services[app.selected];
    let title = format!(" {} ", service.config.name);

    let log_lines: Vec<Line> = service
        .logs
        .iter()
        .map(|l| {
            if l.starts_with("──") {
                Line::from(Span::styled(
                    l.as_str(),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(l.as_str())
            }
        })
        .collect();

    let visible_height = area.height.saturating_sub(2) as usize;
    let scroll = log_lines.len().saturating_sub(visible_height) as u16;

    let paragraph = Paragraph::new(log_lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(paragraph, area);
}

fn draw_tools(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .tools
        .iter()
        .enumerate()
        .map(|(i, tool)| {
            let kind_icon = match &tool.kind {
                ToolKind::Link(_) => "->",
                ToolKind::Copy(_) => "cp",
            };
            let kind_color = match &tool.kind {
                ToolKind::Link(_) => Color::Blue,
                ToolKind::Copy(_) => Color::Magenta,
            };

            let line = Line::from(vec![
                Span::styled(
                    format!(" [{}] ", tool.key.to_ascii_uppercase()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("{} ", kind_icon), Style::default().fg(kind_color)),
                Span::styled(&tool.name, Style::default().fg(Color::White)),
            ]);

            let item = ListItem::new(line);
            if i == app.tools_selected {
                item.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Tools ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(list, area);
}

fn draw_tool_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(tool) = app.tools.get(app.tools_selected) else {
        let empty = Paragraph::new("No tools configured").block(
            Block::default()
                .title(" Detail ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(empty, area);
        return;
    };

    let (label, value) = match &tool.kind {
        ToolKind::Link(url) => ("URL", url.as_str()),
        ToolKind::Copy(text) => ("Text", text.as_str()),
    };

    let mut lines = vec![
        Line::from(Span::styled(
            &tool.name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("{}: ", label), Style::default().fg(Color::DarkGray)),
            Span::raw(value),
        ]),
    ];

    if let Some((msg, _)) = &app.status {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            msg.as_str(),
            Style::default().fg(Color::Green),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Detail ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(" q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" switch  "),
        Span::styled("↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" navigate  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" activate  "),
    ];

    match app.tab {
        Tab::Services => {
            spans.extend([
                Span::styled("Space", Style::default().fg(Color::Yellow)),
                Span::raw(" open  "),
                Span::styled("a", Style::default().fg(Color::Yellow)),
                Span::raw(" start all  "),
                Span::styled("x", Style::default().fg(Color::Yellow)),
                Span::raw(" stop all"),
            ]);
        }
        Tab::Tools => {
            spans.extend([
                Span::styled("[key]", Style::default().fg(Color::Yellow)),
                Span::raw(" run tool"),
            ]);
        }
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
