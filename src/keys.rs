//! Single authority on reserved keybindings and key-conflict detection.

use crate::commands::CommandState;
use crate::services::ServiceState;
use crate::tools::ToolItem;

/// Keys consumed by the Services tab (`x` → stop-all) — no service may shadow
/// them. Globally reserved keys are swallowed by the main event loop before
/// reaching any pane, so they're dead as user bindings on every tab:
///   `q` → quit, `j`/`k` → list navigation, `space` → open URL.
pub const SERVICES_RESERVED: &[char] = &['x'];
pub const GLOBAL_RESERVED: &[char] = &['q', 'j', 'k', ' '];

pub fn is_services_reserved(key: char) -> bool {
    let k = key.to_ascii_lowercase();
    SERVICES_RESERVED.contains(&k) || GLOBAL_RESERVED.contains(&k)
}

fn key_label(c: char) -> String {
    if c == ' ' { "space".to_string() } else { c.to_string() }
}

pub fn detect_conflicts(
    services: &[ServiceState],
    commands: &[CommandState],
    tools: &[ToolItem],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    let mut seen = Vec::<char>::new();
    for s in services.iter() {
        let k = s.config.key_char().to_ascii_lowercase();
        if GLOBAL_RESERVED.contains(&k) {
            out.push(format!(
                "service '{}' key '{}' is globally reserved",
                s.config.name, key_label(k),
            ));
        }
        if SERVICES_RESERVED.contains(&k) {
            out.push(format!(
                "service '{}' key '{}' conflicts with stop-all",
                s.config.name, key_label(k),
            ));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate service key '{}'", key_label(k)));
        } else {
            seen.push(k);
        }
    }

    let mut seen = Vec::<char>::new();
    for c in commands.iter() {
        let k = c.config.key_char().to_ascii_lowercase();
        if GLOBAL_RESERVED.contains(&k) {
            out.push(format!(
                "command '{}' key '{}' is globally reserved",
                c.config.name, key_label(k),
            ));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate command key '{}'", key_label(k)));
        } else {
            seen.push(k);
        }
    }

    let mut seen = Vec::<char>::new();
    for t in tools.iter() {
        let k = t.key.to_ascii_lowercase();
        if GLOBAL_RESERVED.contains(&k) {
            out.push(format!(
                "tool '{}' key '{}' is globally reserved",
                t.name, key_label(k),
            ));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate tool key '{}'", key_label(k)));
        } else {
            seen.push(k);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_reserved_for_services_uppercase_and_lowercase() {
        assert!(is_services_reserved('x'));
        assert!(is_services_reserved('X'));
        assert!(is_services_reserved('q'));
        assert!(is_services_reserved('j'));
        assert!(is_services_reserved('k'));
        assert!(is_services_reserved(' '));
        assert!(!is_services_reserved('a'));
        assert!(!is_services_reserved('b'));
    }

    #[test]
    fn detect_conflicts_flags_globally_consumed_keys() {
        use crate::commands::{CommandState, CommandStatus};
        use crate::config::{CommandConfig, ServiceConfig};
        use crate::id::{CommandId, ServiceId};
        use std::collections::VecDeque;

        let svc_j = ServiceState {
            id: ServiceId(1),
            config: ServiceConfig {
                name: "Jobs".into(), key: "j".into(),
                command: "true".into(), working_dir: ".".into(),
                port: None, url: None, depends_on: vec![],
            },
            process: None, status: crate::services::ServiceStatus::Stopped,
            port_active: false, stopping_since: None,
            logs: VecDeque::new(), config_dirty: false, orphan: false,
        };
        let cmd_space = CommandState {
            id: CommandId(1),
            config: CommandConfig {
                name: "Build".into(), key: " ".into(),
                command: "true".into(), working_dir: ".".into(),
            },
            process: None, status: CommandStatus::Idle,
            logs: VecDeque::new(), config_dirty: false, orphan: false,
        };

        let warnings = detect_conflicts(&[svc_j], &[cmd_space], &[]);
        assert!(warnings.iter().any(|w| w.contains("Jobs") && w.contains('j')));
        assert!(warnings.iter().any(|w| w.contains("Build")));
    }
}
