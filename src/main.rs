mod app;
mod config;
mod process;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use config::Config;

const INSTALL_URL: &str = "https://raw.githubusercontent.com/doxicjs/devc/main/install.sh";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--update" || a == "-u") {
        println!("Updating devc...");
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("curl -fsSL {} | bash", INSTALL_URL))
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }

    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("devc {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let config_path = args
        .get(1)
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "devc.toml".to_string());

    let config_path = PathBuf::from(&config_path)
        .canonicalize()
        .map_err(|e| format!("Config file '{}': {}", config_path, e))?;

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    let config_str = std::fs::read_to_string(&config_path)?;
    let config: Config =
        toml::from_str(&config_str).map_err(|e| format!("Failed to parse config: {}", e))?;

    if config.services.is_empty() && config.commands.is_empty() {
        eprintln!("No services or commands defined in config");
        return Ok(());
    }

    let mut app = App::new(config, config_dir);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app);

    app.cleanup();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        app.tick();
        app.poll_logs();
        app.check_processes();
        app.check_ports();
        app.clear_old_status();

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab | KeyCode::BackTab => app.next_tab(),
                    KeyCode::Up | KeyCode::Char('k') => app.select_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.select_down(),
                    KeyCode::Enter => app.activate_selected(),
                    KeyCode::Char(' ') => {
                        if app.tab == app::Tab::Services {
                            let idx = app.selected;
                            app.open_service_url(idx);
                        }
                    }
                    KeyCode::Char(c) => app.handle_char(c),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
