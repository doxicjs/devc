use std::path::PathBuf;

use crate::commands::CommandsPane;
use crate::config::Config;
use crate::config_watcher::{ConfigWatcher, WatchEvent};
use crate::id::{CommandId, ServiceId};
use crate::port_monitor::PortMonitor;
use crate::services::ServicesPane;
use crate::status::StatusBar;
use crate::tools::ToolsPane;

/// Log messages are tagged with a source: Service(id) or Command(id)
pub enum LogSource {
    Service(ServiceId),
    Command(CommandId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Services = 0,
    Commands = 1,
    Tools = 2,
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
    pub services: ServicesPane,
    pub commands: CommandsPane,
    pub tools: ToolsPane,
    pub config_watcher: ConfigWatcher,
    pub port_monitor: PortMonitor,
    pub status: StatusBar,
    pub tab: Tab,
    pub tick: u64,
    pub project_root: PathBuf,
    pub config_dir: PathBuf,
}

impl App {
    pub fn new(
        config: Config,
        config_dir: PathBuf,
        config_path: PathBuf,
        local_config_path: Option<PathBuf>,
    ) -> Self {
        let project_root = config_dir.join(&config.general.project_root);

        let services = ServicesPane::from_config(config.services);
        let commands = CommandsPane::from_config(config.commands);
        let tools = ToolsPane::from_config(config.links, config.copies);

        for warning in crate::keys::detect_conflicts(services.items(), commands.items(), tools.items()) {
            eprintln!("warning: {}", warning);
        }

        Self {
            services,
            commands,
            tools,
            config_watcher: ConfigWatcher::new(config_path, local_config_path),
            port_monitor: PortMonitor::new(),
            status: StatusBar::new(),
            tab: Tab::Services,
            tick: 0,
            project_root,
            config_dir,
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
            Tab::Services => self.services.select_up(),
            Tab::Commands => self.commands.select_up(),
            Tab::Tools => self.tools.select_up(),
        }
    }

    pub fn select_down(&mut self) {
        match self.tab {
            Tab::Services => self.services.select_down(),
            Tab::Commands => self.commands.select_down(),
            Tab::Tools => self.tools.select_down(),
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        match self.tab {
            Tab::Services => self.services.scroll_up(amount),
            Tab::Commands => self.commands.scroll_up(amount),
            Tab::Tools => {}
        }
    }

    pub fn scroll_down(&mut self, amount: usize) {
        match self.tab {
            Tab::Services => self.services.scroll_down(amount),
            Tab::Commands => self.commands.scroll_down(amount),
            Tab::Tools => {}
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        match self.tab {
            Tab::Services => self.services.scroll_to_bottom(),
            Tab::Commands => self.commands.scroll_to_bottom(),
            Tab::Tools => {}
        }
    }

    pub fn activate_selected(&mut self) {
        match self.tab {
            Tab::Services => {
                self.services.toggle_selected(&self.project_root);
            }
            Tab::Commands => {
                let idx = self.commands.selected_idx();
                self.commands.run(idx, &self.project_root);
            }
            Tab::Tools => {
                let idx = self.tools.selected_idx();
                match self.tools.activate(idx) {
                    Ok(msg) => self.status.set(msg),
                    Err(msg) => self.status.set(msg),
                }
            }
        }
    }

    pub fn handle_char(&mut self, c: char) {
        match self.tab {
            Tab::Services => match c {
                'a' => self.services.start_all(&self.project_root),
                'x' => self.services.stop_all(),
                _ => {
                    if crate::keys::is_services_reserved(c) { return; }
                    if let Some(idx) = self.services.find_by_key(c) {
                        self.services.toggle(idx, &self.project_root);
                    }
                }
            },
            Tab::Commands => {
                if let Some(idx) = self.commands.find_by_key(c) {
                    self.commands.run(idx, &self.project_root);
                }
            }
            Tab::Tools => {
                if let Some(idx) = self.tools.find_by_key(c) {
                    match self.tools.activate(idx) {
                        Ok(msg) => self.status.set(msg),
                        Err(msg) => self.status.set(msg),
                    }
                }
            }
        }
    }

    // --- Tick ---

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn poll_logs(&mut self) {
        self.services.poll_logs();
        self.commands.poll_logs();
    }

    pub fn check_processes(&mut self) {
        self.services.check_processes();
        self.commands.check_processes();
    }

    pub fn check_ports(&mut self) {
        self.services.apply_ports(&self.port_monitor.drain());
        if !self.port_monitor.should_check(self.tick) {
            return;
        }
        self.port_monitor.kick(self.services.port_targets());
    }

    pub fn cleanup(&mut self) {
        self.services.cleanup();
        self.commands.cleanup();
    }

    /// Tail-compact services and commands whose `orphan` flag is set and that have
    /// fully stopped (no process, status Stopped/Idle/Done/Failed). Called every tick
    /// so an orphan disappears as soon as the user stops it — no config-edit nudge needed.
    pub fn compact_stopped_orphans(&mut self) {
        self.services.compact_stopped_orphans();
        self.commands.compact_stopped_orphans();
    }

    pub fn poll(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.check_config_reload();
        self.services.compact_stopped_orphans();
        self.commands.compact_stopped_orphans();
        self.services.poll_logs();
        self.commands.poll_logs();
        self.services.check_processes();
        self.commands.check_processes();
        // Port monitoring
        self.services.apply_ports(&self.port_monitor.drain());
        if self.port_monitor.should_check(self.tick) {
            self.port_monitor.kick(self.services.port_targets());
        }
        self.status.clear_if_expired();
    }

    pub fn check_config_reload(&mut self) {
        match self.config_watcher.poll() {
            WatchEvent::Idle => {}
            WatchEvent::Reloaded(new_cfg) => {
                let report = self.apply_config(new_cfg);
                self.status.set(report.summary());
            }
            WatchEvent::Error(msg) | WatchEvent::Notice(msg) => self.status.set(msg),
        }
    }

    pub fn apply_config(&mut self, new: Config) -> ReloadReport {
        let mut report = ReloadReport::default();

        // ----- Services -----
        let svc_delta = self.services.apply_config(&new.services);
        report.services_added = svc_delta.added;
        report.services_dropped = svc_delta.dropped;
        report.services_pending_restart = svc_delta.pending_restart;
        report.services_orphaned = svc_delta.orphaned;

        // ----- Commands -----
        let cmd_delta = self.commands.apply_config(&new.commands);
        report.commands_added = cmd_delta.added;
        report.commands_dropped = cmd_delta.dropped;
        report.commands_pending_restart = cmd_delta.pending_restart;
        report.commands_orphaned = cmd_delta.orphaned;

        // ----- Tools (full silent rebuild — no background threads) -----
        self.tools.rebuild(&new.links, &new.copies);

        // ----- Project root (no mutation) -----
        let new_root = self.config_dir.join(&new.general.project_root);
        if new_root != self.project_root {
            report.project_root_changed = true;
        }

        // ----- Key conflicts -----
        report.key_conflicts = crate::keys::detect_conflicts(
            self.services.items(), self.commands.items(), self.tools.items(),
        );

        report
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandStatus;
    use crate::config::*;
    use crate::services::ServiceStatus;
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
        assert_eq!(app.services.selected_idx(), 0);
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
        assert_eq!(app.services.selected_idx(), 0);
        app.select_up();
        assert_eq!(app.services.selected_idx(), 0);
    }

    #[test]
    fn select_down_advances() {
        let mut app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        app.select_down();
        assert_eq!(app.services.selected_idx(), 1);
    }

    #[test]
    fn select_down_at_last_stays() {
        let mut app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        app.select_down();
        app.select_down();
        assert_eq!(app.services.selected_idx(), 1);
    }

    #[test]
    fn select_down_empty_services() {
        let mut app = empty_app();
        app.select_down();
        assert_eq!(app.services.selected_idx(), 0);
    }

    #[test]
    fn select_up_empty_services() {
        let mut app = empty_app();
        app.select_up();
        assert_eq!(app.services.selected_idx(), 0);
    }

    #[test]
    fn select_down_commands_tab() {
        let mut app = app_with(
            vec![],
            vec![cmd("a", "a", "echo a"), cmd("b", "b", "echo b")],
        );
        app.tab = Tab::Commands;
        app.select_down();
        assert_eq!(app.commands.selected_idx(), 1);
    }

    #[test]
    fn select_down_commands_empty() {
        let mut app = empty_app();
        app.tab = Tab::Commands;
        app.select_down();
        assert_eq!(app.commands.selected_idx(), 0);
    }

    // ===== Empty list safety =====

    #[test]
    fn toggle_service_empty_no_panic() {
        let mut app = empty_app();
        app.services.toggle(0, &app.project_root.clone());
    }

    #[test]
    fn toggle_service_out_of_bounds_no_panic() {
        let mut app = app_with(vec![svc("a", "a", None)], vec![]);
        app.services.toggle(99, &app.project_root.clone());
    }

    #[test]
    fn run_command_empty_no_panic() {
        let mut app = empty_app();
        app.commands.run(0, &app.project_root.clone());
    }

    #[test]
    fn run_command_out_of_bounds_no_panic() {
        let mut app = app_with(vec![], vec![cmd("a", "a", "echo a")]);
        app.commands.run(99, &app.project_root.clone());
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
        // toggle -> start_service -> unwraps port — should NOT panic
        let root = app.project_root.clone();
        app.services.toggle(0, &root);
    }

    // ===== Issue #4: cleanup must kill command processes =====

    #[test]
    fn cleanup_kills_command_processes() {
        let mut app = app_with(
            vec![],
            vec![cmd("sleeper", "s", "sleep 100")],
        );
        let root = app.project_root.clone();
        app.commands.run(0, &root);
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
        assert_eq!(app.services.find_by_key('w'), Some(0));
        assert_eq!(app.services.find_by_key('a'), Some(1));
    }

    #[test]
    fn find_service_by_key_case_insensitive() {
        let app = app_with(vec![svc("web", "w", None)], vec![]);
        assert_eq!(app.services.find_by_key('W'), Some(0));
    }

    #[test]
    fn find_service_by_key_no_match() {
        let app = app_with(vec![svc("web", "w", None)], vec![]);
        assert_eq!(app.services.find_by_key('z'), None);
    }

    #[test]
    fn find_service_by_key_empty() {
        let app = empty_app();
        assert_eq!(app.services.find_by_key('w'), None);
    }

    #[test]
    fn find_command_by_key_found() {
        let app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        assert_eq!(app.commands.find_by_key('b'), Some(0));
    }

    #[test]
    fn find_command_by_key_no_match() {
        let app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        assert_eq!(app.commands.find_by_key('z'), None);
    }

    // ===== Running count =====

    #[test]
    fn running_count_empty() {
        let app = empty_app();
        assert_eq!(app.services.running_count(), 0);
    }

    #[test]
    fn running_count_all_stopped() {
        let app = app_with(vec![svc("a", "a", None), svc("b", "b", None)], vec![]);
        assert_eq!(app.services.running_count(), 0);
    }

    // ===== handle_char: reserved key behavior (Issue #6) =====

    #[test]
    fn handle_char_a_triggers_start_all_not_service_key() {
        // Service keyed 'a' — pressing 'a' goes through start_all() path,
        // not the direct key-lookup path. This documents the conflict.
        let mut app = app_with(vec![svc("alpha", "a", None)], vec![]);
        app.handle_char('a');
        // start_all calls toggle for each stopped service, so the
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

    // ===== open_url =====

    #[test]
    fn open_service_url_out_of_bounds_no_panic() {
        let app = empty_app();
        // Should return Err, not panic
        let _ = app.services.open_url(0);
        let _ = app.services.open_url(99);
    }

    // ===== depends_on edge cases =====

    #[test]
    fn toggle_service_missing_dependency_no_panic() {
        let mut s = svc("web", "w", None);
        s.depends_on = vec!["nonexistent".to_string()];
        let mut app = app_with(vec![s], vec![]);
        let root = app.project_root.clone();
        app.services.toggle(0, &root);
    }

    // ===== poll_logs: invalid indices =====

    #[test]
    fn poll_logs_invalid_service_index_no_panic() {
        let mut app = empty_app();
        // ServicesPane owns its channel; send via the log_tx which is private.
        // We can't send directly, but poll_logs should be a no-op with empty channel.
        app.poll_logs();
    }

    #[test]
    fn poll_logs_invalid_command_index_no_panic() {
        let mut app = empty_app();
        app.poll_logs();
    }

    // ===== Log capping at 500 =====

    #[test]
    fn service_logs_capped_at_500() {
        let mut app = app_with(vec![svc("web", "w", None)], vec![]);
        // Start the service so we have a process; push logs directly into the buffer
        for i in 0..600 {
            app.services[0].logs.push_back(format!("line {}", i));
            if app.services[0].logs.len() > crate::services::LOG_CAPACITY {
                app.services[0].logs.pop_front();
            }
        }
        assert_eq!(app.services[0].logs.len(), 500);
        // Oldest 100 lines evicted (0..99), first remaining is "line 100"
        assert!(app.services[0].logs.front().unwrap().contains("100"));
    }

    #[test]
    fn command_logs_capped_at_500() {
        let mut app = app_with(vec![], vec![cmd("build", "b", "echo b")]);
        // CommandsPane owns its own channel; push directly to the log buffer.
        for i in 0..600 {
            app.commands[0].logs.push_back(format!("line {}", i));
            if app.commands[0].logs.len() > crate::commands::LOG_CAPACITY {
                app.commands[0].logs.pop_front();
            }
        }
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
        let root = app.project_root.clone();
        app.services.toggle(0, &root); // start A
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
        let root = app.project_root.clone();
        app.services.toggle(0, &root); // should not hang or panic
    }

    #[test]
    fn self_dependency_does_not_infinite_loop() {
        let mut a = svc("a", "a", None);
        a.depends_on = vec!["a".to_string()];
        let mut app = app_with(vec![a], vec![]);
        let root = app.project_root.clone();
        app.services.toggle(0, &root); // should not hang or panic
    }

    // ===== Fix 4: Scroll offset clamped to log length =====

    #[test]
    fn scroll_up_clamped_to_log_length() {
        let mut app = app_with(vec![svc("web", "w", None)], vec![]);
        // Add 10 log lines
        for i in 0..10 {
            app.services[0].logs.push_back(format!("line {}", i));
        }
        // Scroll up way past the log length
        app.scroll_up(1000);
        // Offset should be clamped to the number of log lines
        assert!(
            app.services.log_scroll_offset <= app.services[0].logs.len(),
            "Scroll offset {} should be clamped to log length {}",
            app.services.log_scroll_offset,
            app.services[0].logs.len()
        );
    }

    // ===== Fix 5: Integration — full flow tests =====

    #[test]
    fn integration_toggle_service_produces_logs() {
        let mut app = app_with(vec![svc("echo", "e", None)], vec![]);
        let root = app.project_root.clone();
        app.services.toggle(0, &root);
        assert_eq!(app.services[0].status, ServiceStatus::Starting);
        // Should have a "starting" log entry
        assert!(app.services[0].logs.iter().any(|l| l.contains("starting")));
    }

    #[test]
    fn integration_run_command_completes() {
        let mut app = app_with(vec![], vec![cmd("echo", "e", "echo done")]);
        let root = app.project_root.clone();
        app.commands.run(0, &root);
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
        let root = app.project_root.clone();
        app.commands.run(0, &root);
        std::thread::sleep(std::time::Duration::from_millis(500));
        app.poll_logs();
        app.check_processes();
        assert_eq!(app.commands[0].status, CommandStatus::Failed);
    }

    #[test]
    fn integration_command_output_collected() {
        let mut app = app_with(vec![], vec![cmd("echo", "e", "echo hello_world")]);
        let root = app.project_root.clone();
        app.commands.run(0, &root);
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
        assert_eq!(app.services.selected_idx(), 0);
        app.select_down();
        assert_eq!(app.services.selected_idx(), 1);
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
    use crate::commands::CommandStatus;
    use crate::config::*;
    use crate::services::ServiceStatus;
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
        // Add log lines so scroll has room
        for i in 0..20 {
            app.services[0].logs.push_back(format!("line {}", i));
        }
        assert_eq!(app.services.log_scroll_offset, 0);
        app.scroll_up(5);
        assert_eq!(app.services.log_scroll_offset, 5);
        app.scroll_up(3);
        assert_eq!(app.services.log_scroll_offset, 8);
    }

    #[test]
    fn scroll_down_decreases_offset() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.services.log_scroll_offset = 10;
        app.scroll_down(3);
        assert_eq!(app.services.log_scroll_offset, 7);
    }

    #[test]
    fn scroll_down_saturates_at_zero() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.services.log_scroll_offset = 2;
        app.scroll_down(10);
        assert_eq!(app.services.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_resets() {
        let mut app = app_with(vec![svc("web", "w")], vec![]);
        app.services.log_scroll_offset = 50;
        app.scroll_to_bottom();
        assert_eq!(app.services.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_up_commands_tab_uses_cmd_offset() {
        let mut app = app_with(vec![], vec![cmd("build", "b")]);
        // Add log lines directly (CommandsPane owns its own channel)
        for i in 0..20 {
            app.commands[0].logs.push_back(format!("line {}", i));
        }
        app.tab = Tab::Commands;
        app.scroll_up(5);
        assert_eq!(app.commands.log_scroll_offset, 5);
        assert_eq!(app.services.log_scroll_offset, 0);
    }

    #[test]
    fn scroll_on_tools_tab_is_noop() {
        let mut app = empty_app();
        app.tab = Tab::Tools;
        app.scroll_up(10);
        assert_eq!(app.services.log_scroll_offset, 0);
        assert_eq!(app.commands.log_scroll_offset, 0);
    }

    #[test]
    fn select_down_resets_scroll() {
        let mut app = app_with(vec![svc("a", "a"), svc("b", "b")], vec![]);
        app.services.log_scroll_offset = 20;
        app.select_down();
        assert_eq!(app.services.selected_idx(), 1);
        assert_eq!(app.services.log_scroll_offset, 0);
    }

    #[test]
    fn select_up_resets_scroll() {
        let mut app = app_with(vec![svc("a", "a"), svc("b", "b")], vec![]);
        app.services[1].status; // just access to verify indexing works
        // set selected to 1 via select_down
        app.select_down();
        app.services.log_scroll_offset = 15;
        app.select_up();
        assert_eq!(app.services.selected_idx(), 0);
        assert_eq!(app.services.log_scroll_offset, 0);
    }

    #[test]
    fn select_up_at_zero_keeps_scroll() {
        let mut app = app_with(vec![svc("a", "a")], vec![]);
        app.services.log_scroll_offset = 10;
        app.select_up(); // already at 0, no selection change
        assert_eq!(app.services.log_scroll_offset, 10);
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
        app.services[0].status = ServiceStatus::Running;
        let new = config_with(vec![svc("API", "a"), svc("Worker", "k")], vec![], vec![], vec![]);
        app.apply_config(new);
        // API must still be findable by its stable typed ID
        assert_eq!(app.services[0].config.name, "API");
        // The ServicesPane owns its own channel now — log routing via poll_logs
        // is tested in services::tests. Just verify the service is still there.
        assert_eq!(app.services.len(), 2);
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
        // Manually set internal selected to 2 via select_down twice
        app.tab = crate::app::Tab::Tools;
        app.select_down();
        app.select_down();
        let new = config_with(vec![], vec![], vec![link("A", "a", "u1")], vec![]);
        app.apply_config(new);
        assert_eq!(app.tools.len(), 1);
        assert_eq!(app.tools.selected_idx(), 0);
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
        // We exercise the same code by calling toggle through the pane.
        // To avoid spawn flakiness, set port_active=true → start_service short-circuits, but
        // doesn't clear dirty. Use a real spawnable command instead: "true" exits immediately.
        app.services[0].config.command = "true".to_string();
        app.services[0].config.working_dir = ".".to_string();
        let root = app.project_root.clone();
        app.services.toggle(0, &root);
        assert!(!app.services[0].config_dirty);
    }

    #[test]
    fn run_command_clears_config_dirty() {
        let mut app = app_with(vec![], vec![cmd("c", "c")]);
        app.commands[0].config_dirty = true;
        app.commands[0].config.command = "true".to_string();
        app.commands[0].config.working_dir = ".".to_string();
        let root = app.project_root.clone();
        app.commands.run(0, &root);
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
