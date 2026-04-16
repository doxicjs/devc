use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

/// Max lines kept per service/command log buffer (oldest evicted first).
const LOG_CAPACITY: usize = 500;
/// How often to check ports, in ticks. At 100ms/tick this is ~2 seconds.
const PORT_CHECK_INTERVAL: u64 = 20;
/// Seconds to wait after SIGTERM before escalating to SIGKILL.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

use crate::config::Config;
use crate::config::ServiceConfig;
use crate::config::CommandConfig;
use crate::id::{CommandId, ServiceId};
use crate::process::ProcessHandle;
use crate::status::StatusBar;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    Idle,
    Running,
    Done,
    Failed,
}

pub struct CommandState {
    pub id: CommandId,
    pub config: CommandConfig,
    pub process: Option<ProcessHandle>,
    pub status: CommandStatus,
    pub logs: VecDeque<String>,
    pub config_dirty: bool,
    pub orphan: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Services = 0,
    Commands = 1,
    Tools = 2,
}

pub enum ToolKind {
    Link(String),
    Copy(String),
}

pub struct ToolItem {
    pub name: String,
    pub key: char,
    pub kind: ToolKind,
}

/// Log messages are tagged with a source: Service(id) or Command(id)
pub enum LogSource {
    Service(ServiceId),
    Command(CommandId),
}

#[derive(Debug, Default)]
pub struct ReloadReport {
    pub services_added: usize,
    pub services_dropped: usize,
    pub services_pending_restart: usize,
    pub services_orphaned: usize,
    pub commands_added: usize,
    pub commands_dropped: usize,
    pub commands_pending_restart: usize,
    pub commands_orphaned: usize,
    pub project_root_changed: bool,
    pub key_conflicts: Vec<String>,
}

impl ReloadReport {
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let added = self.services_added + self.commands_added;
        if added > 0 {
            parts.push(format!("+{}", added));
        }
        let dropped = self.services_dropped + self.commands_dropped;
        if dropped > 0 {
            parts.push(format!("-{}", dropped));
        }
        let dirty = self.services_pending_restart
            + self.commands_pending_restart
            + self.services_orphaned
            + self.commands_orphaned;
        if dirty > 0 {
            parts.push(format!("{} reload needed", dirty));
        }
        if self.project_root_changed {
            parts.push("project_root pending".to_string());
        }
        if !self.key_conflicts.is_empty() {
            parts.push(format!("{} key conflict{}", self.key_conflicts.len(), if self.key_conflicts.len() == 1 { "" } else { "s" }));
        }
        if parts.is_empty() {
            "config reloaded".to_string()
        } else {
            format!("reloaded · {}", parts.join(" · "))
        }
    }
}

pub struct App {
    pub services: Vec<ServiceState>,
    pub commands: Vec<CommandState>,
    pub commands_selected: usize,
    pub selected: usize,
    pub tab: Tab,
    pub tools: Vec<ToolItem>,
    pub tools_selected: usize,
    pub status: StatusBar,
    pub tick: u64,
    next_service_id: u64,
    next_command_id: u64,
    log_receiver: mpsc::Receiver<(LogSource, String)>,
    log_sender: mpsc::Sender<(LogSource, String)>,
    port_sender: mpsc::Sender<(usize, bool)>,
    port_receiver: mpsc::Receiver<(usize, bool)>,
    project_root: PathBuf,
    config_dir: PathBuf,
    config_path: PathBuf,
    local_config_path: Option<PathBuf>,
    config_mtime: Option<SystemTime>,
    local_mtime: Option<SystemTime>,
    reload_pending_since: Option<Instant>,
    reload_fail_count: u8,
    pub log_scroll_offset: usize,
    pub cmd_log_scroll_offset: usize,
}

impl App {
    pub fn new(
        config: Config,
        config_dir: PathBuf,
        config_path: PathBuf,
        local_config_path: Option<PathBuf>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (port_tx, port_rx) = mpsc::channel();
        let project_root = config_dir.join(&config.general.project_root);

        let mut next_service_id: u64 = 0;
        let services: Vec<ServiceState> = config
            .services
            .into_iter()
            .map(|cfg| {
                next_service_id += 1;
                ServiceState {
                    id: ServiceId(next_service_id),
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

        let mut next_command_id: u64 = 0;
        let commands: Vec<CommandState> = config
            .commands
            .into_iter()
            .map(|cfg| {
                next_command_id += 1;
                CommandState {
                    id: CommandId(next_command_id),
                    config: cfg,
                    process: None,
                    status: CommandStatus::Idle,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                }
            })
            .collect();

        let mut tools: Vec<ToolItem> = Vec::new();
        for link in config.links {
            tools.push(ToolItem {
                key: link.key.chars().next().unwrap_or('?'),
                name: link.name,
                kind: ToolKind::Link(link.url),
            });
        }
        for copy in config.copies {
            tools.push(ToolItem {
                key: copy.key.chars().next().unwrap_or('?'),
                name: copy.name,
                kind: ToolKind::Copy(copy.text),
            });
        }

        for warning in detect_key_conflicts(&services, &commands, &tools) {
            eprintln!("warning: {}", warning);
        }

        fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
            std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
        }
        let config_mtime = file_mtime(&config_path);
        let local_mtime = local_config_path.as_ref().and_then(|p| file_mtime(p));

        Self {
            services,
            commands,
            commands_selected: 0,
            selected: 0,
            tab: Tab::Services,
            tools,
            tools_selected: 0,
            status: StatusBar::new(),
            tick: 0,
            next_service_id,
            next_command_id,
            log_receiver: rx,
            log_sender: tx,
            port_sender: port_tx,
            port_receiver: port_rx,
            project_root,
            config_dir,
            config_path,
            local_config_path,
            config_mtime,
            local_mtime,
            reload_pending_since: None,
            reload_fail_count: 0,
            log_scroll_offset: 0,
            cmd_log_scroll_offset: 0,
        }
    }

    // --- Tab ---

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Services => Tab::Commands,
            Tab::Commands => Tab::Tools,
            Tab::Tools => Tab::Services,
        };
    }

    pub fn prev_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Services => Tab::Tools,
            Tab::Commands => Tab::Services,
            Tab::Tools => Tab::Commands,
        };
    }

    // --- Navigation ---

    pub fn select_up(&mut self) {
        match self.tab {
            Tab::Services => {
                let new = self.selected.saturating_sub(1);
                if new != self.selected {
                    self.selected = new;
                    self.log_scroll_offset = 0;
                }
            }
            Tab::Commands => {
                let new = self.commands_selected.saturating_sub(1);
                if new != self.commands_selected {
                    self.commands_selected = new;
                    self.cmd_log_scroll_offset = 0;
                }
            }
            Tab::Tools => self.tools_selected = self.tools_selected.saturating_sub(1),
        }
    }

    pub fn select_down(&mut self) {
        match self.tab {
            Tab::Services => {
                if self.selected + 1 < self.services.len() {
                    self.selected += 1;
                    self.log_scroll_offset = 0;
                }
            }
            Tab::Commands => {
                if self.commands_selected + 1 < self.commands.len() {
                    self.commands_selected += 1;
                    self.cmd_log_scroll_offset = 0;
                }
            }
            Tab::Tools => {
                if self.tools_selected + 1 < self.tools.len() {
                    self.tools_selected += 1;
                }
            }
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        match self.tab {
            Tab::Services => {
                let max = self.services.get(self.selected).map_or(0, |s| s.logs.len());
                self.log_scroll_offset =
                    self.log_scroll_offset.saturating_add(amount).min(max);
            }
            Tab::Commands => {
                let max = self.commands.get(self.commands_selected).map_or(0, |c| c.logs.len());
                self.cmd_log_scroll_offset =
                    self.cmd_log_scroll_offset.saturating_add(amount).min(max);
            }
            Tab::Tools => {}
        }
    }

    pub fn scroll_down(&mut self, amount: usize) {
        match self.tab {
            Tab::Services => {
                self.log_scroll_offset = self.log_scroll_offset.saturating_sub(amount);
            }
            Tab::Commands => {
                self.cmd_log_scroll_offset = self.cmd_log_scroll_offset.saturating_sub(amount);
            }
            Tab::Tools => {}
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        match self.tab {
            Tab::Services => self.log_scroll_offset = 0,
            Tab::Commands => self.cmd_log_scroll_offset = 0,
            Tab::Tools => {}
        }
    }

    pub fn activate_selected(&mut self) {
        match self.tab {
            Tab::Services => {
                let idx = self.selected;
                self.toggle_service(idx);
            }
            Tab::Commands => {
                let idx = self.commands_selected;
                self.run_command(idx);
            }
            Tab::Tools => {
                let idx = self.tools_selected;
                self.activate_tool(idx);
            }
        }
    }

    // --- Services ---

    pub fn toggle_service(&mut self, idx: usize) {
        if idx >= self.services.len() {
            return;
        }

        let status = self.services[idx].status;

        // Ignore if in transitional state
        if status == ServiceStatus::Starting || status == ServiceStatus::Stopping {
            return;
        }

        if status == ServiceStatus::Running {
            // Stop: send SIGTERM and enter Stopping state (non-blocking)
            let service = &mut self.services[idx];
            if let Some(ref proc) = service.process {
                proc.send_sigterm();
            }
            service.status = ServiceStatus::Stopping;
            service.stopping_since = Some(Instant::now());
            service.logs.push_back("── stopping ──".to_string());
        } else {
            let mut visited = Vec::<usize>::new();
            self.start_with_deps(idx, &mut visited);
        }
    }

    fn start_with_deps(&mut self, idx: usize, visited: &mut Vec<usize>) {
        if visited.contains(&idx) {
            return; // cycle detected
        }
        visited.push(idx);

        let deps: Vec<String> = self.services[idx].config.depends_on.clone();
        for dep_name in &deps {
            if let Some(dep_idx) = self.find_service_by_name(dep_name) {
                if self.services[dep_idx].status == ServiceStatus::Stopped {
                    self.start_with_deps(dep_idx, visited);
                }
            }
        }
        self.start_service(idx);
    }

    fn start_service(&mut self, idx: usize) {
        let service = &mut self.services[idx];

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

        let working_dir = self.project_root.join(&service.config.working_dir);
        let cmd = service.config.full_command();
        service
            .logs
            .push_back(format!("── starting: {} ──", cmd));

        let service_id = service.id;
        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            self.log_sender.clone(),
            move || LogSource::Service(service_id),
        )
        {
            Ok(handle) => {
                service.process = Some(handle);
            }
            Err(e) => {
                service.logs.push_back(format!("error: {}", e));
                service.status = ServiceStatus::Stopped;
            }
        }
    }

    fn find_service_by_name(&self, name: &str) -> Option<usize> {
        self.services.iter().position(|s| s.config.name == name)
    }

    pub fn open_service_url(&mut self, idx: usize) {
        let Some(service) = self.services.get(idx) else {
            return;
        };
        let Some(url) = service.config.open_url() else {
            self.status.set("No URL for this service".to_string());
            return;
        };
        match crate::platform::open_url(&url) {
            Ok(_) => self.status.set(format!("Opened: {}", url)),
            Err(e) => self.status.set(format!("Error: {}", e)),
        }
    }

    pub fn find_service_by_key(&self, key: char) -> Option<usize> {
        let key_lower = key.to_ascii_lowercase();
        self.services
            .iter()
            .position(|s| s.config.key_char().to_ascii_lowercase() == key_lower)
    }

    pub fn start_all(&mut self) {
        for i in 0..self.services.len() {
            if self.services[i].status == ServiceStatus::Stopped {
                self.toggle_service(i);
            }
        }
    }

    pub fn stop_all(&mut self) {
        for i in 0..self.services.len() {
            if self.services[i].status == ServiceStatus::Running {
                self.toggle_service(i);
            }
        }
    }

    pub fn running_count(&self) -> usize {
        self.services
            .iter()
            .filter(|s| s.status == ServiceStatus::Running)
            .count()
    }

    // --- Commands ---

    pub fn run_command(&mut self, idx: usize) {
        if idx >= self.commands.len() {
            return;
        }

        // Don't run if already running
        if self.commands[idx].status == CommandStatus::Running {
            return;
        }

        let cmd_state = &mut self.commands[idx];
        cmd_state.logs.clear();
        cmd_state.status = CommandStatus::Running;
        cmd_state.config_dirty = false;

        let working_dir = self.project_root.join(&cmd_state.config.working_dir);
        let cmd = cmd_state.config.command.clone();
        cmd_state
            .logs
            .push_back(format!("── running: {} ──", cmd));

        let sender = self.log_sender.clone();
        let cmd_id = cmd_state.id;

        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            sender,
            move || LogSource::Command(cmd_id),
        ) {
            Ok(handle) => {
                cmd_state.process = Some(handle);
            }
            Err(e) => {
                cmd_state.logs.push_back(format!("error: {}", e));
                cmd_state.status = CommandStatus::Failed;
            }
        }
    }

    pub fn find_command_by_key(&self, key: char) -> Option<usize> {
        let key_lower = key.to_ascii_lowercase();
        self.commands
            .iter()
            .position(|c| c.config.key_char().to_ascii_lowercase() == key_lower)
    }

    // --- Tools ---

    pub fn find_tool_by_key(&self, key: char) -> Option<usize> {
        let key_lower = key.to_ascii_lowercase();
        self.tools
            .iter()
            .position(|t| t.key.to_ascii_lowercase() == key_lower)
    }

    pub fn activate_tool(&mut self, idx: usize) {
        let Some(tool) = self.tools.get(idx) else {
            return;
        };

        match &tool.kind {
            ToolKind::Link(url) => {
                let url = url.clone();
                match crate::platform::open_url(&url) {
                    Ok(_) => self.status.set(format!("Opened: {}", url)),
                    Err(e) => self.status.set(format!("Error: {}", e)),
                }
            }
            ToolKind::Copy(text) => {
                let text = text.clone();
                let name = tool.name.clone();
                match crate::platform::copy_to_clipboard(&text) {
                    Ok(_) => self.status.set(format!("Copied: {}", name)),
                    Err(e) => self.status.set(format!("Error: {}", e)),
                }
            }
        }
    }

    pub fn handle_char(&mut self, c: char) {
        match self.tab {
            Tab::Services => match c {
                'a' => self.start_all(),
                'x' => self.stop_all(),
                _ => {
                    if let Some(idx) = self.find_service_by_key(c) {
                        self.toggle_service(idx);
                    }
                }
            },
            Tab::Commands => {
                if let Some(idx) = self.find_command_by_key(c) {
                    self.run_command(idx);
                }
            }
            Tab::Tools => {
                if let Some(idx) = self.find_tool_by_key(c) {
                    self.activate_tool(idx);
                }
            }
        }
    }

    // --- Tick ---

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn poll_logs(&mut self) {
        while let Ok((source, line)) = self.log_receiver.try_recv() {
            match source {
                LogSource::Service(id) => {
                    if let Some(service) = self.services.iter_mut().find(|s| s.id == id) {
                        service.logs.push_back(line);
                        if service.logs.len() > LOG_CAPACITY {
                            service.logs.pop_front();
                        }
                    }
                }
                LogSource::Command(id) => {
                    if let Some(cmd) = self.commands.iter_mut().find(|c| c.id == id) {
                        cmd.logs.push_back(line);
                        if cmd.logs.len() > LOG_CAPACITY {
                            cmd.logs.pop_front();
                        }
                    }
                }
            }
        }
    }

    pub fn check_processes(&mut self) {
        for service in &mut self.services {
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

        // Check command processes
        for cmd in &mut self.commands {
            if cmd.status == CommandStatus::Running {
                if let Some(proc) = &mut cmd.process {
                    if !proc.is_running() {
                        let exit_code = proc.exit_code();
                        cmd.process = None;
                        if exit_code == Some(0) {
                            cmd.status = CommandStatus::Done;
                            cmd.logs.push_back("── done ──".to_string());
                        } else {
                            cmd.status = CommandStatus::Failed;
                            cmd.logs.push_back(format!(
                                "── failed (exit {}) ──",
                                exit_code.map(|c| c.to_string()).unwrap_or("?".into())
                            ));
                        }
                    }
                }
            }
        }
    }

    pub fn check_ports(&mut self) {
        // Poll results from previous checks
        while let Ok((idx, active)) = self.port_receiver.try_recv() {
            if let Some(service) = self.services.get_mut(idx) {
                service.port_active = active;
            }
        }

        if self.tick % PORT_CHECK_INTERVAL != 1 {
            return;
        }

        // Collect all ports to check, then spawn ONE thread for the batch
        let checks: Vec<(usize, u16)> = self
            .services
            .iter()
            .enumerate()
            .filter_map(|(idx, s)| s.config.port.map(|p| (idx, p)))
            .collect();

        if checks.is_empty() {
            return;
        }

        let sender = self.port_sender.clone();
        std::thread::spawn(move || {
            let timeout = Duration::from_millis(50);
            for (idx, port) in checks {
                let addrs: [SocketAddr; 2] = [
                    format!("127.0.0.1:{}", port).parse().unwrap(),
                    format!("[::1]:{}", port).parse().unwrap(),
                ];
                let active = addrs
                    .iter()
                    .any(|addr| TcpStream::connect_timeout(addr, timeout).is_ok());
                let _ = sender.send((idx, active));
            }
        });
    }

    pub fn cleanup(&mut self) {
        for service in &mut self.services {
            if let Some(mut proc) = service.process.take() {
                proc.kill();
            }
        }
        for cmd in &mut self.commands {
            if let Some(mut proc) = cmd.process.take() {
                proc.kill();
            }
        }
    }

    /// Tail-compact services and commands whose `orphan` flag is set and that have
    /// fully stopped (no process, status Stopped/Idle/Done/Failed). Called every tick
    /// so an orphan disappears as soon as the user stops it — no config-edit nudge needed.
    /// Tail-only: entries at the head may still have live background threads; those
    /// threads carry a stable typed ID so they can route logs correctly even if the
    /// slice index shifts — but we still avoid disturbing them to keep things simple.
    pub fn compact_stopped_orphans(&mut self) {
        while let Some(s) = self.services.last() {
            if s.orphan && s.status == ServiceStatus::Stopped && s.process.is_none() {
                self.services.pop();
            } else {
                break;
            }
        }
        if self.services.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.services.len() {
            self.selected = self.services.len() - 1;
        }
        while let Some(c) = self.commands.last() {
            if c.orphan && c.status != CommandStatus::Running && c.process.is_none() {
                self.commands.pop();
            } else {
                break;
            }
        }
        if self.commands.is_empty() {
            self.commands_selected = 0;
        } else if self.commands_selected >= self.commands.len() {
            self.commands_selected = self.commands.len() - 1;
        }
    }

    pub fn check_config_reload(&mut self) {
        fn file_mtime(path: &std::path::Path) -> Result<Option<SystemTime>, std::io::Error> {
            match std::fs::metadata(path) {
                Ok(m) => Ok(m.modified().ok()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }

        let main_mtime = match file_mtime(&self.config_path) {
            Ok(Some(m)) => {
                self.reload_fail_count = 0;
                Some(m)
            }
            Ok(None) | Err(_) => {
                // Missing or unreadable. Tolerate up to 3 consecutive ticks (atomic-rename window).
                self.reload_fail_count = self.reload_fail_count.saturating_add(1);
                if self.reload_fail_count == 3 {
                    self.status.set("config file missing".to_string());
                }
                return;
            }
        };

        let local_mtime = match self.local_config_path.as_ref() {
            Some(p) => file_mtime(p).unwrap_or(None),
            None => None,
        };

        let main_changed = main_mtime != self.config_mtime;
        let local_changed = local_mtime != self.local_mtime;

        if !main_changed && !local_changed && self.reload_pending_since.is_none() {
            return;
        }

        // First detection: start debounce window (~100ms = one more tick).
        if self.reload_pending_since.is_none() {
            self.reload_pending_since = Some(Instant::now());
            return;
        }

        // Wait until debounce elapses.
        if self.reload_pending_since.unwrap().elapsed() < Duration::from_millis(100) {
            return;
        }

        // Time to reload.
        let local_path = self.local_config_path.clone();
        match crate::config::Config::load(&self.config_path, local_path.as_deref()) {
            Ok(new_cfg) => {
                self.config_mtime = main_mtime;
                self.local_mtime = local_mtime;
                self.reload_pending_since = None;
                let report = self.apply_config(new_cfg);
                self.status.set(report.summary());
            }
            Err(e) => {
                self.reload_pending_since = None;
                // Leave stored mtimes unchanged so a subsequent edit (which will bump mtime
                // again) re-triggers a reload attempt.
                self.status.set(format!("config reload failed: {}", e));
            }
        }
    }

    pub fn apply_config(&mut self, new: Config) -> ReloadReport {
        let mut report = ReloadReport::default();

        // ----- Services -----
        // Update kept; mark removed-but-running as orphan (dirty); mark removed-and-stopped for drop.
        let mut svc_drop: Vec<bool> = vec![false; self.services.len()];
        for (i, state) in self.services.iter_mut().enumerate() {
            if let Some(new_cfg) = new.services.iter().find(|s| s.name == state.config.name) {
                let changed = service_config_changed(&state.config, new_cfg);
                state.config = new_cfg.clone();
                state.orphan = false;
                if state.status != ServiceStatus::Stopped {
                    if changed {
                        state.config_dirty = true;
                        report.services_pending_restart += 1;
                    }
                } else {
                    state.config_dirty = false;
                }
            } else if state.status != ServiceStatus::Stopped || state.process.is_some() {
                state.orphan = true;
                state.config_dirty = true;
                report.services_orphaned += 1;
            } else {
                svc_drop[i] = true;
            }
        }
        // Tail-compact: only safe to remove from the end (background threads carry
        // typed IDs but we keep the tail-compact invariant for simplicity).
        while let Some(true) = svc_drop.last().copied() {
            self.services.pop();
            svc_drop.pop();
            report.services_dropped += 1;
        }

        // Append new
        for cfg in new.services.iter() {
            let exists = self.services.iter().any(|s| s.config.name == cfg.name);
            if !exists {
                self.next_service_id += 1;
                self.services.push(ServiceState {
                    id: ServiceId(self.next_service_id),
                    config: cfg.clone(),
                    process: None,
                    status: ServiceStatus::Stopped,
                    port_active: false,
                    stopping_since: None,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                });
                report.services_added += 1;
            }
        }

        // Clamp selection if tail-compact shrunk the list.
        if self.services.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.services.len() {
            self.selected = self.services.len() - 1;
        }

        // ----- Commands -----
        // Kept+not-running → fully replace state (logs cleared, status reset).
        // Kept+running → keep state; set dirty if changed.
        // Removed+running → orphan (dirty).
        // Removed+stopped → mark for tail-compact drop.
        let mut cmd_drop: Vec<bool> = vec![false; self.commands.len()];
        for (i, state) in self.commands.iter_mut().enumerate() {
            if let Some(new_cfg) = new.commands.iter().find(|c| c.name == state.config.name) {
                let changed = command_config_changed(&state.config, new_cfg);
                if state.status == CommandStatus::Running {
                    state.config = new_cfg.clone();
                    state.orphan = false;
                    if changed {
                        state.config_dirty = true;
                        report.commands_pending_restart += 1;
                    }
                } else {
                    // Fully reset so old completion icon + trailing logs disappear.
                    let preserved_id = state.id;
                    *state = CommandState {
                        id: preserved_id,
                        config: new_cfg.clone(),
                        process: None,
                        status: CommandStatus::Idle,
                        logs: VecDeque::with_capacity(LOG_CAPACITY),
                        config_dirty: false,
                        orphan: false,
                    };
                }
            } else if state.status == CommandStatus::Running || state.process.is_some() {
                state.orphan = true;
                state.config_dirty = true;
                report.commands_orphaned += 1;
            } else {
                cmd_drop[i] = true;
            }
        }
        while let Some(true) = cmd_drop.last().copied() {
            self.commands.pop();
            cmd_drop.pop();
            report.commands_dropped += 1;
        }
        for cfg in new.commands.iter() {
            let exists = self.commands.iter().any(|c| c.config.name == cfg.name);
            if !exists {
                self.next_command_id += 1;
                self.commands.push(CommandState {
                    id: CommandId(self.next_command_id),
                    config: cfg.clone(),
                    process: None,
                    status: CommandStatus::Idle,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                });
                report.commands_added += 1;
            }
        }
        if self.commands.is_empty() {
            self.commands_selected = 0;
        } else if self.commands_selected >= self.commands.len() {
            self.commands_selected = self.commands.len() - 1;
        }

        // ----- Tools (full silent rebuild — no background threads) -----
        let mut tools: Vec<ToolItem> = Vec::new();
        for link in new.links.iter() {
            tools.push(ToolItem {
                key: link.key.chars().next().unwrap_or('?'),
                name: link.name.clone(),
                kind: ToolKind::Link(link.url.clone()),
            });
        }
        for c in new.copies.iter() {
            tools.push(ToolItem {
                key: c.key.chars().next().unwrap_or('?'),
                name: c.name.clone(),
                kind: ToolKind::Copy(c.text.clone()),
            });
        }
        self.tools = tools;
        if self.tools.is_empty() {
            self.tools_selected = 0;
        } else if self.tools_selected >= self.tools.len() {
            self.tools_selected = self.tools.len() - 1;
        }

        // ----- Project root (no mutation) -----
        let new_root = self.config_dir.join(&new.general.project_root);
        if new_root != self.project_root {
            report.project_root_changed = true;
        }

        // ----- Key conflicts -----
        report.key_conflicts = detect_key_conflicts(&self.services, &self.commands, &self.tools);

        report
    }
}

fn service_config_changed(a: &ServiceConfig, b: &ServiceConfig) -> bool {
    a.command != b.command
        || a.working_dir != b.working_dir
        || a.port != b.port
        || a.url != b.url
        || a.depends_on != b.depends_on
        || a.key != b.key
        || a.service_type != b.service_type
}

fn command_config_changed(a: &CommandConfig, b: &CommandConfig) -> bool {
    a.command != b.command || a.working_dir != b.working_dir || a.key != b.key
}

fn detect_key_conflicts(
    services: &[ServiceState],
    commands: &[CommandState],
    tools: &[ToolItem],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let reserved = ['a', 'x'];
    let mut seen = Vec::<char>::new();
    for s in services.iter() {
        let k = s.config.key_char().to_ascii_lowercase();
        if k == 'q' {
            out.push(format!("service '{}' key '{}' conflicts with quit", s.config.name, k));
        }
        if reserved.contains(&k) {
            out.push(format!("service '{}' key '{}' conflicts with reserved shortcut", s.config.name, k));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate service key '{}'", k));
        } else {
            seen.push(k);
        }
    }
    seen.clear();
    for c in commands.iter() {
        let k = c.config.key_char().to_ascii_lowercase();
        if k == 'q' {
            out.push(format!("command '{}' key '{}' conflicts with quit", c.config.name, k));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate command key '{}'", k));
        } else {
            seen.push(k);
        }
    }
    seen.clear();
    for t in tools.iter() {
        let k = t.key.to_ascii_lowercase();
        if k == 'q' {
            out.push(format!("tool '{}' key '{}' conflicts with quit", t.name, k));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate tool key '{}'", k));
        } else {
            seen.push(k);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::id::{CommandId, ServiceId};
    use std::path::PathBuf;

    fn svc(name: &str, key: &str, port: Option<u16>) -> ServiceConfig {
        ServiceConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
            service_type: "generic".to_string(),
            port,
            url: None,
            depends_on: vec![],
        }
    }

    fn cmd(name: &str, key: &str, command: &str) -> CommandConfig {
        CommandConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: command.to_string(),
            working_dir: "./".to_string(),
        }
    }

    fn app_with(services: Vec<ServiceConfig>, commands: Vec<CommandConfig>) -> App {
        let config = Config {
            general: General { project_root: "./".to_string() },
            services,
            commands,
            links: vec![],
            copies: vec![],
        };
        App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None)
    }

    fn empty_app() -> App {
        app_with(vec![], vec![])
    }

    // ===== Construction =====

    #[test]
    fn new_with_empty_services() {
        let app = app_with(vec![], vec![cmd("build", "b", "echo build")]);
        assert!(app.services.is_empty());
        assert_eq!(app.commands.len(), 1);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn new_completely_empty() {
        let app = empty_app();
        assert!(app.services.is_empty());
        assert!(app.commands.is_empty());
        assert!(app.tools.is_empty());
        assert_eq!(app.tab, Tab::Services);
    }

    // ===== Tab navigation =====

    #[test]
    fn next_tab_cycles_forward() {
        let mut app = empty_app();
        assert_eq!(app.tab, Tab::Services);
        app.next_tab();
        assert_eq!(app.tab, Tab::Commands);
        app.next_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.next_tab();
        assert_eq!(app.tab, Tab::Services);
    }

    // ===== Selection: up/down =====

    #[test]
    fn select_up_at_zero_stays() {
        let mut app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        assert_eq!(app.selected, 0);
        app.select_up();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_down_advances() {
        let mut app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        app.select_down();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn select_down_at_last_stays() {
        let mut app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        app.select_down();
        app.select_down();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn select_down_empty_services() {
        let mut app = empty_app();
        app.select_down();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_up_empty_services() {
        let mut app = empty_app();
        app.select_up();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_down_commands_tab() {
        let mut app = app_with(
            vec![],
            vec![cmd("a", "a", "echo a"), cmd("b", "b", "echo b")],
        );
        app.tab = Tab::Commands;
        app.select_down();
        assert_eq!(app.commands_selected, 1);
    }

    #[test]
    fn select_down_commands_empty() {
        let mut app = empty_app();
        app.tab = Tab::Commands;
        app.select_down();
        assert_eq!(app.commands_selected, 0);
    }

    // ===== Empty list safety =====

    #[test]
    fn toggle_service_empty_no_panic() {
        let mut app = empty_app();
        app.toggle_service(0);
    }

    #[test]
    fn toggle_service_out_of_bounds_no_panic() {
        let mut app = app_with(vec![svc("a", "a", None)], vec![]);
        app.toggle_service(99);
    }

    #[test]
    fn run_command_empty_no_panic() {
        let mut app = empty_app();
        app.run_command(0);
    }

    #[test]
    fn run_command_out_of_bounds_no_panic() {
        let mut app = app_with(vec![], vec![cmd("a", "a", "echo a")]);
        app.run_command(99);
    }

    #[test]
    fn activate_selected_empty_services_no_panic() {
        let mut app = empty_app();
        app.tab = Tab::Services;
        app.activate_selected();
    }

    #[test]
    fn activate_selected_empty_commands_no_panic() {
        let mut app = empty_app();
        app.tab = Tab::Commands;
        app.activate_selected();
    }

    #[test]
    fn activate_selected_empty_tools_no_panic() {
        let mut app = empty_app();
        app.tab = Tab::Tools;
        app.activate_selected();
    }

    // ===== Issue #14: port_active=true with port=None should not panic =====

    #[test]
    fn start_service_port_active_no_port_no_panic() {
        let mut app = app_with(vec![svc("worker", "w", None)], vec![]);
        app.services[0].port_active = true;
        // toggle_service -> start_service -> unwraps port — should NOT panic
        app.toggle_service(0);
    }

    // ===== Issue #4: cleanup must kill command processes =====

    #[test]
    fn cleanup_kills_command_processes() {
        let mut app = app_with(
            vec![],
            vec![cmd("sleeper", "s", "sleep 100")],
        );
        app.run_command(0);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            app.commands[0].process.is_some(),
            "Command should have a running process"
        );

        app.cleanup();

        assert!(
            app.commands[0].process.is_none(),
            "cleanup() must take and kill command processes"
        );
    }

    // ===== Lookups =====

    #[test]
    fn find_service_by_key_found() {
        let app = app_with(vec![svc("web", "w", None), svc("api", "a", None)], vec![]);
        assert_eq!(app.find_service_by_key('w'), Some(0));
        assert_eq!(app.find_service_by_key('a'), Some(1));
    }

    #[test]
    fn find_service_by_key_case_insensitive() {
        let app = app_with(vec![svc("web", "w", None)], vec![]);
        assert_eq!(app.find_service_by_key('W'), Some(0));
    }

    #[test]
    fn find_service_by_key_no_match() {
        let app = app_with(vec![svc("web", "w", None)], vec![]);
        assert_eq!(app.find_service_by_key('z'), None);
    }

    #[test]
    fn find_service_by_key_empty() {
        let app = empty_app();
        assert_eq!(app.find_service_by_key('w'), None);
    }

    #[test]
    fn find_command_by_key_found() {
        let app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        assert_eq!(app.find_command_by_key('b'), Some(0));
    }

    #[test]
    fn find_command_by_key_no_match() {
        let app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        assert_eq!(app.find_command_by_key('z'), None);
    }

    // ===== Running count =====

    #[test]
    fn running_count_empty() {
        let app = empty_app();
        assert_eq!(app.running_count(), 0);
    }

    #[test]
    fn running_count_all_stopped() {
        let app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        assert_eq!(app.running_count(), 0);
    }

    // ===== handle_char: reserved key behavior (Issue #6) =====

    #[test]
    fn handle_char_a_triggers_start_all_not_service_key() {
        // Service keyed 'a' — pressing 'a' goes through start_all() path,
        // not the direct key-lookup path. This documents the conflict.
        let mut app = app_with(vec![svc("alpha", "a", None)], vec![]);
        app.handle_char('a');
        // start_all calls toggle_service for each stopped service, so the
        // service still gets started — but through start_all, not direct toggle.
        assert_ne!(app.services[0].status, ServiceStatus::Stopped);
    }

    #[test]
    fn handle_char_x_triggers_stop_all_not_service_key() {
        let mut app = app_with(vec![svc("xray", "x", None)], vec![]);
        // 'x' calls stop_all. With no running services, nothing happens.
        app.handle_char('x');
        // Service stays stopped — stop_all only acts on Running services
        assert_eq!(app.services[0].status, ServiceStatus::Stopped);
    }

    // ===== Tick =====

    #[test]
    fn tick_increments() {
        let mut app = empty_app();
        assert_eq!(app.tick, 0);
        app.tick();
        assert_eq!(app.tick, 1);
    }

    #[test]
    fn tick_wraps_at_max() {
        let mut app = empty_app();
        app.tick = u64::MAX;
        app.tick();
        assert_eq!(app.tick, 0);
    }

    // ===== Status =====

    #[test]
    fn status_set_and_current() {
        let mut app = empty_app();
        app.status.set("test".to_string());
        assert_eq!(app.status.current(), Some("test"));
    }

    #[test]
    fn status_clear_if_expired_keeps_fresh() {
        let mut app = empty_app();
        app.status.set("test".to_string());
        app.status.clear_if_expired();
        assert_eq!(app.status.current(), Some("test"));
    }

    // ===== open_service_url =====

    #[test]
    fn open_service_url_out_of_bounds_no_panic() {
        let mut app = empty_app();
        app.open_service_url(0);
        app.open_service_url(99);
    }

    // ===== depends_on edge cases =====

    #[test]
    fn toggle_service_missing_dependency_no_panic() {
        let mut s = svc("web", "w", None);
        s.depends_on = vec!["nonexistent".to_string()];
        let mut app = app_with(vec![s], vec![]);
        app.toggle_service(0);
    }

    // ===== poll_logs: invalid indices =====

    #[test]
    fn poll_logs_invalid_service_index_no_panic() {
        let mut app = empty_app();
        let _ = app.log_sender.send((LogSource::Service(ServiceId(999)), "ghost".to_string()));
        app.poll_logs();
    }

    #[test]
    fn poll_logs_invalid_command_index_no_panic() {
        let mut app = empty_app();
        let _ = app.log_sender.send((LogSource::Command(CommandId(999)), "ghost".to_string()));
        app.poll_logs();
    }

    // ===== Log capping at 500 =====

    #[test]
    fn service_logs_capped_at_500() {
        let mut app = app_with(vec![svc("web", "w", None)], vec![]);
        let svc_id = app.services[0].id;
        for i in 0..600 {
            let _ = app.log_sender.send((LogSource::Service(svc_id), format!("line {}", i)));
        }
        app.poll_logs();
        assert_eq!(app.services[0].logs.len(), 500);
        // Oldest 100 lines evicted (0..99), first remaining is "line 100"
        assert!(app.services[0].logs.front().unwrap().contains("100"));
    }

    #[test]
    fn command_logs_capped_at_500() {
        let mut app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        let cmd_id = app.commands[0].id;
        for i in 0..600 {
            let _ = app.log_sender.send((LogSource::Command(cmd_id), format!("line {}", i)));
        }
        app.poll_logs();
        assert_eq!(app.commands[0].logs.len(), 500);
    }

    // ===== Fix 1: Recursive dependency resolution =====

    #[test]
    fn toggle_service_starts_transitive_deps() {
        // A depends on B, B depends on C. Starting A should start C then B then A.
        let c = svc("c", "c", None);
        let mut b = svc("b", "b", None);
        b.depends_on = vec!["c".to_string()];
        let mut a = svc("a", "a", None);
        a.depends_on = vec!["b".to_string()];
        let mut app = app_with(vec![a, b, c], vec![]);
        app.toggle_service(0); // start A
        // All three should be starting (not just A and B)
        assert_ne!(app.services[2].status, ServiceStatus::Stopped, "C should be started transitively");
        assert_ne!(app.services[1].status, ServiceStatus::Stopped, "B should be started");
        assert_ne!(app.services[0].status, ServiceStatus::Stopped, "A should be started");
    }

    #[test]
    fn circular_dependency_does_not_infinite_loop() {
        // A depends on B, B depends on A — must not stack overflow
        let mut a = svc("a", "a", None);
        a.depends_on = vec!["b".to_string()];
        let mut b = svc("b", "b", None);
        b.depends_on = vec!["a".to_string()];
        let mut app = app_with(vec![a, b], vec![]);
        app.toggle_service(0); // should not hang or panic
    }

    #[test]
    fn self_dependency_does_not_infinite_loop() {
        let mut a = svc("a", "a", None);
        a.depends_on = vec!["a".to_string()];
        let mut app = app_with(vec![a], vec![]);
        app.toggle_service(0); // should not hang or panic
    }

    // ===== Fix 4: Scroll offset clamped to log length =====

    #[test]
    fn scroll_up_clamped_to_log_length() {
        let mut app = app_with(vec![svc("web", "w", None)], vec![]);
        let svc_id = app.services[0].id;
        // Add 10 log lines
        for i in 0..10 {
            let _ = app.log_sender.send((LogSource::Service(svc_id), format!("line {}", i)));
        }
        app.poll_logs();
        // Scroll up way past the log length
        app.scroll_up(1000);
        // Offset should be clamped to the number of log lines
        assert!(
            app.log_scroll_offset <= app.services[0].logs.len(),
            "Scroll offset {} should be clamped to log length {}",
            app.log_scroll_offset,
            app.services[0].logs.len()
        );
    }

    // ===== Fix 5: Integration — full flow tests =====

    #[test]
    fn integration_toggle_service_produces_logs() {
        let mut app = app_with(vec![svc("echo", "e", None)], vec![]);
        app.toggle_service(0);
        assert_eq!(app.services[0].status, ServiceStatus::Starting);
        // Should have a "starting" log entry
        assert!(app.services[0].logs.iter().any(|l| l.contains("starting")));
    }

    #[test]
    fn integration_run_command_completes() {
        let mut app = app_with(vec![], vec![cmd("echo", "e", "echo done")]);
        app.run_command(0);
        assert_eq!(app.commands[0].status, CommandStatus::Running);
        // Wait for command to finish
        std::thread::sleep(std::time::Duration::from_millis(500));
        app.poll_logs();
        app.check_processes();
        assert_eq!(app.commands[0].status, CommandStatus::Done);
        // Should have the "done" marker
        assert!(app.commands[0].logs.iter().any(|l| l.contains("done")));
    }

    #[test]
    fn integration_failed_command_reports_failure() {
        let mut app = app_with(vec![], vec![cmd("fail", "f", "false")]);
        app.run_command(0);
        std::thread::sleep(std::time::Duration::from_millis(500));
        app.poll_logs();
        app.check_processes();
        assert_eq!(app.commands[0].status, CommandStatus::Failed);
    }

    #[test]
    fn integration_command_output_collected() {
        let mut app = app_with(vec![], vec![cmd("echo", "e", "echo hello_world")]);
        app.run_command(0);
        std::thread::sleep(std::time::Duration::from_millis(500));
        app.poll_logs();
        assert!(
            app.commands[0].logs.iter().any(|l| l.contains("hello_world")),
            "Command output should be collected in logs"
        );
    }

    #[test]
    fn integration_tab_navigation_full_cycle() {
        let mut app = app_with(
            vec![svc("web", "w", None)],
            vec![cmd("build", "b", "echo b")],
        );
        // Forward cycle
        assert_eq!(app.tab, Tab::Services);
        app.next_tab();
        assert_eq!(app.tab, Tab::Commands);
        app.next_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.next_tab();
        assert_eq!(app.tab, Tab::Services);
        // Backward cycle
        app.prev_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Commands);
        // Mixed
        app.next_tab();
        app.prev_tab();
        assert_eq!(app.tab, Tab::Commands);
    }

    #[test]
    fn integration_selection_and_activation() {
        let mut app = app_with(
            vec![svc("a", "a", None), svc("b", "b", None)],
            vec![],
        );
        assert_eq!(app.selected, 0);
        app.select_down();
        assert_eq!(app.selected, 1);
        app.activate_selected(); // toggles service at index 1
        assert_ne!(app.services[1].status, ServiceStatus::Stopped);
        // Service 0 should still be stopped
        assert_eq!(app.services[0].status, ServiceStatus::Stopped);
    }

    #[test]
    fn integration_handle_char_dispatches_to_correct_tab() {
        let mut app = app_with(
            vec![svc("web", "w", None)],
            vec![cmd("build", "b", "echo b")],
        );
        // In Services tab, 'w' should toggle the service
        app.handle_char('w');
        assert_ne!(app.services[0].status, ServiceStatus::Stopped);

        // Switch to Commands tab, 'b' should run the command
        app.tab = Tab::Commands;
        app.handle_char('b');
        assert_eq!(app.commands[0].status, CommandStatus::Running);
    }
}

// ===========================================================================
// Tests for planned features — reference methods/fields that don't exist yet.
// These won't compile until the corresponding implementations are added.
// Remove the #[cfg(feature = "__planned")] gate after implementing.
// ===========================================================================
#[cfg(test)]
mod planned_api_tests {
    use super::*;
    use crate::config::*;
    use std::path::PathBuf;

    fn svc(name: &str, key: &str) -> ServiceConfig {
        ServiceConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
            service_type: "generic".to_string(),
            port: None,
            url: None,
            depends_on: vec![],
        }
    }

    fn cmd(name: &str, key: &str) -> CommandConfig {
        CommandConfig {
            name: name.to_string(),
            key: key.to_string(),
            command: format!("echo {}", name),
            working_dir: "./".to_string(),
        }
    }

    fn app_with(services: Vec<ServiceConfig>, commands: Vec<CommandConfig>) -> App {
        let config = Config {
            general: General { project_root: "./".to_string() },
            services,
            commands,
            links: vec![],
            copies: vec![],
        };
        App::new(config, PathBuf::from("/tmp"), PathBuf::from("/tmp/devc.toml"), None)
    }

    fn empty_app() -> App {
        app_with(vec![], vec![])
    }

    // ===== Issue #15: prev_tab =====

    #[test]
    fn prev_tab_cycles_backward() {
        let mut app = empty_app();
        assert_eq!(app.tab, Tab::Services);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Commands);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Services);
    }

    #[test]
    fn prev_tab_is_inverse_of_next_tab() {
        let mut app = empty_app();
        app.next_tab();
        app.prev_tab();
        assert_eq!(app.tab, Tab::Services);

        app.next_tab();
        app.next_tab();
        app.prev_tab();
        assert_eq!(app.tab, Tab::Commands);
    }

    // ===== Issue #17: scroll methods =====

    #[test]
    fn scroll_up_increases_offset() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        let svc_id = app.services[0].id;
        // Add log lines so scroll has room
        for i in 0..20 {
            let _ = app.log_sender.send((LogSource::Service(svc_id), format!("line {}", i)));
        }
        app.poll_logs();
        assert_eq!(app.log_scroll_offset, 0);
        app.scroll_up(5);
        assert_eq!(app.log_scroll_offset, 5);
        app.scroll_up(3);
        assert_eq!(app.log_scroll_offset, 8);
    }

    #[test]
    fn scroll_down_decreases_offset() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.log_scroll_offset = 10;
        app.scroll_down(3);
        assert_eq!(app.log_scroll_offset, 7);
    }

    #[test]
    fn scroll_down_saturates_at_zero() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.log_scroll_offset = 2;
        app.scroll_down(10);
        assert_eq!(app.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_resets() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.log_scroll_offset = 50;
        app.scroll_to_bottom();
        assert_eq!(app.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_up_commands_tab_uses_cmd_offset() {
        let mut app = app_with(vec![], vec![cmd("build", "b")]);
        let cmd_id = app.commands[0].id;
        // Add log lines so scroll has room
        for i in 0..20 {
            let _ = app.log_sender.send((LogSource::Command(cmd_id), format!("line {}", i)));
        }
        app.poll_logs();
        app.tab = Tab::Commands;
        app.scroll_up(5);
        assert_eq!(app.cmd_log_scroll_offset, 5);
        assert_eq!(app.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_on_tools_tab_is_noop() {
        let mut app = empty_app();
        app.tab = Tab::Tools;
        app.scroll_up(10);
        assert_eq!(app.log_scroll_offset, 0);
        assert_eq!(app.cmd_log_scroll_offset, 0);
    }

    #[test]
    fn select_down_resets_scroll() {
        let mut app = app_with(vec![svc("a", "a"), svc("b", "b")], vec![]);
        app.log_scroll_offset = 20;
        app.select_down();
        assert_eq!(app.selected, 1);
        assert_eq!(app.log_scroll_offset, 0);
    }

    #[test]
    fn select_up_resets_scroll() {
        let mut app = app_with(vec![svc("a", "a"), svc("b", "b")], vec![]);
        app.selected = 1;
        app.log_scroll_offset = 15;
        app.select_up();
        assert_eq!(app.selected, 0);
        assert_eq!(app.log_scroll_offset, 0);
    }

    #[test]
    fn select_up_at_zero_keeps_scroll() {
        let mut app = app_with(vec![svc("a", "a")], vec![]);
        app.log_scroll_offset = 10;
        app.select_up(); // already at 0, no selection change
        assert_eq!(app.log_scroll_offset, 10);
    }

    // ===== HMR: apply_config reconcile =====

    fn link(name: &str, key: &str, url: &str) -> LinkConfig {
        LinkConfig {
            name: name.to_string(),
            key: key.to_string(),
            url: url.to_string(),
        }
    }

    fn copy(name: &str, key: &str, text: &str) -> CopyConfig {
        CopyConfig {
            name: name.to_string(),
            key: key.to_string(),
            text: text.to_string(),
        }
    }

    fn config_with(
        services: Vec<ServiceConfig>,
        commands: Vec<CommandConfig>,
        links: Vec<LinkConfig>,
        copies: Vec<CopyConfig>,
    ) -> Config {
        Config {
            general: General { project_root: "./".to_string() },
            services,
            commands,
            links,
            copies,
        }
    }

    #[test]
    fn apply_config_preserves_index_for_kept_service() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        let new = config_with(vec![svc("API", "a"), svc("Web", "w")], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.services.len(), 2);
        assert_eq!(app.services[0].config.name, "API");
        assert_eq!(app.services[1].config.name, "Web");
        assert_eq!(report.services_added, 0);
        assert_eq!(report.services_pending_restart, 0);
    }

    #[test]
    fn apply_config_keeps_running_service_removed_from_config_as_orphan() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        // Mark Web as running (without spawning a real process)
        app.services[1].status = ServiceStatus::Running;
        let new = config_with(vec![svc("API", "a")], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.services.len(), 2, "running orphan must stay in place");
        assert_eq!(app.services[1].config.name, "Web");
        assert_eq!(report.services_orphaned, 1);
    }

    #[test]
    fn apply_config_marks_running_service_dirty_on_command_change() {
        let mut app = app_with(vec![svc("API", "a")], vec![]);
        app.services[0].status = ServiceStatus::Running;
        let mut new_svc = svc("API", "a");
        new_svc.command = "echo NEW".to_string();
        let new = config_with(vec![new_svc], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert!(app.services[0].config_dirty);
        assert_eq!(app.services[0].config.command, "echo NEW");
        assert_eq!(report.services_pending_restart, 1);
    }

    #[test]
    fn apply_config_stopped_service_update_is_silent_not_dirty() {
        let mut app = app_with(vec![svc("API", "a")], vec![]);
        // status is Stopped by default
        let mut new_svc = svc("API", "a");
        new_svc.command = "echo NEW".to_string();
        let new = config_with(vec![new_svc], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert!(!app.services[0].config_dirty);
        assert_eq!(app.services[0].config.command, "echo NEW");
        assert_eq!(report.services_pending_restart, 0);
    }

    #[test]
    fn apply_config_appends_new_service_to_tail() {
        let mut app = app_with(vec![svc("API", "a")], vec![]);
        let new = config_with(vec![svc("API", "a"), svc("Web", "w")], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.services.len(), 2);
        assert_eq!(app.services[1].config.name, "Web");
        assert_eq!(report.services_added, 1);
    }

    #[test]
    fn apply_config_running_service_log_routing_index_unchanged() {
        // Old: [API=0(running), Web=1(stopped)]
        // New config drops Web and adds Worker → API is still found by its stable ID
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        let api_id = app.services[0].id;
        app.services[0].status = ServiceStatus::Running;
        let new = config_with(vec![svc("API", "a"), svc("Worker", "k")], vec![], vec![], vec![]);
        app.apply_config(new);
        // API must still be findable by its stable typed ID
        assert_eq!(app.services[0].config.name, "API");
        // Send a log with API's typed ID and verify it lands in API's buffer
        let _ = app.log_sender.send((LogSource::Service(api_id), "API log line".to_string()));
        app.poll_logs();
        assert!(app.services[0].logs.iter().any(|l| l.contains("API log line")));
    }

    #[test]
    fn apply_config_rebuilds_tools_entirely() {
        let mut app = app_with(vec![], vec![]);
        let new = config_with(
            vec![],
            vec![],
            vec![link("Docs", "d", "https://docs")],
            vec![copy("Token", "t", "abc")],
        );
        app.apply_config(new);
        assert_eq!(app.tools.len(), 2);
    }

    #[test]
    fn apply_config_clamps_tools_selected_when_tools_shrink() {
        let mut app = app_with(vec![], vec![]);
        // Pre-populate tools so we can shrink
        let pre = config_with(
            vec![],
            vec![],
            vec![link("A", "a", "u1"), link("B", "b", "u2"), link("C", "c", "u3")],
            vec![],
        );
        app.apply_config(pre);
        app.tools_selected = 2;
        let new = config_with(vec![], vec![], vec![link("A", "a", "u1")], vec![]);
        app.apply_config(new);
        assert_eq!(app.tools.len(), 1);
        assert_eq!(app.tools_selected, 0);
    }

    #[test]
    fn apply_config_reports_project_root_change_without_mutating() {
        let mut app = app_with(vec![], vec![]);
        let original_root = app.project_root.clone();
        let mut new = config_with(vec![], vec![], vec![], vec![]);
        new.general.project_root = "./other".to_string();
        let report = app.apply_config(new);
        assert!(report.project_root_changed);
        assert_eq!(app.project_root, original_root, "must not mutate live root");
    }

    #[test]
    fn apply_config_detects_key_conflicts_into_report() {
        let mut app = app_with(vec![], vec![]);
        let new = config_with(
            vec![svc("A", "x"), svc("B", "x")],  // duplicate 'x', also reserved
            vec![],
            vec![],
            vec![],
        );
        let report = app.apply_config(new);
        assert!(!report.key_conflicts.is_empty(), "expected conflicts: {:?}", report.key_conflicts);
    }

    #[test]
    fn apply_config_commands_reconcile_same_semantics_as_services() {
        let mut app = app_with(vec![], vec![cmd("build", "b")]);
        app.commands[0].status = CommandStatus::Running;
        let mut changed = cmd("build", "b");
        changed.command = "make".to_string();
        let new = config_with(vec![], vec![changed, cmd("test", "t")], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.commands.len(), 2);
        assert_eq!(app.commands[1].config.name, "test");
        assert!(app.commands[0].config_dirty);
        assert_eq!(app.commands[0].config.command, "make");
        assert_eq!(report.commands_added, 1);
        assert_eq!(report.commands_pending_restart, 1);
    }

    #[test]
    fn apply_config_empty_new_config_does_not_crash_running_app() {
        let mut app = app_with(vec![svc("API", "a")], vec![cmd("build", "b")]);
        app.services[0].status = ServiceStatus::Running;
        app.commands[0].status = CommandStatus::Running;
        let new = config_with(vec![], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        // Running entries become orphans (kept in place)
        assert_eq!(app.services.len(), 1);
        assert_eq!(app.commands.len(), 1);
        assert_eq!(report.services_orphaned, 1);
        assert_eq!(report.commands_orphaned, 1);
        assert!(app.services[0].config_dirty);
        assert!(app.commands[0].config_dirty);
    }

    // ===== HMR refinements: drop semantics + command reset + start clears dirty =====

    #[test]
    fn apply_config_drops_stopped_service_when_removed_from_tail() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        // Web at idx 1 is Stopped, removing from config should drop it.
        let new = config_with(vec![svc("API", "a")], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.services.len(), 1);
        assert_eq!(app.services[0].config.name, "API");
        assert_eq!(report.services_dropped, 1);
        assert_eq!(report.services_orphaned, 0);
    }

    #[test]
    fn apply_config_keeps_stopped_service_in_middle_when_blocked_by_running_tail() {
        // [API(stopped), Web(running)] → remove API. Can't compact (Web's idx must stay 1).
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[1].status = ServiceStatus::Running;
        let new = config_with(vec![svc("Web", "w")], vec![], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.services.len(), 2, "API tombstone stays to preserve Web's idx");
        assert_eq!(app.services[1].config.name, "Web");
        assert_eq!(report.services_dropped, 0);
    }

    #[test]
    fn apply_config_marks_orphan_service_dirty() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[0].status = ServiceStatus::Running;
        let new = config_with(vec![svc("Web", "w")], vec![], vec![], vec![]);
        app.apply_config(new);
        assert!(app.services[0].config_dirty, "running orphan must show reload badge");
    }

    #[test]
    fn apply_config_resets_idle_command_when_replaced() {
        // Command that's been run (Done state, logs present) gets fully replaced.
        let mut app = app_with(vec![], vec![cmd("build", "b")]);
        app.commands[0].status = CommandStatus::Done;
        app.commands[0].logs.push_back("old log line".to_string());
        let mut changed = cmd("build", "b");
        changed.command = "make".to_string();
        let new = config_with(vec![], vec![changed], vec![], vec![]);
        app.apply_config(new);
        assert_eq!(app.commands[0].status, CommandStatus::Idle, "stopped command resets to Idle");
        assert!(app.commands[0].logs.is_empty(), "logs cleared on replace");
        assert!(!app.commands[0].config_dirty);
        assert_eq!(app.commands[0].config.command, "make");
    }

    #[test]
    fn apply_config_marks_running_command_dirty_when_changed() {
        let mut app = app_with(vec![], vec![cmd("build", "b")]);
        app.commands[0].status = CommandStatus::Running;
        app.commands[0].logs.push_back("running output".to_string());
        let mut changed = cmd("build", "b");
        changed.command = "make".to_string();
        let new = config_with(vec![], vec![changed], vec![], vec![]);
        let report = app.apply_config(new);
        assert!(app.commands[0].config_dirty);
        assert_eq!(report.commands_pending_restart, 1);
        assert!(!app.commands[0].logs.is_empty(), "running command logs preserved");
    }

    #[test]
    fn apply_config_drops_stopped_command_from_tail() {
        let mut app = app_with(vec![], vec![cmd("build", "b"), cmd("test", "t")]);
        let new = config_with(vec![], vec![cmd("build", "b")], vec![], vec![]);
        let report = app.apply_config(new);
        assert_eq!(app.commands.len(), 1);
        assert_eq!(report.commands_dropped, 1);
    }

    #[test]
    fn start_service_clears_config_dirty() {
        let mut app = app_with(vec![svc("API", "a")], vec![]);
        app.services[0].config_dirty = true;
        // start_service() spawns a real process; just call the path that flips status + clears dirty.
        // We exercise the same code by calling start_service through toggle_service.
        // To avoid spawn flakiness, set port_active=true → start_service short-circuits, but
        // doesn't clear dirty. Use a real spawnable command instead: "true" exits immediately.
        app.services[0].config.command = "true".to_string();
        app.services[0].config.working_dir = ".".to_string();
        app.toggle_service(0);
        assert!(!app.services[0].config_dirty);
    }

    #[test]
    fn run_command_clears_config_dirty() {
        let mut app = app_with(vec![], vec![cmd("c", "c")]);
        app.commands[0].config_dirty = true;
        app.commands[0].config.command = "true".to_string();
        app.commands[0].config.working_dir = ".".to_string();
        app.run_command(0);
        assert!(!app.commands[0].config_dirty);
    }

    // ===== Orphan flag + auto-compact =====

    #[test]
    fn apply_config_sets_orphan_flag_for_running_removed_service() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[1].status = ServiceStatus::Running;
        let new = config_with(vec![svc("API", "a")], vec![], vec![], vec![]);
        app.apply_config(new);
        assert!(app.services[1].orphan, "removed running service must be flagged orphan");
        assert!(app.services[1].config_dirty);
    }

    #[test]
    fn apply_config_clears_orphan_when_service_returns_to_config() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[1].status = ServiceStatus::Running;
        // First reload: Web removed → orphan
        app.apply_config(config_with(vec![svc("API", "a")], vec![], vec![], vec![]));
        assert!(app.services[1].orphan);
        // Second reload: Web returns
        app.apply_config(config_with(vec![svc("API", "a"), svc("Web", "w")], vec![], vec![], vec![]));
        assert!(!app.services[1].orphan, "orphan flag clears when service returns to config");
    }

    #[test]
    fn compact_stopped_orphans_drops_stopped_orphan_from_tail() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[1].status = ServiceStatus::Running;
        // Mark Web as orphan via reload
        app.apply_config(config_with(vec![svc("API", "a")], vec![], vec![], vec![]));
        assert_eq!(app.services.len(), 2);
        // User stops Web (simulate)
        app.services[1].status = ServiceStatus::Stopped;
        app.services[1].process = None;
        app.compact_stopped_orphans();
        assert_eq!(app.services.len(), 1, "stopped orphan should auto-drop from tail");
        assert_eq!(app.services[0].config.name, "API");
    }

    #[test]
    fn compact_stopped_orphans_does_not_drop_if_orphan_still_running() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        app.services[1].status = ServiceStatus::Running;
        app.apply_config(config_with(vec![svc("API", "a")], vec![], vec![], vec![]));
        // Web still Running; compact should be a no-op
        app.compact_stopped_orphans();
        assert_eq!(app.services.len(), 2);
    }

    #[test]
    fn compact_stopped_orphans_does_not_drop_non_orphan_stopped() {
        let mut app = app_with(vec![svc("API", "a"), svc("Web", "w")], vec![]);
        // Web is stopped but NOT an orphan (still in config)
        app.compact_stopped_orphans();
        assert_eq!(app.services.len(), 2, "stopped non-orphan services stay");
    }

    #[test]
    fn compact_stopped_orphans_handles_commands_too() {
        let mut app = app_with(vec![], vec![cmd("a", "a"), cmd("b", "b")]);
        app.commands[1].status = CommandStatus::Running;
        app.apply_config(config_with(vec![], vec![cmd("a", "a")], vec![], vec![]));
        assert_eq!(app.commands.len(), 2);
        assert!(app.commands[1].orphan);
        // Command finishes
        app.commands[1].status = CommandStatus::Done;
        app.commands[1].process = None;
        app.compact_stopped_orphans();
        assert_eq!(app.commands.len(), 1);
    }
}
