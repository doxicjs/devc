//! Services pane: long-running processes with start/stop, port monitoring,
//! and dependency ordering.
//!
//! Each service has a stable `ServiceId`. Logs flow through an owned mpsc.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::app::LogSource;
use crate::config::ServiceConfig;
use crate::id::ServiceId;
use crate::port_monitor::{probe_port, PortResult, PortTarget};
use crate::process::ProcessHandle;

pub const LOG_CAPACITY: usize = 500;
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
}

/// Who is actually holding this service up. The whole point of the
/// single-instance work: "not running" and "running, but not by us" are
/// different answers, and only the first one means "safe to start".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// No devc process and no port activity — genuinely down.
    None,
    /// devc spawned it and holds the handle.
    Devc,
    /// The port answers but devc has no process for it. Someone started this
    /// outside devc (a stray `pnpm dev` in a shell, a leftover container).
    /// `pid` is resolved lazily by the port monitor via lsof.
    External { pid: Option<i32> },
}

/// What a `start`/`stop` request actually did. Maps onto CLI exit codes:
/// NoOp is a *success* — the caller asked for a state that already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    NoOp,
    Refused,
    NotFound,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ActionResult {
    pub outcome: Outcome,
    pub reason: String,
}

impl ActionResult {
    fn changed(reason: impl Into<String>) -> Self {
        Self { outcome: Outcome::Changed, reason: reason.into() }
    }
    fn noop(reason: impl Into<String>) -> Self {
        Self { outcome: Outcome::NoOp, reason: reason.into() }
    }
    fn refused(reason: impl Into<String>) -> Self {
        Self { outcome: Outcome::Refused, reason: reason.into() }
    }
    fn not_found(reason: impl Into<String>) -> Self {
        Self { outcome: Outcome::NotFound, reason: reason.into() }
    }
    fn failed(reason: impl Into<String>) -> Self {
        Self { outcome: Outcome::Failed, reason: reason.into() }
    }
}

pub struct ServiceState {
    pub id: ServiceId,
    pub config: ServiceConfig,
    pub process: Option<ProcessHandle>,
    pub status: ServiceStatus,
    pub port_active: bool,
    pub owner: Owner,
    pub stopping_since: Option<Instant>,
    pub logs: VecDeque<String>,
    pub config_dirty: bool,
    pub orphan: bool,
}

impl ServiceState {
    /// PID of whatever is holding this service up, devc-owned or not.
    pub fn pid(&self) -> Option<i32> {
        match self.owner {
            Owner::Devc => self.process.as_ref().map(|p| p.pid()),
            Owner::External { pid } => pid,
            Owner::None => None,
        }
    }
}

#[derive(Default)]
pub struct ServicesDelta {
    pub added: usize,
    pub dropped: usize,
    pub pending_restart: usize,
    pub orphaned: usize,
}

pub struct ServicesPane {
    items: Vec<ServiceState>,
    selected: usize,
    pub log_scroll_offset: usize,
    log_rx: mpsc::Receiver<(LogSource, String)>,
    log_tx: mpsc::Sender<(LogSource, String)>,
    next_id: u64,
}

impl ServicesPane {
    pub fn from_config(configs: Vec<ServiceConfig>) -> Self {
        let (log_tx, log_rx) = mpsc::channel();
        let mut next_id: u64 = 0;
        let items = configs
            .into_iter()
            .map(|cfg| {
                next_id += 1;
                ServiceState {
                    id: ServiceId(next_id),
                    config: cfg,
                    process: None,
                    status: ServiceStatus::Stopped,
                    port_active: false,
                    owner: Owner::None,
                    stopping_since: None,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                }
            })
            .collect();
        Self { items, selected: 0, log_scroll_offset: 0, log_rx, log_tx, next_id }
    }

    pub fn items(&self) -> &[ServiceState] { &self.items }
    pub fn selected_idx(&self) -> usize { self.selected }
    #[allow(dead_code)]
    pub fn len(&self) -> usize { self.items.len() }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool { self.items.is_empty() }

    #[allow(dead_code)] // the TUI counts from the snapshot; kept for tests
    pub fn running_count(&self) -> usize {
        self.items.iter().filter(|s| s.status == ServiceStatus::Running).count()
    }

    pub fn find_by_name(&self, name: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|s| s.config.name.eq_ignore_ascii_case(name))
    }

    /// Bring a service up, idempotently. Starting something already up is a
    /// no-op that says so — that's what lets an agent call `devc start Web`
    /// without first having to remember whether it already did.
    pub fn start(&mut self, idx: usize, project_root: &Path) -> ActionResult {
        let Some(service) = self.items.get(idx) else {
            return ActionResult::not_found("no service at index");
        };

        match service.status {
            ServiceStatus::Running | ServiceStatus::Starting => {
                let pid = service.process.as_ref().map(|p| p.pid());
                return ActionResult::noop(match pid {
                    Some(p) => format!("already running (pid {})", p),
                    None => "already running".to_string(),
                });
            }
            ServiceStatus::Stopping => {
                return ActionResult::refused("service is stopping — retry once it's down");
            }
            ServiceStatus::Stopped => {}
        }

        if let Owner::External { pid } = service.owner {
            // Something outside devc is already serving this port. The caller
            // wanted the service up and it is up, so this is a success, not an
            // error — but we must not spawn a second one.
            let reason = external_reason(service, pid);
            self.items[idx].logs.push_back(format!("── {} ──", reason));
            return ActionResult::noop(reason);
        }

        let mut visited = Vec::<usize>::new();
        self.start_with_deps(idx, project_root, &mut visited)
    }

    /// Take a service down. Refuses to touch processes devc didn't spawn.
    pub fn stop(&mut self, idx: usize) -> ActionResult {
        let Some(service) = self.items.get_mut(idx) else {
            return ActionResult::not_found("no service at index");
        };

        if let Owner::External { pid } = service.owner {
            return ActionResult::refused(format!(
                "{} — devc didn't start it, so it won't stop it",
                external_reason(service, pid)
            ));
        }

        match service.status {
            ServiceStatus::Stopped => ActionResult::noop("already stopped"),
            ServiceStatus::Stopping => ActionResult::noop("already stopping"),
            ServiceStatus::Running | ServiceStatus::Starting => {
                if let Some(ref proc) = service.process {
                    proc.send_sigterm();
                }
                service.status = ServiceStatus::Stopping;
                service.stopping_since = Some(Instant::now());
                service.logs.push_back("── stopping ──".to_string());
                ActionResult::changed("stopping")
            }
        }
    }

    pub fn toggle(&mut self, idx: usize, project_root: &Path) {
        let Some(service) = self.items.get(idx) else { return };

        // Transitional states are still ignored on the keyboard path: a
        // half-pressed toggle during startup/shutdown is almost always a
        // mistake. The control API is explicit, so it doesn't need this guard.
        match service.status {
            ServiceStatus::Starting | ServiceStatus::Stopping => {}
            ServiceStatus::Running => {
                self.stop(idx);
            }
            ServiceStatus::Stopped => {
                self.start(idx, project_root);
            }
        }
    }

    pub fn toggle_selected(&mut self, project_root: &Path) {
        let idx = self.selected;
        self.toggle(idx, project_root);
    }

    pub fn stop_all(&mut self) {
        for i in 0..self.items.len() {
            if self.items[i].status == ServiceStatus::Running {
                self.stop(i);
            }
        }
    }

    pub fn find_by_key(&self, key: char) -> Option<usize> {
        let key_lower = key.to_ascii_lowercase();
        self.items
            .iter()
            .position(|s| s.config.key_char().to_ascii_lowercase() == key_lower)
    }

    pub fn open_url(&self, idx: usize) -> Result<String, String> {
        let Some(service) = self.items.get(idx) else {
            return Err("No service at index".to_string());
        };
        let Some(url) = service.config.open_url() else {
            return Err("No URL for this service".to_string());
        };
        match crate::platform::open_url(&url) {
            Ok(_) => Ok(format!("Opened: {}", url)),
            Err(e) => Err(format!("Error: {}", e)),
        }
    }

    pub fn port_targets(&self) -> Vec<PortTarget> {
        self.items
            .iter()
            .filter_map(|s| {
                s.config.port.map(|port| PortTarget {
                    id: s.id,
                    port,
                    // Only pay for lsof when a live port can't be ours.
                    resolve_pid: s.process.is_none(),
                })
            })
            .collect()
    }

    pub fn apply_ports(&mut self, results: &[PortResult]) {
        for r in results {
            let Some(s) = self.items.iter_mut().find(|s| s.id == r.id) else { continue };
            s.port_active = r.active;
            if s.process.is_some() {
                continue; // ownership is settled by check_processes
            }
            s.owner = if r.active {
                // Keep a previously resolved pid if this round's lsof came up
                // empty, so the label doesn't flicker between probes.
                let pid = r.pid.or(match s.owner {
                    Owner::External { pid } => pid,
                    _ => None,
                });
                Owner::External { pid }
            } else {
                Owner::None
            };
        }
    }

    pub fn poll_logs(&mut self) {
        while let Ok((source, line)) = self.log_rx.try_recv() {
            if let LogSource::Service(id) = source {
                if let Some(service) = self.items.iter_mut().find(|s| s.id == id) {
                    service.logs.push_back(line);
                    if service.logs.len() > LOG_CAPACITY {
                        service.logs.pop_front();
                    }
                }
            }
        }
    }

    pub fn check_processes(&mut self) {
        for service in &mut self.items {
            match service.status {
                ServiceStatus::Starting => {
                    if let Some(proc) = &mut service.process {
                        if proc.is_running() {
                            service.status = ServiceStatus::Running;
                            service.owner = Owner::Devc;
                        } else {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
                            service.owner = Owner::None;
                            service.logs.push_back("── process exited ──".to_string());
                        }
                    }
                }
                ServiceStatus::Running => {
                    if let Some(proc) = &mut service.process {
                        if !proc.is_running() {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
                            service.owner = Owner::None;
                            service.logs.push_back("── process exited ──".to_string());
                        }
                    }
                }
                ServiceStatus::Stopping => {
                    if let Some(proc) = &mut service.process {
                        if !proc.is_running() {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
                            service.owner = Owner::None;
                            service.stopping_since = None;
                            service.logs.push_back("── stopped ──".to_string());
                        } else if let Some(since) = service.stopping_since {
                            if since.elapsed() > KILL_TIMEOUT {
                                proc.send_sigkill();
                            }
                        }
                    } else {
                        service.status = ServiceStatus::Stopped;
                        service.stopping_since = None;
                    }
                }
                ServiceStatus::Stopped => {}
            }
        }
    }

    pub fn apply_config(&mut self, new: &[ServiceConfig]) -> ServicesDelta {
        let mut delta = ServicesDelta::default();
        let mut svc_drop: Vec<bool> = vec![false; self.items.len()];
        for (i, state) in self.items.iter_mut().enumerate() {
            if let Some(new_cfg) = new.iter().find(|s| s.name == state.config.name) {
                let changed = service_config_changed(&state.config, new_cfg);
                state.config = new_cfg.clone();
                state.orphan = false;
                if state.status != ServiceStatus::Stopped {
                    if changed {
                        state.config_dirty = true;
                        delta.pending_restart += 1;
                    }
                } else {
                    state.config_dirty = false;
                }
            } else if state.status != ServiceStatus::Stopped || state.process.is_some() {
                state.orphan = true;
                state.config_dirty = true;
                delta.orphaned += 1;
            } else {
                svc_drop[i] = true;
            }
        }
        while let Some(true) = svc_drop.last().copied() {
            self.items.pop();
            svc_drop.pop();
            delta.dropped += 1;
        }
        for cfg in new.iter() {
            let exists = self.items.iter().any(|s| s.config.name == cfg.name);
            if !exists {
                self.next_id += 1;
                self.items.push(ServiceState {
                    id: ServiceId(self.next_id),
                    config: cfg.clone(),
                    process: None,
                    status: ServiceStatus::Stopped,
                    port_active: false,
                    owner: Owner::None,
                    stopping_since: None,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                });
                delta.added += 1;
            }
        }
        if self.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
        delta
    }

    pub fn compact_stopped_orphans(&mut self) {
        while let Some(s) = self.items.last() {
            if s.orphan && s.status == ServiceStatus::Stopped && s.process.is_none() {
                self.items.pop();
            } else {
                break;
            }
        }
        if self.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
    }

    pub fn cleanup(&mut self) {
        for service in &mut self.items {
            if let Some(mut proc) = service.process.take() {
                proc.kill();
            }
        }
    }

    pub fn select_up(&mut self) {
        let new = self.selected.saturating_sub(1);
        if new != self.selected {
            self.selected = new;
            self.log_scroll_offset = 0;
        }
    }

    pub fn select_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
            self.log_scroll_offset = 0;
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        let max = self.items.get(self.selected).map_or(0, |s| s.logs.len());
        self.log_scroll_offset = self.log_scroll_offset.saturating_add(n).min(max);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.log_scroll_offset = self.log_scroll_offset.saturating_sub(n);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.log_scroll_offset = 0;
    }

    // --- Private helpers ---

    fn start_with_deps(
        &mut self,
        idx: usize,
        project_root: &Path,
        visited: &mut Vec<usize>,
    ) -> ActionResult {
        if visited.contains(&idx) {
            return ActionResult::refused("dependency cycle");
        }
        visited.push(idx);

        let deps: Vec<String> = self.items[idx].config.depends_on.clone();
        for dep_name in &deps {
            if let Some(dep_idx) = self.items.iter().position(|s| s.config.name == *dep_name) {
                if self.items[dep_idx].status == ServiceStatus::Stopped
                    && !matches!(self.items[dep_idx].owner, Owner::External { .. })
                {
                    self.start_with_deps(dep_idx, project_root, visited);
                }
            }
        }
        self.start_service(idx, project_root)
    }

    fn start_service(&mut self, idx: usize, project_root: &Path) -> ActionResult {
        // Probe the port *now* rather than trusting `port_active`, which the
        // monitor only refreshes every couple of seconds. That stale window is
        // precisely where a second copy of a server gets spawned.
        if let Some(port) = self.items[idx].config.port {
            if probe_port(port) {
                let service = &mut self.items[idx];
                service.port_active = true;
                let pid = service.pid();
                if service.owner == Owner::None {
                    service.owner = Owner::External { pid: None };
                }
                let reason = match pid {
                    Some(p) => format!("port {} already held by pid {}", port, p),
                    None => format!("port {} already in use", port),
                };
                service.logs.push_back(format!("── {} ──", reason));
                return ActionResult::noop(reason);
            }
        }

        let service = &mut self.items[idx];
        service.status = ServiceStatus::Starting;
        service.config_dirty = false;

        let working_dir = project_root.join(&service.config.working_dir);
        let cmd = service.config.command.clone();
        service.logs.push_back(format!("── starting: {} ──", cmd));

        let service_id = service.id;
        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            self.log_tx.clone(),
            move || LogSource::Service(service_id),
        ) {
            Ok(handle) => {
                let pid = handle.pid();
                self.items[idx].process = Some(handle);
                self.items[idx].owner = Owner::Devc;
                ActionResult::changed(format!("started (pid {})", pid))
            }
            Err(e) => {
                self.items[idx].logs.push_back(format!("error: {}", e));
                self.items[idx].status = ServiceStatus::Stopped;
                self.items[idx].owner = Owner::None;
                ActionResult::failed(format!("spawn failed: {}", e))
            }
        }
    }
}

/// Human-readable "someone else has this" message, shared by start and stop so
/// the two never drift.
fn external_reason(service: &ServiceState, pid: Option<i32>) -> String {
    let port = service
        .config
        .port
        .map(|p| format!("port {}", p))
        .unwrap_or_else(|| "the port".to_string());
    match pid {
        Some(p) => format!("{} held by external pid {}", port, p),
        None => format!("{} held by an external process", port),
    }
}

impl std::ops::Index<usize> for ServicesPane {
    type Output = ServiceState;
    fn index(&self, idx: usize) -> &ServiceState { &self.items[idx] }
}

impl std::ops::IndexMut<usize> for ServicesPane {
    fn index_mut(&mut self, idx: usize) -> &mut ServiceState { &mut self.items[idx] }
}

fn service_config_changed(a: &ServiceConfig, b: &ServiceConfig) -> bool {
    a.command != b.command
        || a.working_dir != b.working_dir
        || a.port != b.port
        || a.url != b.url
        || a.depends_on != b.depends_on
        || a.key != b.key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc_cfg(name: &str, key: &str, port: Option<u16>) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            key: key.into(),
            command: format!("echo {}", name),
            working_dir: "./".into(),
            port,
            url: None,
            depends_on: vec![],
        }
    }

    #[test]
    fn from_config_assigns_unique_ids() {
        let p = ServicesPane::from_config(vec![
            svc_cfg("a", "a", None),
            svc_cfg("b", "b", None),
        ]);
        assert_ne!(p.items()[0].id, p.items()[1].id);
    }

    #[test]
    fn apply_config_appends_new() {
        let mut p = ServicesPane::from_config(vec![svc_cfg("a", "a", None)]);
        let delta = p.apply_config(&[
            svc_cfg("a", "a", None),
            svc_cfg("b", "b", None),
        ]);
        assert_eq!(delta.added, 1);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn start_on_a_running_service_is_an_idempotent_noop() {
        let mut p = ServicesPane::from_config(vec![ServiceConfig {
            command: "sleep 5".into(),
            ..svc_cfg("a", "a", None)
        }]);

        let first = p.start(0, Path::new("."));
        assert_eq!(first.outcome, Outcome::Changed, "{}", first.reason);

        let second = p.start(0, Path::new("."));
        assert_eq!(
            second.outcome,
            Outcome::NoOp,
            "second start must not spawn a duplicate: {}",
            second.reason
        );
        assert!(second.reason.contains("already running"));

        p.cleanup();
    }

    #[test]
    fn start_refuses_to_duplicate_a_port_held_by_someone_else() {
        // Stand in for "an agent already ran `pnpm dev` in a shell".
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut p = ServicesPane::from_config(vec![ServiceConfig {
            command: "sleep 5".into(),
            ..svc_cfg("a", "a", Some(port))
        }]);

        // port_active is still false — the monitor hasn't run. The synchronous
        // pre-spawn probe is what has to catch this.
        assert!(!p.items()[0].port_active);

        let r = p.start(0, Path::new("."));
        assert_eq!(r.outcome, Outcome::NoOp, "{}", r.reason);
        assert!(r.reason.contains("already"), "got: {}", r.reason);
        assert!(p.items()[0].process.is_none(), "must not have spawned anything");
        assert_eq!(p.items()[0].status, ServiceStatus::Stopped);
    }

    #[test]
    fn stop_refuses_a_process_devc_did_not_start() {
        let mut p = ServicesPane::from_config(vec![svc_cfg("a", "a", Some(3000))]);
        p[0].owner = Owner::External { pid: Some(4242) };
        p[0].port_active = true;

        let r = p.stop(0);
        assert_eq!(r.outcome, Outcome::Refused);
        assert!(r.reason.contains("4242"), "got: {}", r.reason);
    }

    #[test]
    fn stop_on_a_stopped_service_is_a_noop() {
        let mut p = ServicesPane::from_config(vec![svc_cfg("a", "a", None)]);
        assert_eq!(p.stop(0).outcome, Outcome::NoOp);
    }

    #[test]
    fn actions_on_unknown_index_report_not_found() {
        let mut p = ServicesPane::from_config(vec![svc_cfg("a", "a", None)]);
        assert_eq!(p.start(9, Path::new(".")).outcome, Outcome::NotFound);
        assert_eq!(p.stop(9).outcome, Outcome::NotFound);
    }

    #[test]
    fn apply_ports_marks_unowned_live_ports_as_external() {
        let mut p = ServicesPane::from_config(vec![svc_cfg("a", "a", Some(3000))]);
        let id = p.items()[0].id;

        p.apply_ports(&[PortResult { id, active: true, pid: Some(99) }]);
        assert_eq!(p.items()[0].owner, Owner::External { pid: Some(99) });

        // A round where lsof came up empty keeps the last known pid.
        p.apply_ports(&[PortResult { id, active: true, pid: None }]);
        assert_eq!(p.items()[0].owner, Owner::External { pid: Some(99) });

        p.apply_ports(&[PortResult { id, active: false, pid: None }]);
        assert_eq!(p.items()[0].owner, Owner::None);
    }

    #[test]
    fn port_targets_request_pid_only_for_unowned_services() {
        let mut p = ServicesPane::from_config(vec![ServiceConfig {
            command: "sleep 5".into(),
            ..svc_cfg("a", "a", Some(3000))
        }]);
        assert!(p.port_targets()[0].resolve_pid, "no process yet — ask who's there");

        p.start(0, Path::new("."));
        assert!(!p.port_targets()[0].resolve_pid, "we own it — skip the lsof");

        p.cleanup();
    }

    #[test]
    fn find_by_name_is_case_insensitive() {
        let p = ServicesPane::from_config(vec![svc_cfg("Web", "w", None)]);
        assert_eq!(p.find_by_name("web"), Some(0));
        assert_eq!(p.find_by_name("WEB"), Some(0));
        assert_eq!(p.find_by_name("api"), None);
    }

    #[test]
    fn port_targets_includes_only_configured_ports() {
        let p = ServicesPane::from_config(vec![
            svc_cfg("a", "a", None),
            svc_cfg("b", "b", Some(3000)),
        ]);
        let targets = p.port_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].port, 3000);
    }
}
