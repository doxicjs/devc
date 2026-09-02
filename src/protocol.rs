//! Wire types for the control socket, and the rules for where that socket
//! lives.
//!
//! One newline-delimited JSON `Request` per line in, one `Response` per line
//! out. `Snapshot` does double duty: it's the payload an attached client
//! renders, and `devc status --json` is a projection of it — so the TUI and the
//! machine-readable output can't drift apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bumped only on incompatible changes. A client with a different version is
/// rejected with a message naming both, rather than misparsing.
pub const PROTOCOL_VERSION: u32 = 1;

// ===== Requests =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub op: Op,
}

impl Request {
    pub fn new(op: Op) -> Self {
        Self { v: PROTOCOL_VERSION, op }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    /// Full state, for `devc status`.
    Status,
    Start { name: String, wait: bool, timeout_ms: u64 },
    Stop { name: String, wait: bool, timeout_ms: u64 },
    Restart { name: String, wait: bool, timeout_ms: u64 },
    /// Run a `[[commands]]` entry.
    Run { name: String },
    Logs { name: String, lines: usize },
    /// Attach: stream a snapshot whenever the view changes.
    Subscribe,
    /// Attach: forward a keystroke to the primary's event handling.
    Key { key: RemoteKey },
}

/// The keys an attached TUI can forward. Deliberately an enum rather than raw
/// crossterm events — the wire format shouldn't be hostage to a dependency's
/// representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", content = "c", rename_all = "snake_case")]
pub enum RemoteKey {
    Char(char),
    Tab,
    BackTab,
    Up,
    Down,
    Enter,
    Space,
    PageUp,
    PageDown,
    Home,
    End,
    ScrollUp,
    ScrollDown,
}

// ===== Responses =====

/// Mirrors `services::Outcome`, kept separate so the wire format is stable
/// independent of internal refactors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Changed,
    NoOp,
    Refused,
    NotFound,
    Failed,
}

impl OutcomeKind {
    /// Process exit code. `NoOp` is a success: the caller asked for a state
    /// that already holds, which is the whole point of an idempotent API.
    pub fn exit_code(self) -> i32 {
        match self {
            OutcomeKind::Changed | OutcomeKind::NoOp => 0,
            OutcomeKind::Failed => 1,
            OutcomeKind::NotFound => 2,
            OutcomeKind::Refused => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub outcome: OutcomeKind,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<Vec<String>>,
}

impl Response {
    pub fn new(outcome: OutcomeKind, reason: impl Into<String>) -> Self {
        Self { outcome, reason: reason.into(), snapshot: None, logs: None }
    }
    pub fn err(reason: impl Into<String>) -> Self {
        Self::new(OutcomeKind::Failed, reason)
    }
    pub fn not_found(reason: impl Into<String>) -> Self {
        Self::new(OutcomeKind::NotFound, reason)
    }
}

// ===== Snapshot (view model + status payload) =====

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabKind {
    Services,
    Commands,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    /// Nothing running.
    None,
    /// This devc spawned it.
    Devc,
    /// Something outside devc holds the port.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatusKind {
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceRow {
    pub name: String,
    pub key: char,
    pub status: StatusKind,
    pub owner: OwnerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub port_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Config changed under a running service — needs a stop+start to apply.
    pub dirty: bool,
    /// Removed from config while still running.
    pub orphan: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRow {
    pub name: String,
    pub key: char,
    pub status: CommandStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub dirty: bool,
    pub orphan: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolRowKind {
    Link { url: String },
    Copy { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRow {
    pub name: String,
    pub key: char,
    #[serde(flatten)]
    pub kind: ToolRowKind,
}

/// Everything the TUI draws. Logs are carried only for the selected service and
/// selected command — that's all `draw_logs` ever reads, and shipping 500 lines
/// per entry on every frame would be pure waste.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Bumped whenever anything visible changes. Subscribers only get a push
    /// when this moves, so an idle devc sends nothing.
    pub version: u64,
    /// Drives the spinner animation.
    pub tick: u64,
    pub tab: TabKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_msg: Option<String>,
    pub conflicts: Vec<String>,

    pub services: Vec<ServiceRow>,
    pub service_selected: usize,
    pub service_logs: Vec<String>,
    pub service_log_scroll: usize,

    pub commands: Vec<CommandRow>,
    pub command_selected: usize,
    pub command_logs: Vec<String>,
    pub command_log_scroll: usize,

    pub tools: Vec<ToolRow>,
    pub tool_selected: usize,
}

impl Snapshot {
    pub fn running_count(&self) -> usize {
        self.services
            .iter()
            .filter(|s| s.status == StatusKind::Running)
            .count()
    }
}

// ===== Socket location =====

/// FNV-1a. Hand-rolled rather than `DefaultHasher` because two *separately
/// built* devc binaries have to derive the same socket path from the same
/// config path, and std makes no stability promise about its hasher's output
/// across releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Per-user directory holding one socket per project. `$XDG_RUNTIME_DIR` when
/// set (Linux), else `$TMPDIR` (macOS gives each user a private one), else
/// `/tmp`. The uid suffix matters only in the `/tmp` fallback, where the
/// directory is shared.
pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let uid = unsafe { libc::getuid() };
    base.join(format!("devc-{}", uid))
}

/// Create `dir` 0700 — the socket accepts commands that run whatever the config
/// says, so it must not be group- or world-reachable.
pub fn ensure_runtime_dir_at(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("Failed to secure {}: {}", dir.display(), e))?;
    Ok(())
}

/// Socket path for a project, keyed by its canonical config path. Hashed rather
/// than embedded because `sun_path` is capped at ~104 bytes on macOS, which a
/// real project path blows past easily.
pub fn socket_path_in(dir: &Path, canonical_config: &Path) -> PathBuf {
    dir.join(format!(
        "{:016x}.sock",
        fnv1a(canonical_config.as_os_str().as_encoded_bytes())
    ))
}

/// Sidecar next to the socket, so a client can name the pid it's talking to and
/// spot a hash collision instead of silently driving the wrong project.
pub fn meta_path_in(dir: &Path, canonical_config: &Path) -> PathBuf {
    dir.join(format!(
        "{:016x}.json",
        fnv1a(canonical_config.as_os_str().as_encoded_bytes())
    ))
}

pub fn socket_path(canonical_config: &Path) -> PathBuf {
    socket_path_in(&runtime_dir(), canonical_config)
}

#[cfg(test)]
pub fn meta_path(canonical_config: &Path) -> PathBuf {
    meta_path_in(&runtime_dir(), canonical_config)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMeta {
    pub pid: i32,
    pub config_path: String,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot {
            version: 3,
            tick: 9,
            tab: TabKind::Services,
            status_msg: Some("hi".into()),
            conflicts: vec![],
            services: vec![ServiceRow {
                name: "Web".into(),
                key: 'w',
                status: StatusKind::Running,
                owner: OwnerKind::Devc,
                pid: Some(42),
                port: Some(5173),
                port_active: true,
                url: None,
                dirty: false,
                orphan: false,
            }],
            service_selected: 0,
            service_logs: vec!["line".into()],
            service_log_scroll: 0,
            commands: vec![],
            command_selected: 0,
            command_logs: vec![],
            command_log_scroll: 0,
            tools: vec![ToolRow {
                name: "Docs".into(),
                key: 'd',
                kind: ToolRowKind::Link { url: "https://x".into() },
            }],
            tool_selected: 0,
        }
    }

    #[test]
    fn snapshot_round_trips() {
        let json = serde_json::to_string(&snap()).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 3);
        assert_eq!(back.services[0].name, "Web");
        assert_eq!(back.services[0].owner, OwnerKind::Devc);
        assert_eq!(back.running_count(), 1);
    }

    #[test]
    fn request_round_trips() {
        let req = Request::new(Op::Start {
            name: "Web".into(),
            wait: true,
            timeout_ms: 30_000,
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back.op {
            Op::Start { name, wait, timeout_ms } => {
                assert_eq!(name, "Web");
                assert!(wait);
                assert_eq!(timeout_ms, 30_000);
            }
            other => panic!("wrong op: {:?}", other),
        }
    }

    #[test]
    fn remote_key_round_trips() {
        for k in [RemoteKey::Char('x'), RemoteKey::Tab, RemoteKey::ScrollUp] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<RemoteKey>(&json).unwrap(), k);
        }
    }

    #[test]
    fn noop_is_a_successful_exit_code() {
        assert_eq!(OutcomeKind::NoOp.exit_code(), 0);
        assert_eq!(OutcomeKind::Changed.exit_code(), 0);
        assert_eq!(OutcomeKind::NotFound.exit_code(), 2);
        assert_eq!(OutcomeKind::Refused.exit_code(), 4);
    }

    #[test]
    fn socket_path_is_stable_and_per_project() {
        let a = Path::new("/tmp/one/devc.toml");
        let b = Path::new("/tmp/two/devc.toml");
        assert_eq!(socket_path(a), socket_path(a), "must be deterministic");
        assert_ne!(socket_path(a), socket_path(b));
        assert_eq!(socket_path(a).extension().unwrap(), "sock");
        assert_eq!(meta_path(a).extension().unwrap(), "json");
    }

    #[test]
    fn socket_path_fits_in_sun_path() {
        // macOS caps sun_path at 104 bytes; a deep project path must not
        // overflow it, which is the entire reason the path is hashed.
        let deep = PathBuf::from("/Users/someone").join("a/".repeat(60)).join("devc.toml");
        assert!(
            socket_path(&deep).as_os_str().len() < 100,
            "socket path too long: {}",
            socket_path(&deep).display()
        );
    }

    #[test]
    fn fnv1a_matches_known_vector() {
        // Canonical FNV-1a 64-bit test vector — guards against a typo in the
        // constants silently changing every socket path.
        assert_eq!(fnv1a(b"a"), 0xaf63dc4c8601ec8c);
    }
}
