//! Tools pane: links (open in browser) and copies (to clipboard).
//!
//! No processes, no logs, no IDs — fully rebuilt on config reload.

use crate::config::{CopyConfig, LinkConfig};
use crate::platform;

pub enum ToolKind {
    Link(String),
    Copy(String),
}

pub struct ToolItem {
    pub name: String,
    pub key: char,
    pub kind: ToolKind,
}

pub struct ToolsPane {
    items: Vec<ToolItem>,
    selected: usize,
}

impl ToolsPane {
    pub fn from_config(links: Vec<LinkConfig>, copies: Vec<CopyConfig>) -> Self {
        let items = build_items(&links, &copies);
        Self { items, selected: 0 }
    }

    pub fn items(&self) -> &[ToolItem] { &self.items }
    pub fn selected_idx(&self) -> usize { self.selected }
    pub fn len(&self) -> usize { self.items.len() }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }

    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn select_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn find_by_key(&self, key: char) -> Option<usize> {
        let k = key.to_ascii_lowercase();
        self.items.iter().position(|t| t.key.to_ascii_lowercase() == k)
    }

    /// Returns a status string to show (Ok or Err).
    pub fn activate(&self, idx: usize) -> Result<String, String> {
        let tool = self.items.get(idx).ok_or_else(|| "no such tool".to_string())?;
        match &tool.kind {
            ToolKind::Link(url) => platform::open_url(url)
                .map(|_| format!("Opened: {}", url))
                .map_err(|e| format!("Error: {}", e)),
            ToolKind::Copy(text) => platform::copy_to_clipboard(text)
                .map(|_| format!("Copied: {}", tool.name))
                .map_err(|e| format!("Error: {}", e)),
        }
    }

    pub fn rebuild(&mut self, links: &[LinkConfig], copies: &[CopyConfig]) {
        self.items = build_items(links, copies);
        if self.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
    }
}

fn build_items(links: &[LinkConfig], copies: &[CopyConfig]) -> Vec<ToolItem> {
    let mut out = Vec::with_capacity(links.len() + copies.len());
    for l in links {
        out.push(ToolItem {
            key: l.key.chars().next().unwrap_or('?'),
            name: l.name.clone(),
            kind: ToolKind::Link(l.url.clone()),
        });
    }
    for c in copies {
        out.push(ToolItem {
            key: c.key.chars().next().unwrap_or('?'),
            name: c.name.clone(),
            kind: ToolKind::Copy(c.text.clone()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, key: &str, url: &str) -> LinkConfig {
        LinkConfig { name: name.into(), key: key.into(), url: url.into() }
    }
    fn copy(name: &str, key: &str, text: &str) -> CopyConfig {
        CopyConfig { name: name.into(), key: key.into(), text: text.into() }
    }

    #[test]
    fn from_config_orders_links_then_copies() {
        let p = ToolsPane::from_config(
            vec![link("Docs", "d", "https://x")],
            vec![copy("Tok", "t", "secret")],
        );
        assert_eq!(p.items().len(), 2);
        assert_eq!(p.items()[0].name, "Docs");
        assert_eq!(p.items()[1].name, "Tok");
    }

    #[test]
    fn find_by_key_is_case_insensitive() {
        let p = ToolsPane::from_config(
            vec![link("Docs", "D", "https://x")],
            vec![],
        );
        assert_eq!(p.find_by_key('d'), Some(0));
    }

    #[test]
    fn rebuild_clamps_selection() {
        let mut p = ToolsPane::from_config(
            vec![link("A", "a", "u"), link("B", "b", "u"), link("C", "c", "u")],
            vec![],
        );
        p.select_down(); p.select_down();  // selected = 2
        p.rebuild(&[link("A", "a", "u")], &[]);
        assert_eq!(p.selected_idx(), 0);
    }

    #[test]
    fn select_up_saturates_at_zero() {
        let mut p = ToolsPane::from_config(
            vec![link("A", "a", "u")], vec![]
        );
        p.select_up();
        assert_eq!(p.selected_idx(), 0);
    }
}
