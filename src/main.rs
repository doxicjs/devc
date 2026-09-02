mod app;
mod client;
mod commands;
mod config;
mod config_watcher;
mod control;
mod id;
mod keys;
mod platform;
mod port_monitor;
mod process;
mod protocol;
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
use protocol::RemoteKey;

const INSTALL_URL: &str = "https://raw.githubusercontent.com/doxicjs/devc/main/install.sh";

/// RAII guard: enables raw mode + alt screen + mouse capture on `enter`, and
/// restores the terminal on drop — including on panic — so users are never
/// stranded in an unusable terminal state.
pub struct RawTerminal;

impl RawTerminal {
    pub fn enter() -> Result<Self, Box<dyn std::error::Error>> {
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

    // Control subcommands run against an already-running devc and exit. Checked
    // before anything else so `devc status` can never be mistaken for a request
    // to open a config file named "status".
    if let Some(verb) = args.get(1) {
        if client::is_verb(verb) {
            std::process::exit(client::run(&args[1..]));
        }
        if verb == "--help" || verb == "-h" {
            print_help();
            return Ok(());
        }
    }

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

    // Claim the control socket before doing anything expensive. If another
    // devc already owns this project, become a second view of it rather than a
    // second supervisor — two supervisors would each spawn their own copy of
    // every service, which is exactly the problem this is here to prevent.
    let bind = control::ControlServer::bind(&config_path)?;
    let server = match bind {
        control::Bind::Bound(server) => server,
        control::Bind::AlreadyRunning { pid, .. } => {
            match pid {
                Some(pid) => eprintln!("devc is already running for this project (pid {}) — attaching", pid),
                None => eprintln!("devc is already running for this project — attaching"),
            }
            return client::attach(&config_path);
        }
    };

    let local_path = local_config_path(&config_path);
    let config = Config::load(&config_path, local_path.as_deref())?;

    if config.services.is_empty() && config.commands.is_empty() {
        eprintln!("No services or commands defined in config");
        return Ok(());
    }

    let mut app = App::new(config, config_dir, config_path, local_path);
    app.attach_control(server);

    // Handle SIGINT/SIGTERM so cleanup() runs before exit.
    // Uses libc directly (no extra deps) — the handler only touches an AtomicBool,
    // which is async-signal-safe.
    static RUNNING: AtomicBool = AtomicBool::new(true);

    extern "C" fn signal_handler(_: libc::c_int) {
        RUNNING.store(false, Ordering::SeqCst);
    }

    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal_handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        // Closing the terminal is a normal way for a TUI to end. Without this,
        // devc dies on the spot — leaving its services orphaned and its control
        // socket behind for the next run to clean up.
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
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

pub fn local_config_path(main_path: &std::path::Path) -> Option<PathBuf> {
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

        terminal.draw(|f| ui::draw(f, &app.snapshot))?;

        // 100ms poll = ~10fps render + tick rate for spinners and port checks
        if event::poll(Duration::from_millis(100))? {
            // Local keys go through the same handler as keys forwarded by an
            // attached client, so the two can't drift apart.
            let key = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Tab => RemoteKey::Tab,
                    KeyCode::BackTab => RemoteKey::BackTab,
                    KeyCode::Up => RemoteKey::Up,
                    KeyCode::Down => RemoteKey::Down,
                    KeyCode::Enter => RemoteKey::Enter,
                    KeyCode::Char(' ') => RemoteKey::Space,
                    KeyCode::PageUp => RemoteKey::PageUp,
                    KeyCode::PageDown => RemoteKey::PageDown,
                    KeyCode::Home => RemoteKey::Home,
                    KeyCode::End => RemoteKey::End,
                    KeyCode::Char(c) => RemoteKey::Char(c),
                    _ => continue,
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => RemoteKey::ScrollUp,
                    MouseEventKind::ScrollDown => RemoteKey::ScrollDown,
                    _ => continue,
                },
                _ => continue,
            };
            if !app.handle_key(key) {
                break;
            }
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
devc {version} — dev service control

  devc [CONFIG]            run the TUI (default config: ./devc.toml)
                           if a devc already owns this project, attach to it

Control a running devc (for scripts and agents):
  devc ls [--json]                     list configured services and commands
  devc status [NAME] [--json]          what's up, who owns it, on which pid
  devc start NAME [--wait]             start if not already up (idempotent)
  devc stop NAME [--no-wait]           stop, if devc started it
  devc restart NAME [--wait]           stop then start
  devc run NAME                        run a [[commands]] entry
  devc logs NAME [-n 100]              tail buffered output

Options:
  -c, --config PATH        config file to target (default ./devc.toml)
  -t, --timeout SECS       cap on --wait (default 30s; 10s for stop)
  -w, --wait               block until the service is actually serving
      --json               machine-readable output
  -v, --version            print version
  -u, --update             update devc in place

Exit codes: 0 ok (including 'already running')  1 usage/failure
            2 no such name  3 no devc running  4 refused
",
        version = env!("CARGO_PKG_VERSION")
    );
}
