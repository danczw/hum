use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use sysinfo::{Pid, Process, System};

/// Process names that mark the root of an agent session.
const AGENTS: &[&str] = &[
    "claude",
    "codex",
    "cursor-agent",
    "aider",
    "gemini",
    "opencode",
    "copilot",
];

/// Interpreters that may run an agent as a script, e.g. `node /usr/bin/codex`.
const RUNTIMES: &[&str] = &["node", "bun", "deno", "python", "python3"];

pub struct Proc {
    pub pid: Pid,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
    pub depth: usize,
}

pub struct Agent {
    pub kind: &'static str,
    /// The agent process first, then its descendants depth-first.
    pub procs: Vec<Proc>,
}

impl Agent {
    pub fn cpu(&self) -> f32 {
        self.procs.iter().map(|p| p.cpu).sum()
    }

    pub fn mem(&self) -> u64 {
        self.procs.iter().map(|p| p.mem).sum()
    }
}

pub fn scan(sys: &System) -> Vec<Agent> {
    // Threads are listed as processes on Linux; skip them.
    let procs = || {
        sys.processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
    };
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for p in procs() {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(p.pid());
        }
    }

    let mut agents: Vec<Agent> = procs()
        .filter_map(|p| {
            let kind = agent_kind(p)?;
            // Agents started by another agent belong to that agent's tree.
            if has_agent_ancestor(sys, p) {
                return None;
            }
            let mut out = Vec::new();
            walk(sys, &children, p.pid(), 0, &mut out);
            Some(Agent { kind, procs: out })
        })
        .collect();
    agents.sort_by_key(|a| a.procs[0].pid);
    agents
}

fn walk(
    sys: &System,
    children: &HashMap<Pid, Vec<Pid>>,
    pid: Pid,
    depth: usize,
    out: &mut Vec<Proc>,
) {
    let Some(p) = sys.process(pid) else { return };
    out.push(Proc {
        pid,
        name: p.name().to_string_lossy().into_owned(),
        cpu: p.cpu_usage(),
        mem: p.memory(),
        depth,
    });
    if let Some(kids) = children.get(&pid) {
        let mut kids = kids.clone();
        kids.sort_unstable();
        for kid in kids {
            walk(sys, children, kid, depth + 1, out);
        }
    }
}

fn has_agent_ancestor(sys: &System, p: &Process) -> bool {
    let mut parent = p.parent();
    while let Some(pid) = parent {
        let Some(proc) = sys.process(pid) else {
            return false;
        };
        if agent_kind(proc).is_some() {
            return true;
        }
        parent = proc.parent();
    }
    false
}

fn agent_kind(p: &Process) -> Option<&'static str> {
    if let Some(kind) = match_agent(p.name()) {
        return Some(kind);
    }
    let cmd = p.cmd();
    let runtime = cmd.first().map(|a| basename(a))?;
    if RUNTIMES.contains(&runtime.to_string_lossy().as_ref()) {
        return cmd.get(1).and_then(|a| match_agent(basename(a)));
    }
    None
}

fn match_agent(name: &OsStr) -> Option<&'static str> {
    AGENTS.iter().copied().find(|a| OsStr::new(a) == name)
}

fn basename(arg: &OsStr) -> &OsStr {
    Path::new(arg).file_name().unwrap_or(arg)
}

pub struct Orphan {
    pub pid: Pid,
    pub name: String,
    pub agent: &'static str,
    pub cpu: f32,
    pub mem: u64,
}

/// Remembers which processes agents spawned, to spot the ones left behind.
/// Only catches orphans whose agent tree changed while hum was running.
#[derive(Default)]
pub struct OrphanTracker {
    /// pid -> (agent kind, start time); start time guards against pid reuse.
    seen: HashMap<Pid, (&'static str, u64)>,
}

impl OrphanTracker {
    pub fn update(&mut self, sys: &System, agents: &[Agent]) -> Vec<Orphan> {
        self.seen
            .retain(|pid, (_, start)| sys.process(*pid).is_some_and(|p| p.start_time() == *start));

        let mut current = HashMap::new();
        for agent in agents {
            for p in &agent.procs[1..] {
                current.insert(p.pid, agent.kind);
            }
        }
        for (pid, kind) in &current {
            if let Some(p) = sys.process(*pid) {
                self.seen.insert(*pid, (kind, p.start_time()));
            }
        }
        // A nested agent whose parent agent exited is now a root, not an orphan.
        for agent in agents {
            self.seen.remove(&agent.procs[0].pid);
        }

        let mut orphans: Vec<Orphan> = self
            .seen
            .iter()
            .filter(|(pid, _)| !current.contains_key(pid))
            .filter_map(|(pid, (agent, _))| {
                let p = sys.process(*pid)?;
                Some(Orphan {
                    pid: *pid,
                    name: p.name().to_string_lossy().into_owned(),
                    agent,
                    cpu: p.cpu_usage(),
                    mem: p.memory(),
                })
            })
            .collect();
        orphans.sort_by_key(|o| o.pid);
        orphans
    }
}
