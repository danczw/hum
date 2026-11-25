//! Copilot CLI subagents. They run inside the copilot process and log only
//! to its session's `events.jsonl`, so that log is followed across ticks.
//! Its format is undocumented, so unknown lines are skipped.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::agents::{Agent, ProcKey};
use crate::claude::{SUBAGENT_QUIET, Subagent, top_level_raw, top_level_str};
use crate::host::Host;

/// `$COPILOT_HOME`, else `~/.copilot`.
pub fn home(copilot_home: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    copilot_home
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(home?.join(".copilot")))
}

/// The session logs of running copilot agents, kept across ticks so each
/// tick reads only the lines appended since.
pub struct Copilot {
    home: PathBuf,
    logs: HashMap<ProcKey, Log>,
}

/// A session dir and what its `events.jsonl` held up to `offset`.
struct Log {
    dir: PathBuf,
    offset: u64,
    /// In start order, each with when its last event was read.
    running: Vec<(Subagent, SystemTime)>,
}

impl Copilot {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            logs: HashMap::new(),
        }
    }

    /// Sets the subagents of each copilot agent and forgets the logs of
    /// agents gone.
    pub fn update(&mut self, agents: &mut [Agent], now: SystemTime, host: &impl Host) {
        self.logs
            .retain(|key, _| agents.iter().any(|a| a.key() == *key));
        for agent in agents.iter_mut().filter(|a| a.kind == "copilot") {
            let key = agent.key();
            // copilot holds the lock while the session is open, and moves it
            // when the user switches sessions
            let lock = format!("inuse.{}.lock", key.pid);
            let held = |log: &Log| host.times(&log.dir.join(&lock)).is_some();
            if !self.logs.get(&key).is_some_and(held) {
                self.logs.remove(&key);
                if let Some(dir) = session_dir(&self.home, &lock, key.start, host) {
                    let log = Log {
                        dir,
                        offset: 0,
                        running: Vec::new(),
                    };
                    self.logs.insert(key, log);
                }
            }
            if let Some(log) = self.logs.get_mut(&key) {
                log.read(now, host);
                agent.subagents = log.running.iter().map(|(s, _)| s.clone()).collect();
            }
        }
    }
}

/// The session dir holding `lock`, unless the lock is older than the
/// process, started at `start` in seconds since the epoch: then an earlier
/// process on the same pid left it behind.
fn session_dir(home: &Path, lock: &str, start: u64, host: &impl Host) -> Option<PathBuf> {
    // sysinfo counts the start from the boot time in whole seconds, so it
    // may read up to a second late
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(start.saturating_sub(1));
    host.list(&home.join("session-state"))?
        .into_iter()
        .find(|dir| {
            host.times(&dir.join(lock))
                .is_some_and(|t| t.created.unwrap_or(t.modified) >= started)
        })
}

impl Log {
    /// Applies the lines appended since the last read, then drops subagents
    /// quiet for [`SUBAGENT_QUIET`], as a crashed one never logs its end. A
    /// log shorter than what was read was replaced, so it is read anew.
    fn read(&mut self, now: SystemTime, host: &impl Host) {
        let path = self.dir.join("events.jsonl");
        let bytes = host.read_from(&path, self.offset).unwrap_or_else(|| {
            self.offset = 0;
            self.running.clear();
            host.read_from(&path, 0).unwrap_or_default()
        });
        // a line still being written is read whole next time
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        self.offset += end as u64;
        for line in String::from_utf8_lossy(&bytes[..end]).lines() {
            self.apply(line, now);
        }
        let quiet_since = now
            .checked_sub(SUBAGENT_QUIET)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.running.retain(|&(_, last)| last >= quiet_since);
    }

    /// One event, parsed as a whole line: tool output may quote event names.
    /// The `task` tool's end is no subagent's end, as a background subagent
    /// keeps running after it.
    fn apply(&mut self, line: &str, now: SystemTime) {
        let Some(id) = top_level_str(line, "agentId") else {
            return;
        };
        match top_level_str(line, "type").as_deref() {
            Some("subagent.started") => {
                let data = top_level_raw(line, "data").unwrap_or_default();
                let named = |key| top_level_str(&data, key).filter(|n| !n.is_empty());
                if let Some(kind) = named("agentDisplayName").or_else(|| named("agentName")) {
                    self.running.push((Subagent { id, kind }, now));
                }
            }
            Some("subagent.completed" | "subagent.failed") => {
                self.running.retain(|(s, _)| s.id != id);
            }
            // the subagent's own tool calls
            _ => {
                for (s, last) in &mut self.running {
                    if s.id == id {
                        *last = now;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{fixture::proc, scan};
    use crate::host::MemHost;

    const DIR: &str = "/cp/session-state/5e55";

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn started(id: &str, name: &str, mode: &str) -> String {
        format!(
            r#"{{"type":"subagent.started","data":{{"toolCallId":"t{id}","agentName":"task","agentDisplayName":"{name}","executionMode":"{mode}"}},"agentId":"{id}","id":"e1"}}"#
        )
    }

    fn event(id: &str, kind: &str) -> String {
        format!(r#"{{"type":"{kind}","data":{{"toolCallId":"t{id}"}},"agentId":"{id}"}}"#)
    }

    /// The `task` tool's end, which the parent logs without an `agentId`.
    fn task_complete(id: &str) -> String {
        format!(
            r#"{{"type":"tool.execution_complete","data":{{"toolCallId":"t{id}","result":{{"content":"Agent started in background"}}}}}}"#
        )
    }

    /// Writes the log with `lines`, each ended by a newline.
    fn log(host: &mut MemHost, lines: &[String]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        host.file(format!("{DIR}/events.jsonl"), &text, at(100));
    }

    /// A host where copilot pid 10, started at second 100, holds the lock of
    /// session 5e55, logged with `lines`.
    fn host(lines: &[String]) -> MemHost {
        let mut host = MemHost::default();
        host.file(format!("{DIR}/inuse.10.lock"), "10", at(100));
        log(&mut host, lines);
        host
    }

    /// The names of the subagents running at second `secs`.
    fn tick(copilot: &mut Copilot, host: &MemHost, secs: u64) -> Vec<String> {
        let procs = HashMap::from([proc(10, 1, 100, "copilot")]);
        let mut agents = scan(&procs, None, &[], None, host);
        copilot.update(&mut agents, at(secs), host);
        agents[0].subagents.iter().map(|s| s.kind.clone()).collect()
    }

    fn copilot() -> Copilot {
        Copilot::new(PathBuf::from("/cp"))
    }

    #[test]
    fn subagent_runs_until_it_completes() {
        let mut host = host(&[started("a1", "sleeper", "sync")]);
        let mut copilot = copilot();
        assert_eq!(tick(&mut copilot, &host, 100), ["sleeper"]);
        log(
            &mut host,
            &[
                started("a1", "sleeper", "sync"),
                event("a1", "subagent.completed"),
            ],
        );
        assert!(tick(&mut copilot, &host, 101).is_empty());
    }

    #[test]
    fn background_subagent_outlives_its_task_call() {
        let mut lines = vec![started("a1", "bg", "background"), task_complete("a1")];
        let mut host = host(&lines);
        let mut copilot = copilot();
        assert_eq!(tick(&mut copilot, &host, 100), ["bg"]);
        lines.push(event("a1", "subagent.completed"));
        log(&mut host, &lines);
        assert!(tick(&mut copilot, &host, 101).is_empty());
    }

    #[test]
    fn failed_subagent_ends() {
        let host = host(&[
            started("a1", "x", "sync"),
            started("a2", "y", "sync"),
            event("a1", "subagent.failed"),
        ]);
        assert_eq!(tick(&mut copilot(), &host, 100), ["y"]);
    }

    #[test]
    fn subagent_quiet_too_long_ends() {
        let mut lines = vec![started("a1", "x", "sync")];
        let mut host = host(&lines);
        let mut copilot = copilot();
        tick(&mut copilot, &host, 100);
        // its own tool call keeps it running
        lines.push(event("a1", "tool.execution_start"));
        log(&mut host, &lines);
        assert_eq!(tick(&mut copilot, &host, 700), ["x"]);
        assert_eq!(tick(&mut copilot, &host, 1360), ["x"]);
        assert!(tick(&mut copilot, &host, 1361).is_empty());
    }

    #[test]
    fn only_appended_lines_are_read() {
        let first = started("a1", "x", "sync") + "\n";
        let mut host = host(&[]);
        // the last line is still being written
        let partial = started("a2", "y", "sync");
        let text = format!("{first}{}", &partial[..20]);
        host.file(format!("{DIR}/events.jsonl"), &text, at(100));
        let mut copilot = copilot();
        assert_eq!(tick(&mut copilot, &host, 100), ["x"]);
        // blanking what was read shows it is not read again
        let text = format!("{}\n{partial}\n", " ".repeat(first.len() - 1));
        host.file(format!("{DIR}/events.jsonl"), &text, at(100));
        assert_eq!(tick(&mut copilot, &host, 101), ["x", "y"]);
    }

    #[test]
    fn event_names_in_tool_output_are_no_events() {
        let output = r#"{"type":"tool.execution_complete","data":{"result":{"content":"{\"type\":\"subagent.started\",\"agentId\":\"a9\"}"}},"agentId":"a9"}"#;
        assert!(tick(&mut copilot(), &host(&[output.into()]), 100).is_empty());
    }

    #[test]
    fn missing_or_stale_lock_has_no_session() {
        let line = started("a1", "x", "sync");
        let mut host = host(&[line]);
        // left by an earlier process on pid 10
        host.file(format!("{DIR}/inuse.10.lock"), "10", at(50));
        assert!(tick(&mut copilot(), &host, 100).is_empty());
        host.files
            .remove(Path::new(&format!("{DIR}/inuse.10.lock")));
        assert!(tick(&mut copilot(), &host, 100).is_empty());
    }

    #[test]
    fn empty_copilot_home_falls_back_to_home() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            super::home(Some(OsString::new()), home),
            Some(PathBuf::from("/home/u/.copilot"))
        );
    }
}
