//! Typed, monotonically-assigned identifiers for services and commands.
//!
//! IDs are issued by each owning pane from a private counter. They are stable
//! for the lifetime of an entry — background log-reader threads capture an ID
//! at spawn time and remain correct even if the entry's slot index shifts.

use std::fmt;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ServiceId(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct CommandId(pub u64);

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "svc#{}", self.0)
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cmd#{}", self.0)
    }
}
