//! Commands pane: one-shot processes that run to completion.
//!
//! Each command has a stable `CommandId`. Logs flow through an owned mpsc.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc;

use crate::app::LogSource;
use crate::config::CommandConfig;
use crate::id::CommandId;
use crate::process::ProcessHandle;

pub const LOG_CAPACITY: usize = 500;

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

#[derive(Default)]
pub struct CommandsDelta {
    pub added: usize,
    pub dropped: usize,
    pub pending_restart: usize,
    pub orphaned: usize,
}

pub struct CommandsPane {
    items: Vec<CommandState>,
    selected: usize,
    pub log_scroll_offset: usize,
    log_rx: mpsc::Receiver<(LogSource, String)>,
    log_tx: mpsc::Sender<(LogSource, String)>,
    next_id: u64,
}

impl CommandsPane {
    pub fn from_config(configs: Vec<CommandConfig>) -> Self {
        let (log_tx, log_rx) = mpsc::channel();
        let mut next_id: u64 = 0;
        let items = configs
            .into_iter()
            .map(|cfg| {
                next_id += 1;
                CommandState {
                    id: CommandId(next_id),
                    config: cfg,
                    process: None,
                    status: CommandStatus::Idle,
                    logs: VecDeque::with_capacity(LOG_CAPACITY),
                    config_dirty: false,
                    orphan: false,
                }
            })
            .collect();
        Self { items, selected: 0, log_scroll_offset: 0, log_rx, log_tx, next_id }
    }

    pub fn items(&self) -> &[CommandState] { &self.items }
    pub fn selected_idx(&self) -> usize { self.selected }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }

    pub fn run(&mut self, idx: usize, project_root: &Path) {
        let Some(state) = self.items.get_mut(idx) else { return; };
        if state.status == CommandStatus::Running {
            return;
        }
        state.logs.clear();
        state.status = CommandStatus::Running;
        state.config_dirty = false;
        let working_dir = project_root.join(&state.config.working_dir);
        let cmd = state.config.command.clone();
        state.logs.push_back(format!("── running: {} ──", cmd));
        let id = state.id;
        match ProcessHandle::spawn(
            &cmd,
            working_dir.to_str().unwrap_or("."),
            self.log_tx.clone(),
            move || LogSource::Command(id),
        ) {
            Ok(handle) => state.process = Some(handle),
            Err(e) => {
                state.logs.push_back(format!("error: {}", e));
                state.status = CommandStatus::Failed;
            }
        }
    }

    pub fn find_by_key(&self, key: char) -> Option<usize> {
        let k = key.to_ascii_lowercase();
        self.items.iter().position(|c| c.config.key_char().to_ascii_lowercase() == k)
    }

    pub fn poll_logs(&mut self) {
        while let Ok((source, line)) = self.log_rx.try_recv() {
            if let LogSource::Command(id) = source {
                if let Some(c) = self.items.iter_mut().find(|c| c.id == id) {
                    c.logs.push_back(line);
                    if c.logs.len() > LOG_CAPACITY {
                        c.logs.pop_front();
                    }
                }
            }
        }
    }

    pub fn check_processes(&mut self) {
        for cmd in &mut self.items {
            if cmd.status == CommandStatus::Running {
                if let Some(proc) = &mut cmd.process {
                    if !proc.is_running() {
                        let code = proc.exit_code();
                        cmd.process = None;
                        if code == Some(0) {
                            cmd.status = CommandStatus::Done;
                            cmd.logs.push_back("── done ──".to_string());
                        } else {
                            cmd.status = CommandStatus::Failed;
                            cmd.logs.push_back(format!(
                                "── failed (exit {}) ──",
                                code.map(|c| c.to_string()).unwrap_or("?".into())
                            ));
                        }
                    }
                }
            }
        }
    }

    pub fn apply_config(&mut self, new: &[CommandConfig]) -> CommandsDelta {
        let mut delta = CommandsDelta::default();
        let mut drop_flags: Vec<bool> = vec![false; self.items.len()];
        for (i, state) in self.items.iter_mut().enumerate() {
            if let Some(new_cfg) = new.iter().find(|c| c.name == state.config.name) {
                let changed = command_config_changed(&state.config, new_cfg);
                if state.status == CommandStatus::Running {
                    state.config = new_cfg.clone();
                    state.orphan = false;
                    if changed {
                        state.config_dirty = true;
                        delta.pending_restart += 1;
                    }
                } else {
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
                delta.orphaned += 1;
            } else {
                drop_flags[i] = true;
            }
        }
        while let Some(true) = drop_flags.last().copied() {
            self.items.pop();
            drop_flags.pop();
            delta.dropped += 1;
        }
        for cfg in new.iter() {
            let exists = self.items.iter().any(|c| c.config.name == cfg.name);
            if !exists {
                self.next_id += 1;
                self.items.push(CommandState {
                    id: CommandId(self.next_id),
                    config: cfg.clone(),
                    process: None,
                    status: CommandStatus::Idle,
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
        while let Some(c) = self.items.last() {
            if c.orphan && c.status != CommandStatus::Running && c.process.is_none() {
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
        for c in &mut self.items {
            if let Some(mut proc) = c.process.take() {
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
        let max = self.items.get(self.selected).map_or(0, |c| c.logs.len());
        self.log_scroll_offset = self.log_scroll_offset.saturating_add(n).min(max);
    }
    pub fn scroll_down(&mut self, n: usize) {
        self.log_scroll_offset = self.log_scroll_offset.saturating_sub(n);
    }
    pub fn scroll_to_bottom(&mut self) { self.log_scroll_offset = 0; }
}

impl std::ops::Index<usize> for CommandsPane {
    type Output = CommandState;
    fn index(&self, idx: usize) -> &CommandState { &self.items[idx] }
}

impl std::ops::IndexMut<usize> for CommandsPane {
    fn index_mut(&mut self, idx: usize) -> &mut CommandState { &mut self.items[idx] }
}

fn command_config_changed(a: &CommandConfig, b: &CommandConfig) -> bool {
    a.command != b.command || a.working_dir != b.working_dir || a.key != b.key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_cfg(name: &str, key: &str, command: &str) -> CommandConfig {
        CommandConfig {
            name: name.into(),
            key: key.into(),
            command: command.into(),
            working_dir: ".".into(),
        }
    }

    #[test]
    fn from_config_assigns_unique_ids() {
        let p = CommandsPane::from_config(vec![
            cmd_cfg("a", "a", "true"),
            cmd_cfg("b", "b", "true"),
        ]);
        assert_ne!(p.items()[0].id, p.items()[1].id);
    }

    #[test]
    fn apply_config_appends_new() {
        let mut p = CommandsPane::from_config(vec![cmd_cfg("a", "a", "true")]);
        let delta = p.apply_config(&[
            cmd_cfg("a", "a", "true"),
            cmd_cfg("b", "b", "true"),
        ]);
        assert_eq!(delta.added, 1);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn apply_config_drops_missing_stopped() {
        let mut p = CommandsPane::from_config(vec![
            cmd_cfg("a", "a", "true"),
            cmd_cfg("b", "b", "true"),
        ]);
        let delta = p.apply_config(&[cmd_cfg("a", "a", "true")]);
        assert_eq!(delta.dropped, 1);
        assert_eq!(p.len(), 1);
    }
}
