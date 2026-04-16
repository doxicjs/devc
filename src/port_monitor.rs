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

pub struct PortMonitor {
    tx: mpsc::Sender<(ServiceId, bool)>,
    rx: mpsc::Receiver<(ServiceId, bool)>,
}

impl PortMonitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }

    pub fn should_check(&self, tick: u64) -> bool {
        tick % PORT_CHECK_INTERVAL == 1
    }

    pub fn kick(&self, targets: Vec<(ServiceId, u16)>) {
        if targets.is_empty() {
            return;
        }
        let sender = self.tx.clone();
        std::thread::spawn(move || {
            for (id, port) in targets {
                let addrs: [SocketAddr; 2] = [
                    format!("127.0.0.1:{}", port).parse().unwrap(),
                    format!("[::1]:{}", port).parse().unwrap(),
                ];
                let active = addrs
                    .iter()
                    .any(|addr| TcpStream::connect_timeout(addr, CONNECT_TIMEOUT).is_ok());
                let _ = sender.send((id, active));
            }
        });
    }

    pub fn drain(&self) -> Vec<(ServiceId, bool)> {
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
        m.kick(vec![(ServiceId(7), 1)]);
        std::thread::sleep(Duration::from_millis(200));
        let results = m.drain();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, ServiceId(7));
        assert!(!results[0].1);
    }
}
