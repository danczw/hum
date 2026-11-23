pub mod cursor;
pub mod outline;

use sysinfo::Pid;

use crate::agents::ProcKey;

/// Names a row across ticks so the cursor follows it when rows reorder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Proc(ProcKey),
    /// A subagent by its id.
    Subagent(String),
    /// A listening port by number and holder; `pid` is None when the holder
    /// is unreadable.
    Port {
        port: u16,
        pid: Option<Pid>,
    },
}
