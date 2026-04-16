//! Single authority on reserved keybindings and key-conflict detection.

use crate::commands::CommandState;
use crate::services::ServiceState;
use crate::tools::ToolItem;

/// Keys reserved by the Services tab (start-all / stop-all) — no service may
/// shadow them. `q` is the global quit key and is forbidden everywhere.
pub const SERVICES_RESERVED: &[char] = &['a', 'x'];
pub const GLOBAL_RESERVED: &[char] = &['q'];

pub fn is_services_reserved(key: char) -> bool {
    let k = key.to_ascii_lowercase();
    SERVICES_RESERVED.contains(&k) || GLOBAL_RESERVED.contains(&k)
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
            out.push(format!("service '{}' key '{}' conflicts with quit", s.config.name, k));
        }
        if SERVICES_RESERVED.contains(&k) {
            out.push(format!(
                "service '{}' key '{}' conflicts with reserved shortcut",
                s.config.name, k
            ));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate service key '{}'", k));
        } else {
            seen.push(k);
        }
    }

    let mut seen = Vec::<char>::new();
    for c in commands.iter() {
        let k = c.config.key_char().to_ascii_lowercase();
        if GLOBAL_RESERVED.contains(&k) {
            out.push(format!("command '{}' key '{}' conflicts with quit", c.config.name, k));
        }
        if seen.contains(&k) {
            out.push(format!("duplicate command key '{}'", k));
        } else {
            seen.push(k);
        }
    }

    let mut seen = Vec::<char>::new();
    for t in tools.iter() {
        let k = t.key.to_ascii_lowercase();
        if GLOBAL_RESERVED.contains(&k) {
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

    #[test]
    fn is_reserved_for_services_uppercase_and_lowercase() {
        assert!(is_services_reserved('a'));
        assert!(is_services_reserved('A'));
        assert!(is_services_reserved('x'));
        assert!(is_services_reserved('q'));
        assert!(!is_services_reserved('b'));
    }
}
