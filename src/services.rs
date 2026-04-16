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

pub struct ServiceState {
    pub id: ServiceId,
    pub config: ServiceConfig,
    pub process: Option<ProcessHandle>,
    pub status: ServiceStatus,
    pub port_active: bool,
    pub stopping_since: Option<Instant>,
    pub logs: VecDeque<String>,
    pub config_dirty: bool,
    pub orphan: bool,
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

    pub fn running_count(&self) -> usize {
        self.items.iter().filter(|s| s.status == ServiceStatus::Running).count()
    }

    pub fn toggle(&mut self, idx: usize, project_root: &Path) {
        if idx >= self.items.len() {
            return;
        }

        let status = self.items[idx].status;

        // Ignore if in transitional state
        if status == ServiceStatus::Starting || status == ServiceStatus::Stopping {
            return;
        }

        if status == ServiceStatus::Running {
            // Stop: send SIGTERM and enter Stopping state (non-blocking)
            let service = &mut self.items[idx];
            if let Some(ref proc) = service.process {
                proc.send_sigterm();
            }
            service.status = ServiceStatus::Stopping;
            service.stopping_since = Some(Instant::now());
            service.logs.push_back("── stopping ──".to_string());
        } else {
            let mut visited = Vec::<usize>::new();
            self.start_with_deps(idx, project_root, &mut visited);
        }
    }

    pub fn toggle_selected(&mut self, project_root: &Path) {
        let idx = self.selected;
        self.toggle(idx, project_root);
    }

    pub fn start_all(&mut self, project_root: &Path) {
        for i in 0..self.items.len() {
            if self.items[i].status == ServiceStatus::Stopped {
                self.toggle(i, project_root);
            }
        }
    }

    pub fn stop_all(&mut self) {
        for i in 0..self.items.len() {
            if self.items[i].status == ServiceStatus::Running {
                self.toggle(i, Path::new("."));
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

    pub fn port_targets(&self) -> Vec<(ServiceId, u16)> {
        self.items
            .iter()
            .filter_map(|s| s.config.port.map(|p| (s.id, p)))
            .collect()
    }

    pub fn apply_ports(&mut self, results: &[(ServiceId, bool)]) {
        for (id, active) in results {
            if let Some(s) = self.items.iter_mut().find(|s| s.id == *id) {
                s.port_active = *active;
            }
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
                        } else {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
                            service.logs.push_back("── process exited ──".to_string());
                        }
                    }
                }
                ServiceStatus::Running => {
                    if let Some(proc) = &mut service.process {
                        if !proc.is_running() {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
                            service.logs.push_back("── process exited ──".to_string());
                        }
                    }
                }
                ServiceStatus::Stopping => {
                    if let Some(proc) = &mut service.process {
                        if !proc.is_running() {
                            service.process = None;
                            service.status = ServiceStatus::Stopped;
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

    fn start_with_deps(&mut self, idx: usize, project_root: &Path, visited: &mut Vec<usize>) {
        if visited.contains(&idx) {
            return; // cycle detected
        }
        visited.push(idx);

        let deps: Vec<String> = self.items[idx].config.depends_on.clone();
        for dep_name in &deps {
            if let Some(dep_idx) = self.items.iter().position(|s| s.config.name == *dep_name) {
                if self.items[dep_idx].status == ServiceStatus::Stopped {
                    self.start_with_deps(dep_idx, project_root, visited);
                }
            }
        }
        self.start_service(idx, project_root);
    }

    fn start_service(&mut self, idx: usize, project_root: &Path) {
        let service = &mut self.items[idx];

        if service.port_active {
            if let Some(port) = service.config.port {
                service.logs.push_back(format!(
                    "── port {} already in use ──",
                    port
                ));
            }
            return;
        }

        service.status = ServiceStatus::Starting;
        service.config_dirty = false;

        let working_dir = project_root.join(&service.config.working_dir);
        let cmd = service.config.full_command();
        service.logs.push_back(format!("── starting: {} ──", cmd));

        let service_id = service.id;
        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            self.log_tx.clone(),
            move || LogSource::Service(service_id),
        ) {
            Ok(handle) => {
                self.items[idx].process = Some(handle);
            }
            Err(e) => {
                self.items[idx].logs.push_back(format!("error: {}", e));
                self.items[idx].status = ServiceStatus::Stopped;
            }
        }
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
    fn port_targets_includes_only_configured_ports() {
        let p = ServicesPane::from_config(vec![
            svc_cfg("a", "a", None),
            svc_cfg("b", "b", Some(3000)),
        ]);
        let targets = p.port_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].1, 3000);
    }
}
