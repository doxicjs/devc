//! Control socket plumbing: accept connections, hand requests to the main
//! thread, push snapshots back out.
//!
//! Deliberately transport-only — what a request *means* lives in `app.rs`, on
//! the one thread that owns the service table. Nothing here mutates state, so
//! there are no locks around it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crate::protocol::{
    self as proto, ensure_runtime_dir_at, meta_path_in, runtime_dir, socket_path_in, InstanceMeta,
    Request, Response, Snapshot,
};

/// A request waiting to be applied, with the channel to answer on.
pub struct Job {
    pub req: Request,
    pub reply: mpsc::Sender<Response>,
}

/// What `bind` found at the socket path.
pub enum Bind {
    /// We own the socket — this process is the primary.
    Bound(Box<ControlServer>),
    /// A live devc answered. The caller should attach rather than start a
    /// second instance that would double-spawn every service.
    AlreadyRunning { pid: Option<i32> },
}

pub struct ControlServer {
    socket_path: PathBuf,
    meta_path: PathBuf,
    jobs: mpsc::Receiver<Job>,
    /// Attached clients, each with the version they last saw.
    subscribers: Vec<Subscriber>,
}

struct Subscriber {
    tx: mpsc::Sender<Response>,
    last_version: Option<u64>,
}

impl ControlServer {
    /// Claim the socket for `canonical_config`, or report the live instance
    /// already holding it.
    ///
    /// A socket file left behind by a crashed devc looks identical to a live
    /// one on disk, so liveness is established by handshake — see `probe_live`.
    /// Anything that fails that check gets its socket removed and rebound.
    pub fn bind(canonical_config: &Path) -> Result<Bind, String> {
        Self::bind_in(&runtime_dir(), canonical_config)
    }

    /// `bind` with an explicit runtime directory. Exists so tests can each get
    /// their own socket dir without touching process-wide environment.
    pub fn bind_in(dir: &Path, canonical_config: &Path) -> Result<Bind, String> {
        ensure_runtime_dir_at(dir)?;
        let sock = socket_path_in(dir, canonical_config);
        let meta = meta_path_in(dir, canonical_config);

        if sock.exists() {
            match probe_live(&sock, &meta) {
                Some(pid) => return Ok(Bind::AlreadyRunning { pid: Some(pid) }),
                None => {
                    // Nobody home — clear the corpse and take the address.
                    let _ = std::fs::remove_file(&sock);
                    let _ = std::fs::remove_file(&meta);
                }
            }
        }

        let listener = UnixListener::bind(&sock)
            .map_err(|e| format!("Failed to bind control socket {}: {}", sock.display(), e))?;

        let meta_json = serde_json::to_string(&InstanceMeta {
            pid: std::process::id() as i32,
            config_path: canonical_config.display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .map_err(|e| e.to_string())?;
        std::fs::write(&meta, meta_json)
            .map_err(|e| format!("Failed to write {}: {}", meta.display(), e))?;

        let (job_tx, job_rx) = mpsc::channel();
        thread::spawn(move || accept_loop(listener, job_tx));

        Ok(Bind::Bound(Box::new(Self {
            socket_path: sock,
            meta_path: meta,
            jobs: job_rx,
            subscribers: Vec::new(),
        })))
    }

    /// Non-blocking: everything that arrived since the last tick.
    pub fn drain_jobs(&mut self) -> Vec<Job> {
        let mut out = Vec::new();
        while let Ok(job) = self.jobs.try_recv() {
            out.push(job);
        }
        out
    }

    pub fn add_subscriber(&mut self, tx: mpsc::Sender<Response>) {
        self.subscribers.push(Subscriber { tx, last_version: None });
    }

    #[cfg(test)]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Push to any subscriber that hasn't seen this version. Send failures mean
    /// the client's writer thread is gone, so drop it.
    pub fn broadcast(&mut self, snapshot: &Snapshot) {
        if self.subscribers.is_empty() {
            return;
        }
        let mut payload: Option<Response> = None;
        self.subscribers.retain_mut(|sub| {
            if sub.last_version == Some(snapshot.version) {
                return true;
            }
            let resp = payload.get_or_insert_with(|| Response {
                outcome: proto::OutcomeKind::Changed,
                reason: String::new(),
                snapshot: Some(snapshot.clone()),
                logs: None,
            });
            match sub.tx.send(resp.clone()) {
                Ok(()) => {
                    sub.last_version = Some(snapshot.version);
                    true
                }
                Err(_) => false,
            }
        });
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        // Leave nothing for the next run to mistake for a live instance.
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.meta_path);
    }
}

fn accept_loop(listener: UnixListener, jobs: mpsc::Sender<Job>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let jobs = jobs.clone();
        thread::spawn(move || {
            if let Err(_e) = serve_connection(stream, jobs) {
                // A client that hung up mid-request is routine, not an error
                // worth surfacing into the TUI.
            }
        });
    }
}

/// One reader thread parses requests; one writer thread owns all writes. Both
/// are needed because an attached client sends keys while snapshots stream back.
fn serve_connection(stream: UnixStream, jobs: mpsc::Sender<Job>) -> std::io::Result<()> {
    let write_half = stream.try_clone()?;
    let (out_tx, out_rx) = mpsc::channel::<Response>();

    let writer = thread::spawn(move || {
        let mut w = write_half;
        while let Ok(resp) = out_rx.recv() {
            let Ok(mut line) = serde_json::to_string(&resp) else { continue };
            line.push('\n');
            if w.write_all(line.as_bytes()).is_err() || w.flush().is_err() {
                break;
            }
        }
        let _ = w.shutdown(std::net::Shutdown::Write);
    });

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = out_tx.send(Response::err(format!("malformed request: {}", e)));
                continue;
            }
        };
        if req.v != proto::PROTOCOL_VERSION {
            let _ = out_tx.send(Response::err(format!(
                "protocol mismatch: client speaks v{}, this devc speaks v{}",
                req.v,
                proto::PROTOCOL_VERSION
            )));
            continue;
        }
        if jobs.send(Job { req, reply: out_tx.clone() }).is_err() {
            break; // devc is shutting down
        }
    }

    drop(out_tx);
    let _ = writer.join();
    Ok(())
}

fn read_meta(path: &Path) -> Option<InstanceMeta> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Is `pid` still around? Signal 0 checks for existence without delivering
/// anything. EPERM means it exists but belongs to another user, which still
/// counts as alive.
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Decide whether the socket at `sock` belongs to a devc that will actually
/// talk to us, returning its pid if so.
///
/// A successful `connect` is *not* sufficient evidence: on macOS, connecting to
/// the path of a listener that was closed moments ago can transiently succeed
/// while the kernel finishes tearing the socket down. Attaching on that basis
/// would wire a fresh devc to a corpse. So we require a real reply.
///
/// The pid check runs first because it's instant: after a crash the recorded
/// pid is gone, and we can reclaim the socket without waiting on a handshake
/// that was never going to be answered.
fn probe_live(sock: &Path, meta_path: &Path) -> Option<i32> {
    const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

    let meta = read_meta(meta_path)?;
    if !pid_alive(meta.pid) {
        return None;
    }

    let mut stream = UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;

    let mut line = serde_json::to_string(&Request::new(proto::Op::Status)).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    stream.flush().ok()?;

    // A live devc answers on its next tick (~100ms). Anything that can't
    // produce a parseable response isn't something we should attach to.
    let mut reply = String::new();
    let n = BufReader::new(stream).read_line(&mut reply).ok()?;
    if n == 0 {
        return None;
    }
    serde_json::from_str::<Response>(&reply).ok()?;
    Some(meta.pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Op;
    use std::time::{Duration, Instant};

    /// A private socket directory per test. Tests run in parallel and the
    /// runtime dir is derived from process-wide env, so each test passes its
    /// own directory explicitly rather than mutating `XDG_RUNTIME_DIR` — one
    /// test's env change would otherwise land in another's `bind`.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devc-t{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn bind_or_panic(dir: &Path, cfg: &Path) -> Box<ControlServer> {
        match ControlServer::bind_in(dir, cfg).unwrap() {
            Bind::Bound(s) => s,
            Bind::AlreadyRunning { .. } => panic!("expected to claim the socket"),
        }
    }

    /// Stand-in for the event loop: keeps answering requests so the instance
    /// looks alive to `bind`'s liveness handshake. Returns a stop flag and the
    /// join handle that hands the server back.
    fn spawn_responder(
        mut server: Box<ControlServer>,
    ) -> (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread::JoinHandle<Box<ControlServer>>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                for job in server.drain_jobs() {
                    let _ = job
                        .reply
                        .send(Response::new(proto::OutcomeKind::NoOp, "ok"));
                }
                thread::sleep(Duration::from_millis(5));
            }
            server
        });
        (stop, handle)
    }

    #[test]
    fn bind_then_second_bind_reports_already_running() {
        use std::sync::atomic::Ordering;

        let dir = scratch("bind");
        let cfg = Path::new("/tmp/proj-a/devc.toml");

        let (stop, handle) = spawn_responder(bind_or_panic(&dir, cfg));

        match ControlServer::bind_in(&dir, cfg).unwrap() {
            Bind::AlreadyRunning { pid, .. } => {
                assert_eq!(pid, Some(std::process::id() as i32));
            }
            Bind::Bound(_) => panic!("second bind must not claim a live socket"),
        }

        stop.store(true, Ordering::SeqCst);
        drop(handle.join().unwrap());

        // Dropping unlinks the socket, so a later run binds cleanly.
        assert!(matches!(
            ControlServer::bind_in(&dir, cfg).unwrap(),
            Bind::Bound(_)
        ));
    }

    #[test]
    fn a_socket_whose_owner_is_gone_is_reclaimed() {
        let dir = scratch("dead-pid");
        let cfg = Path::new("/tmp/proj-h/devc.toml");
        ensure_runtime_dir_at(&dir).unwrap();

        // A socket file plus metadata naming a pid that no longer exists: what
        // a devc killed with SIGKILL leaves behind. pid 0 is never a live
        // process, so this must be reclaimed without waiting on a handshake.
        let sock = socket_path_in(&dir, cfg);
        let listener = UnixListener::bind(&sock).unwrap();
        std::fs::write(
            meta_path_in(&dir, cfg),
            serde_json::to_string(&InstanceMeta {
                pid: 0,
                config_path: cfg.display().to_string(),
                version: "0.0.0".into(),
            })
            .unwrap(),
        )
        .unwrap();

        let started = Instant::now();
        let bound = matches!(ControlServer::bind_in(&dir, cfg).unwrap(), Bind::Bound(_));
        drop(listener);

        assert!(bound, "a socket owned by a dead pid must be reclaimed");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a dead owner should be detected instantly, not by handshake timeout"
        );
    }

    #[test]
    fn two_projects_get_independent_sockets() {
        let dir = scratch("two-projects");
        let a = Path::new("/tmp/proj-one/devc.toml");
        let b = Path::new("/tmp/proj-two/devc.toml");

        let _server_a = bind_or_panic(&dir, a);
        // A different project in the same runtime dir must still bind.
        let _server_b = bind_or_panic(&dir, b);
    }

    #[test]
    fn a_stale_socket_file_is_reclaimed() {
        let dir = scratch("stale");
        let cfg = Path::new("/tmp/proj-b/devc.toml");

        // Simulate a devc killed with SIGKILL: socket file on disk, nothing
        // listening. `bind` must not mistake this for a live instance.
        ensure_runtime_dir_at(&dir).unwrap();
        let sock = socket_path_in(&dir, cfg);
        drop(UnixListener::bind(&sock).unwrap());
        assert!(sock.exists());

        assert!(
            matches!(ControlServer::bind_in(&dir, cfg).unwrap(), Bind::Bound(_)),
            "a stale socket must be reclaimed, not treated as live"
        );
    }

    #[test]
    fn the_runtime_dir_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        ensure_runtime_dir_at(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "socket dir must be user-only, got {:o}", mode);
    }

    #[test]
    fn a_request_reaches_the_main_thread_and_the_reply_comes_back() {
        let dir = scratch("roundtrip");
        let cfg = Path::new("/tmp/proj-c/devc.toml");
        let mut server = bind_or_panic(&dir, cfg);

        let mut client = UnixStream::connect(socket_path_in(&dir, cfg)).unwrap();
        let mut req = serde_json::to_string(&Request::new(Op::Status)).unwrap();
        req.push('\n');
        client.write_all(req.as_bytes()).unwrap();

        // Poll the way the event loop does.
        let deadline = Instant::now() + Duration::from_secs(2);
        let job = loop {
            if let Some(job) = server.drain_jobs().into_iter().next() {
                break job;
            }
            assert!(Instant::now() < deadline, "request never arrived");
            std::thread::sleep(Duration::from_millis(20));
        };

        assert!(matches!(job.req.op, Op::Status));
        job.reply
            .send(Response::new(proto::OutcomeKind::Changed, "pong"))
            .unwrap();

        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        let resp: Response = serde_json::from_str(&reply).unwrap();
        assert_eq!(resp.reason, "pong");
    }

    #[test]
    fn a_version_mismatch_is_rejected_without_reaching_the_app() {
        let dir = scratch("version");
        let cfg = Path::new("/tmp/proj-d/devc.toml");
        let mut server = bind_or_panic(&dir, cfg);

        let mut client = UnixStream::connect(socket_path_in(&dir, cfg)).unwrap();
        client
            .write_all(b"{\"v\":999,\"op\":{\"kind\":\"status\"}}\n")
            .unwrap();

        let mut reply = String::new();
        BufReader::new(client.try_clone().unwrap())
            .read_line(&mut reply)
            .unwrap();
        let resp: Response = serde_json::from_str(&reply).unwrap();
        assert_eq!(resp.outcome, proto::OutcomeKind::Failed);
        assert!(resp.reason.contains("protocol mismatch"), "got: {}", resp.reason);

        std::thread::sleep(Duration::from_millis(100));
        assert!(
            server.drain_jobs().is_empty(),
            "a mismatched client must never reach the service table"
        );
    }

    #[test]
    fn malformed_json_gets_an_error_rather_than_a_dropped_connection() {
        let dir = scratch("malformed");
        let cfg = Path::new("/tmp/proj-g/devc.toml");
        let _server = bind_or_panic(&dir, cfg);

        let mut client = UnixStream::connect(socket_path_in(&dir, cfg)).unwrap();
        client.write_all(b"not json at all\n").unwrap();

        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        let resp: Response = serde_json::from_str(&reply).unwrap();
        assert_eq!(resp.outcome, proto::OutcomeKind::Failed);
        assert!(resp.reason.contains("malformed"), "got: {}", resp.reason);
    }

    #[test]
    fn broadcast_skips_versions_a_subscriber_already_has() {
        let dir = scratch("broadcast");
        let cfg = Path::new("/tmp/proj-e/devc.toml");
        let mut server = bind_or_panic(&dir, cfg);

        let (tx, rx) = mpsc::channel();
        server.add_subscriber(tx);

        let mut snap = crate::app::empty_snapshot();
        snap.version = 7;
        server.broadcast(&snap);
        server.broadcast(&snap);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "same version must not be resent");

        snap.version = 8;
        server.broadcast(&snap);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn a_disconnected_subscriber_is_dropped() {
        let dir = scratch("drop-sub");
        let cfg = Path::new("/tmp/proj-f/devc.toml");
        let mut server = bind_or_panic(&dir, cfg);

        let (tx, rx) = mpsc::channel();
        server.add_subscriber(tx);
        assert_eq!(server.subscriber_count(), 1);

        drop(rx);
        let mut snap = crate::app::empty_snapshot();
        snap.version = 1;
        server.broadcast(&snap);
        assert_eq!(server.subscriber_count(), 0);
    }
}
