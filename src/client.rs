//! The other half of the binary: `devc status|start|stop|...` talking to a
//! running devc over its control socket, plus the attached-TUI mode a second
//! `devc` in the same project drops into.
//!
//! Exit codes are the contract for scripts and agents:
//!   0 success — *including* "already running", which is the desired end state
//!   1 usage or config error, or the action failed
//!   2 no such service/command
//!   3 no devc running for this config
//!   4 refused (e.g. stopping something devc didn't start)

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::protocol::{
    self as proto, socket_path, CommandStatusKind, Op, OwnerKind, Request, Response, Snapshot,
    StatusKind,
};

pub const EXIT_USAGE: i32 = 1;
pub const EXIT_NO_INSTANCE: i32 = 3;

/// Subcommands that switch devc from "run the TUI" to "drive a running one".
/// Checked before the positional config-path argument, so a config file
/// literally named `status` needs `devc --config status`.
pub const VERBS: &[&str] = &["status", "start", "stop", "restart", "run", "logs", "ls"];

pub fn is_verb(arg: &str) -> bool {
    VERBS.contains(&arg)
}

// ===== Connection =====

pub struct Connection {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Connection {
    pub fn open(canonical_config: &Path) -> Result<Self, String> {
        let sock = socket_path(canonical_config);
        let stream = UnixStream::connect(&sock).map_err(|_| {
            format!(
                "no devc running for {}\n  start one with: devc {}",
                canonical_config.display(),
                canonical_config.display()
            )
        })?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        Ok(Self { stream, reader })
    }

    /// Cap how long we'll wait for a reply. Without this, a socket left by a
    /// crashed devc that transiently accepts a connection would hang the CLI
    /// forever instead of failing fast.
    pub fn set_read_timeout(&mut self, timeout: std::time::Duration) {
        let _ = self.stream.set_read_timeout(Some(timeout));
    }

    pub fn send(&mut self, op: Op) -> Result<Response, String> {
        let mut line = serde_json::to_string(&Request::new(op)).map_err(|e| e.to_string())?;
        line.push('\n');
        self.stream
            .write_all(line.as_bytes())
            .map_err(|e| format!("devc closed the connection: {}", e))?;
        self.stream.flush().map_err(|e| e.to_string())?;
        self.recv()
    }

    pub fn recv(&mut self) -> Result<Response, String> {
        let mut buf = String::new();
        let n = self
            .reader
            .read_line(&mut buf)
            .map_err(|e| format!("failed reading reply: {}", e))?;
        if n == 0 {
            return Err("devc closed the connection".to_string());
        }
        serde_json::from_str(&buf).map_err(|e| format!("unreadable reply: {}", e))
    }
}

// ===== CLI =====

struct Args {
    verb: String,
    name: Option<String>,
    config: PathBuf,
    json: bool,
    wait: bool,
    no_wait: bool,
    timeout_ms: u64,
    lines: usize,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        verb: argv[0].clone(),
        name: None,
        config: PathBuf::from("devc.toml"),
        json: false,
        wait: false,
        no_wait: false,
        timeout_ms: 30_000,
        lines: 100,
    };
    let mut explicit_timeout = false;

    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--json" => args.json = true,
            "--wait" | "-w" => args.wait = true,
            "--no-wait" => args.no_wait = true,
            "--config" | "-c" => {
                i += 1;
                let v = argv.get(i).ok_or("--config needs a path")?;
                args.config = PathBuf::from(v);
            }
            "--timeout" | "-t" => {
                i += 1;
                let v = argv.get(i).ok_or("--timeout needs a value in seconds")?;
                let secs: f64 = v.parse().map_err(|_| format!("bad --timeout: {}", v))?;
                if secs <= 0.0 {
                    return Err("--timeout must be positive".to_string());
                }
                args.timeout_ms = (secs * 1000.0) as u64;
                explicit_timeout = true;
            }
            "-n" | "--lines" => {
                i += 1;
                let v = argv.get(i).ok_or("-n needs a count")?;
                args.lines = v.parse().map_err(|_| format!("bad -n: {}", v))?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag '{}'", other));
            }
            other => {
                if args.name.is_some() {
                    return Err(format!("unexpected argument '{}'", other));
                }
                args.name = Some(other.to_string());
            }
        }
        i += 1;
    }

    // Stopping is fast and bounded, so waiting is the useful default; a shorter
    // default timeout keeps a wedged process from hanging a script for 30s.
    if args.verb == "stop" && !explicit_timeout {
        args.timeout_ms = 10_000;
    }

    Ok(args)
}

/// Runs a control subcommand. Returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("devc: {}", e);
            return EXIT_USAGE;
        }
    };

    let needs_name = matches!(args.verb.as_str(), "start" | "stop" | "restart" | "run" | "logs");
    if needs_name && args.name.is_none() {
        eprintln!("devc {}: needs a name — try `devc ls` to see them", args.verb);
        return EXIT_USAGE;
    }

    let config = match args.config.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("devc: config '{}': {}", args.config.display(), e);
            return EXIT_USAGE;
        }
    };

    // `ls` reads the config directly — it's the one verb that's useful when
    // nothing is running, since it answers "what can I even start?".
    if args.verb == "ls" {
        return run_ls(&config, args.json);
    }

    let mut conn = match Connection::open(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("devc: {}", e);
            return EXIT_NO_INSTANCE;
        }
    };

    let name = args.name.clone().unwrap_or_default();
    let op = match args.verb.as_str() {
        "status" => Op::Status,
        "start" => Op::Start { name, wait: args.wait, timeout_ms: args.timeout_ms },
        "stop" => Op::Stop { name, wait: !args.no_wait, timeout_ms: args.timeout_ms },
        "restart" => Op::Restart { name, wait: args.wait, timeout_ms: args.timeout_ms },
        "run" => Op::Run { name },
        "logs" => Op::Logs { name, lines: args.lines },
        other => {
            eprintln!("devc: unknown command '{}'", other);
            return EXIT_USAGE;
        }
    };

    // A `--wait` request is answered only once the service gets there, so the
    // read deadline has to clear the request's own timeout, plus slack for the
    // tick the reply rides out on.
    let waiting = matches!(op, Op::Start { wait: true, .. } | Op::Stop { wait: true, .. } | Op::Restart { wait: true, .. });
    conn.set_read_timeout(std::time::Duration::from_millis(
        if waiting { args.timeout_ms + 5_000 } else { 10_000 },
    ));

    let resp = match conn.send(op) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("devc: {}", e);
            return EXIT_USAGE;
        }
    };

    match args.verb.as_str() {
        "status" => print_status(&resp, args.name.as_deref(), args.json),
        "logs" => {
            for line in resp.logs.iter().flatten() {
                println!("{}", line);
            }
            if resp.logs.is_none() {
                eprintln!("devc: {}", resp.reason);
            }
        }
        _ => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string())
                );
            } else if !resp.reason.is_empty() {
                let stream_is_err = resp.outcome.exit_code() != 0;
                if stream_is_err {
                    eprintln!("devc: {}", resp.reason);
                } else {
                    println!("{}", resp.reason);
                }
            }
        }
    }

    resp.outcome.exit_code()
}

fn run_ls(config: &Path, json: bool) -> i32 {
    let local = crate::local_config_path(config);
    let cfg = match crate::config::Config::load(config, local.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("devc: {}", e);
            return EXIT_USAGE;
        }
    };

    if json {
        let services: Vec<_> = cfg
            .services
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name, "key": s.key, "port": s.port, "command": s.command,
                })
            })
            .collect();
        let commands: Vec<_> = cfg
            .commands
            .iter()
            .map(|c| serde_json::json!({ "name": c.name, "key": c.key, "command": c.command }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "services": services, "commands": commands })
        );
        return 0;
    }

    if !cfg.services.is_empty() {
        println!("services:");
        for s in &cfg.services {
            match s.port {
                Some(p) => println!("  {:<20} [{}]  :{}", s.name, s.key, p),
                None => println!("  {:<20} [{}]", s.name, s.key),
            }
        }
    }
    if !cfg.commands.is_empty() {
        println!("commands:");
        for c in &cfg.commands {
            println!("  {:<20} [{}]", c.name, c.key);
        }
    }
    0
}

fn print_status(resp: &Response, filter: Option<&str>, json: bool) {
    let Some(snap) = resp.snapshot.as_ref() else {
        eprintln!("devc: {}", resp.reason);
        return;
    };

    let matches = |name: &str| filter.is_none_or(|f| name.eq_ignore_ascii_case(f));

    if json {
        let services: Vec<_> = snap
            .services
            .iter()
            .filter(|s| matches(&s.name))
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "status": status_word(s.status, s.owner),
                    "owner": owner_word(s.owner),
                    "pid": s.pid,
                    "port": s.port,
                    "port_active": s.port_active,
                    "url": s.url,
                    "needs_reload": s.dirty,
                    "removed_from_config": s.orphan,
                })
            })
            .collect();
        let commands: Vec<_> = snap
            .commands
            .iter()
            .filter(|c| matches(&c.name))
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "status": command_word(c.status),
                    "exit_code": c.exit_code,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "services": services, "commands": commands })
        );
        return;
    }

    let services: Vec<_> = snap.services.iter().filter(|s| matches(&s.name)).collect();
    if !services.is_empty() {
        println!("{:<20} {:<10} {:<8} PID", "SERVICE", "STATUS", "PORT");
        for s in services {
            println!(
                "{:<20} {:<10} {:<8} {}",
                s.name,
                status_word(s.status, s.owner),
                s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            );
        }
    }

    let commands: Vec<_> = snap.commands.iter().filter(|c| matches(&c.name)).collect();
    if !commands.is_empty() {
        println!();
        println!("{:<20} {:<10} EXIT", "COMMAND", "STATUS");
        for c in commands {
            println!(
                "{:<20} {:<10} {}",
                c.name,
                command_word(c.status),
                c.exit_code.map(|e| e.to_string()).unwrap_or_else(|| "-".into()),
            );
        }
    }
}

/// `external` outranks `stopped`: devc isn't running it, but something is, and
/// that distinction is the whole reason this command exists.
fn status_word(status: StatusKind, owner: OwnerKind) -> &'static str {
    match status {
        StatusKind::Running => "running",
        StatusKind::Starting => "starting",
        StatusKind::Stopping => "stopping",
        StatusKind::Stopped if owner == OwnerKind::External => "external",
        StatusKind::Stopped => "stopped",
    }
}

fn owner_word(owner: OwnerKind) -> &'static str {
    match owner {
        OwnerKind::None => "none",
        OwnerKind::Devc => "devc",
        OwnerKind::External => "external",
    }
}

fn command_word(status: CommandStatusKind) -> &'static str {
    match status {
        CommandStatusKind::Idle => "idle",
        CommandStatusKind::Running => "running",
        CommandStatusKind::Done => "done",
        CommandStatusKind::Failed => "failed",
    }
}

// ===== Attach =====

/// Second-view mode: render whatever the primary is showing and forward
/// keystrokes to it. Deliberately stateless — like `tmux attach`, this drives
/// the primary's cursor rather than keeping its own.
pub fn attach(canonical_config: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseEventKind};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use std::sync::mpsc;
    use std::time::Duration;

    let mut conn = Connection::open(canonical_config).map_err(|e| e.to_string())?;

    // Subscribe, and keep the first snapshot so there's something to draw
    // before the primary's next change.
    let first = conn.send(Op::Subscribe)?;
    let mut latest: Snapshot = first
        .snapshot
        .ok_or("devc did not send an initial snapshot")?;

    // Reader thread: snapshots in. Writes stay on the main thread.
    let (snap_tx, snap_rx) = mpsc::channel::<Snapshot>();
    let mut reader = BufReader::new(conn.stream.try_clone()?);
    std::thread::spawn(move || loop {
        let mut buf = String::new();
        match reader.read_line(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Ok(resp) = serde_json::from_str::<Response>(&buf) {
                    if let Some(s) = resp.snapshot {
                        if snap_tx.send(s).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    let _guard = crate::RawTerminal::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    loop {
        while let Ok(s) = snap_rx.try_recv() {
            latest = s;
        }
        terminal.draw(|f| crate::ui::draw(f, &latest))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                // Detaching must not take the primary's services down with it.
                KeyCode::Char('q') => break,
                KeyCode::Tab => proto::RemoteKey::Tab,
                KeyCode::BackTab => proto::RemoteKey::BackTab,
                KeyCode::Up => proto::RemoteKey::Up,
                KeyCode::Down => proto::RemoteKey::Down,
                KeyCode::Enter => proto::RemoteKey::Enter,
                KeyCode::Char(' ') => proto::RemoteKey::Space,
                KeyCode::PageUp => proto::RemoteKey::PageUp,
                KeyCode::PageDown => proto::RemoteKey::PageDown,
                KeyCode::Home => proto::RemoteKey::Home,
                KeyCode::End => proto::RemoteKey::End,
                KeyCode::Char(c) => proto::RemoteKey::Char(c),
                _ => continue,
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => proto::RemoteKey::ScrollUp,
                MouseEventKind::ScrollDown => proto::RemoteKey::ScrollDown,
                _ => continue,
            },
            _ => continue,
        };

        // Fire and forget: the ack arrives interleaved with snapshots on the
        // reader thread, and a dropped keystroke is not worth blocking a frame.
        let mut line = serde_json::to_string(&Request::new(Op::Key { key }))?;
        line.push('\n');
        if conn.stream.write_all(line.as_bytes()).is_err() {
            break; // primary went away
        }
        let _ = conn.stream.flush();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn verbs_are_recognised_and_paths_are_not() {
        assert!(is_verb("status"));
        assert!(is_verb("start"));
        assert!(!is_verb("devc.toml"));
        assert!(!is_verb("path/to/devc.toml"));
    }

    #[test]
    fn parses_a_name_and_flags() {
        let a = parse_args(&argv(&["start", "Web", "--wait", "--timeout", "5"])).unwrap();
        assert_eq!(a.verb, "start");
        assert_eq!(a.name.as_deref(), Some("Web"));
        assert!(a.wait);
        assert_eq!(a.timeout_ms, 5_000);
    }

    #[test]
    fn stop_defaults_to_waiting_with_a_shorter_timeout() {
        let a = parse_args(&argv(&["stop", "Web"])).unwrap();
        assert!(!a.no_wait, "stop should wait unless told otherwise");
        assert_eq!(a.timeout_ms, 10_000);

        // An explicit --timeout still wins.
        let a = parse_args(&argv(&["stop", "Web", "--timeout", "60"])).unwrap();
        assert_eq!(a.timeout_ms, 60_000);
    }

    #[test]
    fn start_does_not_wait_by_default() {
        let a = parse_args(&argv(&["start", "Web"])).unwrap();
        assert!(!a.wait);
        assert_eq!(a.timeout_ms, 30_000);
    }

    #[test]
    fn rejects_unknown_flags_and_extra_names() {
        assert!(parse_args(&argv(&["start", "Web", "--turbo"])).is_err());
        assert!(parse_args(&argv(&["start", "Web", "Api"])).is_err());
        assert!(parse_args(&argv(&["start", "Web", "--timeout"])).is_err());
        assert!(parse_args(&argv(&["start", "Web", "--timeout", "x"])).is_err());
        assert!(parse_args(&argv(&["start", "Web", "--timeout", "0"])).is_err());
    }

    #[test]
    fn config_flag_overrides_the_default_path() {
        let a = parse_args(&argv(&["status", "--config", "other/devc.toml"])).unwrap();
        assert_eq!(a.config, PathBuf::from("other/devc.toml"));

        let a = parse_args(&argv(&["status"])).unwrap();
        assert_eq!(a.config, PathBuf::from("devc.toml"));
    }

    #[test]
    fn external_reads_as_its_own_status_word() {
        assert_eq!(status_word(StatusKind::Stopped, OwnerKind::External), "external");
        assert_eq!(status_word(StatusKind::Stopped, OwnerKind::None), "stopped");
        assert_eq!(status_word(StatusKind::Running, OwnerKind::Devc), "running");
    }

    #[test]
    fn connecting_with_no_instance_explains_how_to_start_one() {
        let missing = std::env::temp_dir().join("devc-no-such-project/devc.toml");
        let err = match Connection::open(&missing) {
            Err(e) => e,
            Ok(_) => panic!("nothing should be listening for a nonexistent project"),
        };
        assert!(err.contains("no devc running"), "got: {}", err);
    }
}
