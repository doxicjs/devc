use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Max lines kept per service/command log buffer (oldest evicted first).
const LOG_CAPACITY: usize = 500;
/// How often to check ports, in ticks. At 100ms/tick this is ~2 seconds.
const PORT_CHECK_INTERVAL: u64 = 20;
/// Seconds to wait after SIGTERM before escalating to SIGKILL.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);
/// How long status messages stay visible.
const STATUS_TTL: Duration = Duration::from_secs(3);

use crate::config::Config;
use crate::config::ServiceConfig;
use crate::config::CommandConfig;
use crate::process::ProcessHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
}

pub struct ServiceState {
    pub config: ServiceConfig,
    pub process: Option<ProcessHandle>,
    pub status: ServiceStatus,
    pub port_active: bool,
    pub stopping_since: Option<Instant>,
    pub logs: VecDeque<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandStatus {
    Idle,
    Running,
    Done,
    Failed,
}

pub struct CommandState {
    pub config: CommandConfig,
    pub process: Option<ProcessHandle>,
    pub status: CommandStatus,
    pub logs: VecDeque<String>,
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

/// Log messages are tagged with a source: Service(idx) or Command(idx)
pub enum LogSource {
    Service(usize),
    Command(usize),
}

pub struct App {
    pub services: Vec<ServiceState>,
    pub commands: Vec<CommandState>,
    pub commands_selected: usize,
    pub selected: usize,
    pub tab: Tab,
    pub tools: Vec<ToolItem>,
    pub tools_selected: usize,
    pub status: Option<(String, Instant)>,
    pub tick: u64,
    log_receiver: mpsc::Receiver<(LogSource, String)>,
    log_sender: mpsc::Sender<(LogSource, String)>,
    port_sender: mpsc::Sender<(usize, bool)>,
    port_receiver: mpsc::Receiver<(usize, bool)>,
    project_root: PathBuf,
    pub log_scroll_offset: usize,
    pub cmd_log_scroll_offset: usize,
}

impl App {
    pub fn new(config: Config, config_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let (port_tx, port_rx) = mpsc::channel();
        let project_root = config_dir.join(&config.general.project_root);

        let services: Vec<ServiceState> = config
            .services
            .into_iter()
            .map(|cfg| ServiceState {
                config: cfg,
                process: None,
                status: ServiceStatus::Stopped,
                port_active: false,
                stopping_since: None,
                logs: VecDeque::with_capacity(LOG_CAPACITY),
            })
            .collect();

        let commands: Vec<CommandState> = config
            .commands
            .into_iter()
            .map(|cfg| CommandState {
                config: cfg,
                process: None,
                status: CommandStatus::Idle,
                logs: VecDeque::with_capacity(LOG_CAPACITY),
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

        {
            let reserved = ['a', 'x'];
            let mut seen = Vec::<char>::new();
            for s in services.iter() {
                let k = s.config.key_char().to_ascii_lowercase();
                if k == 'q' {
                    eprintln!("warning: service '{}' key '{}' conflicts with quit", s.config.name, k);
                }
                if reserved.contains(&k) {
                    eprintln!("warning: service '{}' key '{}' conflicts with reserved shortcut", s.config.name, k);
                }
                if seen.contains(&k) {
                    eprintln!("warning: duplicate service key '{}'", k);
                } else {
                    seen.push(k);
                }
            }
            seen.clear();
            for c in commands.iter() {
                let k = c.config.key_char().to_ascii_lowercase();
                if k == 'q' {
                    eprintln!("warning: command '{}' key '{}' conflicts with quit", c.config.name, k);
                }
                if seen.contains(&k) {
                    eprintln!("warning: duplicate command key '{}'", k);
                } else {
                    seen.push(k);
                }
            }
            seen.clear();
            for t in tools.iter() {
                let k = t.key.to_ascii_lowercase();
                if k == 'q' {
                    eprintln!("warning: tool '{}' key '{}' conflicts with quit", t.name, k);
                }
                if seen.contains(&k) {
                    eprintln!("warning: duplicate tool key '{}'", k);
                } else {
                    seen.push(k);
                }
            }
        }

        Self {
            services,
            commands,
            commands_selected: 0,
            selected: 0,
            tab: Tab::Services,
            tools,
            tools_selected: 0,
            status: None,
            tick: 0,
            log_receiver: rx,
            log_sender: tx,
            port_sender: port_tx,
            port_receiver: port_rx,
            project_root,
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

        let working_dir = self.project_root.join(&service.config.working_dir);
        let cmd = service.config.full_command();
        service
            .logs
            .push_back(format!("── starting: {} ──", cmd));

        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            self.log_sender.clone(),
            idx,
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
            self.set_status("No URL for this service".to_string());
            return;
        };
        match crate::platform::open_url(&url) {
            Ok(_) => self.set_status(format!("Opened: {}", url)),
            Err(e) => self.set_status(format!("Error: {}", e)),
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

        let working_dir = self.project_root.join(&cmd_state.config.working_dir);
        let cmd = cmd_state.config.command.clone();
        cmd_state
            .logs
            .push_back(format!("── running: {} ──", cmd));

        let sender = self.log_sender.clone();
        let cmd_idx = idx;

        match ProcessHandle::spawn_tagged(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            sender,
            cmd_idx,
            true,
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
                    Ok(_) => self.set_status(format!("Opened: {}", url)),
                    Err(e) => self.set_status(format!("Error: {}", e)),
                }
            }
            ToolKind::Copy(text) => {
                let text = text.clone();
                let name = tool.name.clone();
                match crate::platform::copy_to_clipboard(&text) {
                    Ok(_) => self.set_status(format!("Copied: {}", name)),
                    Err(e) => self.set_status(format!("Error: {}", e)),
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

    // --- Status ---

    fn set_status(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    pub fn clear_old_status(&mut self) {
        if let Some((_, ts)) = &self.status {
            if ts.elapsed() > STATUS_TTL {
                self.status = None;
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
                LogSource::Service(idx) => {
                    if let Some(service) = self.services.get_mut(idx) {
                        service.logs.push_back(line);
                        if service.logs.len() > LOG_CAPACITY {
                            service.logs.pop_front();
                        }
                    }
                }
                LogSource::Command(idx) => {
                    if let Some(cmd) = self.commands.get_mut(idx) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
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
        App::new(config, PathBuf::from("/tmp"))
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
    fn clear_old_status_removes_expired() {
        let mut app = empty_app();
        app.status = Some(("test".to_string(), Instant::now() - Duration::from_secs(4)));
        app.clear_old_status();
        assert!(app.status.is_none());
    }

    #[test]
    fn clear_old_status_keeps_recent() {
        let mut app = empty_app();
        app.status = Some(("test".to_string(), Instant::now()));
        app.clear_old_status();
        assert!(app.status.is_some());
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
        let _ = app.log_sender.send((LogSource::Service(999), "ghost".to_string()));
        app.poll_logs();
    }

    #[test]
    fn poll_logs_invalid_command_index_no_panic() {
        let mut app = empty_app();
        let _ = app.log_sender.send((LogSource::Command(999), "ghost".to_string()));
        app.poll_logs();
    }

    // ===== Log capping at 500 =====

    #[test]
    fn service_logs_capped_at_500() {
        let mut app = app_with(vec![svc("web", "w", None)], vec![]);
        for i in 0..600 {
            let _ = app.log_sender.send((LogSource::Service(0), format!("line {}", i)));
        }
        app.poll_logs();
        assert_eq!(app.services[0].logs.len(), 500);
        // Oldest 100 lines evicted (0..99), first remaining is "line 100"
        assert!(app.services[0].logs.front().unwrap().contains("100"));
    }

    #[test]
    fn command_logs_capped_at_500() {
        let mut app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        for i in 0..600 {
            let _ = app.log_sender.send((LogSource::Command(0), format!("line {}", i)));
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
        // Add 10 log lines
        for i in 0..10 {
            let _ = app.log_sender.send((LogSource::Service(0), format!("line {}", i)));
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
        App::new(config, PathBuf::from("/tmp"))
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
        // Add log lines so scroll has room
        for i in 0..20 {
            let _ = app.log_sender.send((LogSource::Service(0), format!("line {}", i)));
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
        // Add log lines so scroll has room
        for i in 0..20 {
            let _ = app.log_sender.send((LogSource::Command(0), format!("line {}", i)));
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
}
