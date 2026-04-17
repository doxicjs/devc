mod app;
mod commands;
mod config;
mod config_watcher;
mod id;
mod keys;
mod platform;
mod port_monitor;
mod process;
mod services;
mod status;
mod tools;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use config::Config;

const INSTALL_URL: &str = "https://raw.githubusercontent.com/doxicjs/devc/main/install.sh";

/// RAII guard: enables raw mode + alt screen + mouse capture on `enter`, and
/// restores the terminal on drop — including on panic — so users are never
/// stranded in an unusable terminal state.
struct RawTerminal;

impl RawTerminal {
    fn enter() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--update" || a == "-u") {
        println!("Updating devc...");

        let tmp_dir = std::env::temp_dir().join("devc-update");
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;
        let script_path = tmp_dir.join("install.sh");

        let download_status = std::process::Command::new("curl")
            .args(["-fsSL", INSTALL_URL, "-o"])
            .arg(&script_path)
            .status()?;

        if !download_status.success() {
            eprintln!("Failed to download update script");
            std::process::exit(1);
        }

        println!("Running update...");
        let status = std::process::Command::new("bash")
            .arg(&script_path)
            .status()?;

        let _ = std::fs::remove_dir_all(&tmp_dir);
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

    let local_path = local_config_path(&config_path);
    let config = Config::load(&config_path, local_path.as_deref())?;

    if config.services.is_empty() && config.commands.is_empty() {
        eprintln!("No services or commands defined in config");
        return Ok(());
    }

    let mut app = App::new(config, config_dir, config_path, local_path);

    // Handle SIGINT/SIGTERM so cleanup() runs before exit.
    // Uses libc directly (no extra deps) — the handler only touches an AtomicBool,
    // which is async-signal-safe.
    static RUNNING: AtomicBool = AtomicBool::new(true);

    extern "C" fn signal_handler(_: libc::c_int) {
        RUNNING.store(false, Ordering::SeqCst);
    }

    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal_handler as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }

    let result = {
        let _guard = RawTerminal::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        let r = run(&mut terminal, &mut app, &RUNNING);
        app.cleanup();
        r
        // _guard drops here — raw mode, alt-screen, and mouse capture are
        // all restored before we print anything else, even on panic.
    };

    if !app.conflicts.is_empty() {
        eprintln!();
        for warning in &app.conflicts {
            eprintln!("warning: {}", warning);
        }
    }

    result
}

fn local_config_path(main_path: &std::path::Path) -> Option<PathBuf> {
    let parent = main_path.parent()?;
    let file_name = main_path.file_name()?.to_str()?;
    let local_name = match main_path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let stem = main_path.file_stem()?.to_str()?;
            format!("{}.local.{}", stem, ext)
        }
        None => format!("{}.local", file_name),
    };
    Some(parent.join(local_name))
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    running: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        app.poll();

        terminal.draw(|f| ui::draw(f, app))?;

        // 100ms poll = ~10fps render + tick rate for spinners and port checks
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Tab => app.next_tab(),
                        KeyCode::BackTab => app.prev_tab(),
                        KeyCode::Up | KeyCode::Char('k') => app.select_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.select_down(),
                        KeyCode::Enter => app.activate_selected(),
                        KeyCode::Char(' ') => {
                            if app.tab == app::Tab::Services {
                                let idx = app.services.selected_idx();
                                match app.services.open_url(idx) {
                                    Ok(msg) | Err(msg) => app.status.set(msg),
                                }
                            }
                        }
                        KeyCode::PageUp => app.scroll_up(10),
                        KeyCode::PageDown => app.scroll_down(10),
                        KeyCode::Home => app.scroll_up(usize::MAX / 2),
                        KeyCode::End => app.scroll_to_bottom(),
                        KeyCode::Char(c) => app.handle_char(c),
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => app.scroll_up(1),
                    MouseEventKind::ScrollDown => app.scroll_down(1),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Ok(())
}
