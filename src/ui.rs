use ansi_to_tui::IntoText;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Tab};
use crate::commands::CommandStatus;
use crate::services::ServiceStatus;
use crate::tools::ToolKind;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Build a scrollable log panel. `scroll_offset` is lines from bottom (0 = auto-scroll).
fn render_log_panel(
    f: &mut Frame,
    logs: &std::collections::VecDeque<String>,
    title: String,
    scroll_offset: usize,
    area: Rect,
) {
    let log_lines: Vec<Line> = logs
        .iter()
        .flat_map(|l| {
            if l.starts_with("──") {
                vec![Line::from(Span::styled(
                    l.as_str(),
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                l.as_bytes()
                    .into_text()
                    .map(|t| t.lines)
                    .unwrap_or_else(|_| vec![Line::from(l.as_str())])
            }
        })
        .collect();

    let visible_height = area.height.saturating_sub(2) as usize;
    let total = log_lines.len();
    let max_scroll = total.saturating_sub(visible_height);
    let scroll = if scroll_offset == 0 {
        max_scroll
    } else {
        max_scroll.saturating_sub(scroll_offset)
    };

    let paragraph = Paragraph::new(log_lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    f.render_widget(paragraph, area);
}

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
        Tab::Commands => {
            draw_commands(f, app, main[0]);
            draw_command_logs(f, app, main[1]);
        }
        Tab::Tools => {
            draw_tools(f, app, main[0]);
            draw_tool_detail(f, app, main[1]);
        }
    }

    draw_help(f, app, outer[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let running = app.services.running_count();
    let total = app.services.len();

    let tabs = Tabs::new(vec!["Services", "Commands", "Tools"])
        .select(app.tab as usize)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");

    // Render tabs and status on the same line.
    // When a transient status flash is active, it takes the right slot;
    // otherwise the persistent N/M running count is shown.
    let right_text = if let Some(msg) = app.status.current() {
        format!(" {} ", msg)
    } else {
        format!(" {}/{} running ", running, total)
    };
    let right_color = if app.status.current().is_some() || running > 0 {
        Color::Green
    } else {
        Color::DarkGray
    };
    let right_width = (right_text.chars().count() as u16).max(16);

    let conflict_text = if app.conflicts.is_empty() {
        None
    } else {
        Some(format!(
            " ⚠ {} conflict{} ",
            app.conflicts.len(),
            if app.conflicts.len() == 1 { "" } else { "s" },
        ))
    };
    let conflict_width = conflict_text
        .as_ref()
        .map(|t| t.chars().count() as u16)
        .unwrap_or(0);

    let header_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(20),
            Constraint::Length(conflict_width),
            Constraint::Length(right_width),
        ])
        .split(area);

    f.render_widget(tabs, header_layout[0]);

    if let Some(text) = conflict_text {
        let badge = Paragraph::new(Line::from(vec![Span::styled(
            text,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]));
        f.render_widget(badge, header_layout[1]);
    }

    let status = Paragraph::new(Line::from(vec![Span::styled(
        right_text,
        Style::default().fg(right_color),
    )]));
    f.render_widget(status, header_layout[2]);
}

fn draw_services(f: &mut Frame, app: &App, area: Rect) {
    let spinner_frame = SPINNER[app.tick as usize % SPINNER.len()];

    let items: Vec<ListItem> = app
        .services
        .items()
        .iter()
        .enumerate()
        .map(|(i, service)| {
            let (status_icon, status_color) = match service.status {
                ServiceStatus::Running => ("●", Color::Green),
                ServiceStatus::Starting => (spinner_frame, Color::Yellow),
                ServiceStatus::Stopping => (spinner_frame, Color::Red),
                ServiceStatus::Stopped if service.port_active => ("◆", Color::Cyan),
                ServiceStatus::Stopped => ("○", Color::DarkGray),
            };

            let port_str = service
                .config
                .port
                .map(|p| format!(":{}", p))
                .unwrap_or_default();

            let mut spans = vec![
                Span::styled(
                    format!(" [{}] ", service.config.key_char().to_ascii_uppercase()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("{} ", status_icon), Style::default().fg(status_color)),
                Span::styled(
                    format!("{}{}", service.config.name, port_str),
                    Style::default().fg(Color::White),
                ),
            ];
            if service.orphan {
                spans.push(Span::styled(
                    " [removed]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            } else if service.config_dirty {
                spans.push(Span::styled(
                    " [reload]",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
            let line = Line::from(spans);

            let item = ListItem::new(line);
            if i == app.services.selected_idx() {
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
    let Some(service) = app.services.items().get(app.services.selected_idx()) else {
        let empty = Paragraph::new("No services configured").block(
            Block::default()
                .title(" Logs ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(empty, area);
        return;
    };
    render_log_panel(
        f,
        &service.logs,
        format!(" {} ", service.config.name),
        app.services.log_scroll_offset,
        area,
    );
}

fn draw_commands(f: &mut Frame, app: &App, area: Rect) {
    let spinner_frame = SPINNER[app.tick as usize % SPINNER.len()];

    let items: Vec<ListItem> = app
        .commands
        .items()
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let (status_icon, status_color) = match cmd.status {
                CommandStatus::Idle => ("○", Color::DarkGray),
                CommandStatus::Running => (spinner_frame, Color::Yellow),
                CommandStatus::Done => ("✓", Color::Green),
                CommandStatus::Failed => ("✗", Color::Red),
            };

            let mut spans = vec![
                Span::styled(
                    format!(" [{}] ", cmd.config.key_char().to_ascii_uppercase()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("{} ", status_icon), Style::default().fg(status_color)),
                Span::styled(cmd.config.name.clone(), Style::default().fg(Color::White)),
            ];
            if cmd.orphan {
                spans.push(Span::styled(
                    " [removed]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            } else if cmd.config_dirty {
                spans.push(Span::styled(
                    " [reload]",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
            let line = Line::from(spans);

            let item = ListItem::new(line);
            if i == app.commands.selected_idx() {
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
            .title(" Commands ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(list, area);
}

fn draw_command_logs(f: &mut Frame, app: &App, area: Rect) {
    let Some(cmd) = app.commands.items().get(app.commands.selected_idx()) else {
        let empty = Paragraph::new("No commands configured").block(
            Block::default()
                .title(" Output ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(empty, area);
        return;
    };
    render_log_panel(
        f,
        &cmd.logs,
        format!(" {} ", cmd.config.name),
        app.commands.log_scroll_offset,
        area,
    );
}

fn draw_tools(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .tools
        .items()
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
            if i == app.tools.selected_idx() {
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
    let Some(tool) = app.tools.items().get(app.tools.selected_idx()) else {
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

    let lines = vec![
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
                Span::raw(" stop all  "),
                Span::styled("PgUp/Dn", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll"),
            ]);
        }
        Tab::Commands => {
            spans.extend([
                Span::styled("[key]", Style::default().fg(Color::Yellow)),
                Span::raw(" run command  "),
                Span::styled("PgUp/Dn", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll"),
            ]);
        }
        Tab::Tools => {
            spans.extend([
                Span::styled("[key]", Style::default().fg(Color::Yellow)),
                Span::raw(" run tool"),
            ]);
        }
    }

    let version = format!(" devc v{} ", env!("CARGO_PKG_VERSION"));
    let version_width = version.chars().count() as u16;
    let row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(version_width)])
        .split(area);

    f.render_widget(Paragraph::new(Line::from(spans)), row[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            version,
            Style::default().fg(Color::DarkGray),
        ))),
        row[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn svc(name: &str, key: &str) -> ServiceConfig {
        ServiceConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
            port: None,
            url: None,
            depends_on: vec![],
        }
    }

    fn cmd(name: &str, key: &str) -> CommandConfig {
        CommandConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
        }
    }

    fn make_config(services: Vec<ServiceConfig>, commands: Vec<CommandConfig>) -> Config {
        Config {
            general: General { project_root: "./".to_string() },
            services,
            commands,
            links: vec![],
            copies: vec![],
        }
    }

    // ===== Issue #1: draw with empty services must NOT panic =====

    #[test]
    fn draw_with_empty_services_does_not_panic() {
        let config = make_config(vec![], vec![cmd("build", "b")]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        // Panics on current code: draw_logs does app.services[app.selected]
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_with_empty_everything_does_not_panic() {
        let config = make_config(vec![], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    // ===== Normal rendering =====

    #[test]
    fn draw_services_tab_with_services() {
        let config = make_config(vec![svc("web", "w"), svc("api", "a")], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_commands_tab_no_commands() {
        let config = make_config(vec![svc("web", "w")], vec![]);
        let mut app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        app.tab = Tab::Commands;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_commands_tab_with_commands() {
        let config = make_config(vec![], vec![cmd("build", "b"), cmd("test", "t")]);
        let mut app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        app.tab = Tab::Commands;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_tools_tab_empty() {
        let config = make_config(vec![], vec![]);
        let mut app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        app.tab = Tab::Tools;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_tools_tab_with_links_and_copies() {
        let config = Config {
            general: General { project_root: "./".to_string() },
            services: vec![],
            commands: vec![],
            links: vec![LinkConfig {
                name: "Docs".to_string(),
                key: "d".to_string(),
                url: "https://docs.example.com".to_string(),
            }],
            copies: vec![CopyConfig {
                name: "Token".to_string(),
                key: "t".to_string(),
                text: "abc123".to_string(),
            }],
        };
        let mut app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        app.tab = Tab::Tools;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    // ===== Edge cases =====

    #[test]
    fn draw_with_tiny_terminal() {
        let config = make_config(vec![svc("web", "w")], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        let backend = TestBackend::new(10, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn draw_with_single_row_terminal() {
        let config = make_config(vec![svc("web", "w")], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn header_shows_conflict_badge_when_conflicts_present() {
        // Two services bound to 'w' → duplicate-key conflict detected at construction.
        let config = make_config(vec![svc("A", "w"), svc("B", "w")], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        assert!(!app.conflicts.is_empty());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let header: String = (0..buf.area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or("").to_string())
            .collect();
        assert!(header.contains("⚠"), "header should show warning glyph, got: {:?}", header);
        assert!(header.contains("conflict"), "header should mention conflicts, got: {:?}", header);
    }

    #[test]
    fn header_hides_conflict_badge_when_clean() {
        let config = make_config(vec![svc("A", "a"), svc("B", "b")], vec![]);
        let app = App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None);
        assert!(app.conflicts.is_empty());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let header: String = (0..buf.area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or("").to_string())
            .collect();
        assert!(!header.contains("⚠"), "clean config must not show the badge, got: {:?}", header);
    }
}
