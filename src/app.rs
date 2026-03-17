use std::collections::VecDeque;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::config::ServiceConfig;
use crate::process::ProcessHandle;

#[derive(Clone, Copy, PartialEq, Eq)]
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Services = 0,
    Tools = 1,
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

pub struct App {
    pub services: Vec<ServiceState>,
    pub selected: usize,
    pub tab: Tab,
    pub tools: Vec<ToolItem>,
    pub tools_selected: usize,
    pub status: Option<(String, Instant)>,
    pub tick: u64,
    log_receiver: mpsc::Receiver<(usize, String)>,
    log_sender: mpsc::Sender<(usize, String)>,
    project_root: PathBuf,
}

impl App {
    pub fn new(config: Config, config_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let project_root = config_dir.join(&config.general.project_root);

        let services = config
            .services
            .into_iter()
            .map(|cfg| ServiceState {
                config: cfg,
                process: None,
                status: ServiceStatus::Stopped,
                port_active: false,
                stopping_since: None,
                logs: VecDeque::with_capacity(500),
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

        Self {
            services,
            selected: 0,
            tab: Tab::Services,
            tools,
            tools_selected: 0,
            status: None,
            tick: 0,
            log_receiver: rx,
            log_sender: tx,
            project_root,
        }
    }

    // --- Tab ---

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Services => Tab::Tools,
            Tab::Tools => Tab::Services,
        };
    }

    // --- Navigation ---

    pub fn select_up(&mut self) {
        match self.tab {
            Tab::Services => self.selected = self.selected.saturating_sub(1),
            Tab::Tools => self.tools_selected = self.tools_selected.saturating_sub(1),
        }
    }

    pub fn select_down(&mut self) {
        match self.tab {
            Tab::Services => {
                if self.selected + 1 < self.services.len() {
                    self.selected += 1;
                }
            }
            Tab::Tools => {
                if self.tools_selected + 1 < self.tools.len() {
                    self.tools_selected += 1;
                }
            }
        }
    }

    pub fn activate_selected(&mut self) {
        match self.tab {
            Tab::Services => {
                let idx = self.selected;
                self.toggle_service(idx);
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
            // Start: ensure dependencies are running first
            let deps: Vec<String> = self.services[idx].config.depends_on.clone();
            for dep_name in &deps {
                if let Some(dep_idx) = self.find_service_by_name(dep_name) {
                    if self.services[dep_idx].status == ServiceStatus::Stopped {
                        self.start_service(dep_idx);
                    }
                }
            }
            self.start_service(idx);
        }
    }

    fn start_service(&mut self, idx: usize) {
        let service = &mut self.services[idx];

        if service.port_active {
            service.logs.push_back(format!(
                "── port {} already in use ──",
                service.config.port.unwrap()
            ));
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
        ) {
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
        match Command::new("open").arg(&url).spawn() {
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
                match Command::new("open").arg(&url).spawn() {
                    Ok(_) => self.set_status(format!("Opened: {}", url)),
                    Err(e) => self.set_status(format!("Error: {}", e)),
                }
            }
            ToolKind::Copy(text) => {
                let text = text.clone();
                let name = tool.name.clone();
                match Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
                    Ok(mut child) => {
                        if let Some(ref mut stdin) = child.stdin {
                            let _ = stdin.write_all(text.as_bytes());
                        }
                        let _ = child.wait();
                        self.set_status(format!("Copied: {}", name));
                    }
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
            if ts.elapsed() > std::time::Duration::from_secs(3) {
                self.status = None;
            }
        }
    }

    // --- Tick ---

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn poll_logs(&mut self) {
        while let Ok((idx, line)) = self.log_receiver.try_recv() {
            if let Some(service) = self.services.get_mut(idx) {
                service.logs.push_back(line);
                if service.logs.len() > 500 {
                    service.logs.pop_front();
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
                            if since.elapsed() > Duration::from_millis(500) {
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

    pub fn check_ports(&mut self) {
        // Check on first tick, then every ~2 seconds
        if self.tick % 20 != 1 {
            return;
        }
        for service in &mut self.services {
            if let Some(port) = service.config.port {
                let timeout = Duration::from_millis(50);
                let addrs: [SocketAddr; 2] = [
                    format!("127.0.0.1:{}", port).parse().unwrap(),
                    format!("[::1]:{}", port).parse().unwrap(),
                ];
                let active = addrs
                    .iter()
                    .any(|addr| TcpStream::connect_timeout(addr, timeout).is_ok());
                service.port_active = active;
            }
        }
    }

    pub fn cleanup(&mut self) {
        for service in &mut self.services {
            if let Some(mut proc) = service.process.take() {
                proc.kill();
            }
        }
    }
}
