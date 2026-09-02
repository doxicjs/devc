//! Batched TCP port monitoring for services.
//!
//! `kick` spawns one thread per batch that probes every target; results stream
//! back through an mpsc the caller drains on the next tick. Gated by
//! `should_check` so we only probe ~every 2 seconds (at 100ms tick rate).

use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use crate::id::ServiceId;

const PORT_CHECK_INTERVAL: u64 = 20;   // ticks (at ~100ms/tick)
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Synchronous liveness probe for a localhost port. Blocks for at most
/// 2 × CONNECT_TIMEOUT, so it's cheap enough to call on the UI thread
/// immediately before spawning a service — which is the point: the cached
/// `port_active` flag can be up to PORT_CHECK_INTERVAL ticks stale, and that
/// window is exactly where duplicate services get started.
pub fn probe_port(port: u16) -> bool {
    let addrs: [SocketAddr; 2] = [
        SocketAddr::from(([127, 0, 0, 1], port)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
    ];
    addrs
        .iter()
        .any(|addr| TcpStream::connect_timeout(addr, CONNECT_TIMEOUT).is_ok())
}

/// PID of whatever is listening on `port`, via `lsof`. Spawns a subprocess, so
/// this must only be called from the port-monitor thread — never the UI thread.
/// Returns None if lsof is missing, finds nothing, or returns something
/// unparseable (multiple listeners → first PID wins).
pub fn listener_pid(port: u16) -> Option<i32> {
    let out = std::process::Command::new("lsof")
        .args([
            "-nP",
            &format!("-iTCP:{}", port),
            "-sTCP:LISTEN",
            "-t",
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().parse::<i32>().ok())
}

/// One service's port to probe. `resolve_pid` is set by the caller for services
/// devc has no process for — if such a port answers, something *else* is
/// listening and we want to name it. Owned services skip the lsof entirely.
#[derive(Clone, Copy, Debug)]
pub struct PortTarget {
    pub id: ServiceId,
    pub port: u16,
    pub resolve_pid: bool,
}

/// Probe result: whether the port answered, and — when requested and active —
/// the PID of the foreign listener.
#[derive(Clone, Copy, Debug)]
pub struct PortResult {
    pub id: ServiceId,
    pub active: bool,
    pub pid: Option<i32>,
}

pub struct PortMonitor {
    tx: mpsc::Sender<PortResult>,
    rx: mpsc::Receiver<PortResult>,
}

impl PortMonitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }

    pub fn should_check(&self, tick: u64) -> bool {
        tick % PORT_CHECK_INTERVAL == 1
    }

    pub fn kick(&self, targets: Vec<PortTarget>) {
        if targets.is_empty() {
            return;
        }
        let sender = self.tx.clone();
        std::thread::spawn(move || {
            for t in targets {
                let active = probe_port(t.port);
                let pid = if active && t.resolve_pid {
                    listener_pid(t.port)
                } else {
                    None
                };
                let _ = sender.send(PortResult { id: t.id, active, pid });
            }
        });
    }

    pub fn drain(&self) -> Vec<PortResult> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            out.push(msg);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_check_is_gated() {
        let m = PortMonitor::new();
        assert!(!m.should_check(0));
        assert!(m.should_check(1));
        assert!(!m.should_check(2));
        assert!(m.should_check(21));  // next interval
    }

    #[test]
    fn drain_empty_returns_empty() {
        let m = PortMonitor::new();
        assert!(m.drain().is_empty());
    }

    #[test]
    fn kick_empty_is_noop() {
        let m = PortMonitor::new();
        m.kick(vec![]);
        std::thread::sleep(Duration::from_millis(50));
        assert!(m.drain().is_empty());
    }

    #[test]
    fn kick_closed_port_returns_inactive() {
        let m = PortMonitor::new();
        // Port 1 is almost certainly not listening on localhost.
        m.kick(vec![PortTarget { id: ServiceId(7), port: 1, resolve_pid: false }]);
        std::thread::sleep(Duration::from_millis(200));
        let results = m.drain();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, ServiceId(7));
        assert!(!results[0].active);
    }

    #[test]
    fn probe_port_detects_a_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe_port(port), "bound port should probe as active");
    }

    #[test]
    fn probe_port_reports_unbound_port_inactive() {
        // Bind only to be handed a port the OS considers free, then release it
        // without ever connecting — a probed-then-closed socket can linger in
        // the backlog and keep answering, which would make this flaky.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        assert!(!probe_port(port), "unbound port should probe as inactive");
    }

    #[test]
    fn kick_resolves_pid_only_when_requested() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let m = PortMonitor::new();
        m.kick(vec![PortTarget { id: ServiceId(1), port, resolve_pid: false }]);
        std::thread::sleep(Duration::from_millis(300));
        let r = m.drain();
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert!(r[0].pid.is_none(), "pid must not be resolved unless asked");
    }
}
