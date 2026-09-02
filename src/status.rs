//! Transient status line shown at the bottom of the TUI.
//!
//! Messages auto-expire after `TTL`; `set` replaces any prior message.

use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(3);

pub struct StatusBar {
    current: Option<(String, Instant)>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self { current: None }
    }

    pub fn set(&mut self, msg: String) {
        self.current = Some((msg, Instant::now()));
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_ref().map(|(m, _)| m.as_str())
    }

    pub fn clear_if_expired(&mut self) {
        if let Some((_, ts)) = &self.current {
            if ts.elapsed() > TTL {
                self.current = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_stores_message() {
        let mut s = StatusBar::new();
        s.set("hello".to_string());
        assert_eq!(s.current(), Some("hello"));
    }

    #[test]
    fn set_replaces_prior_message() {
        let mut s = StatusBar::new();
        s.set("first".to_string());
        s.set("second".to_string());
        assert_eq!(s.current(), Some("second"));
    }

    #[test]
    fn clear_if_expired_noop_while_fresh() {
        let mut s = StatusBar::new();
        s.set("hi".to_string());
        s.clear_if_expired();
        assert_eq!(s.current(), Some("hi"));
    }

    #[test]
    fn clear_if_expired_empty_is_noop() {
        let mut s = StatusBar::new();
        s.clear_if_expired();
        assert!(s.current().is_none());
    }
}
