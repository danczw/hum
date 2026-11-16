use std::collections::{HashMap, HashSet};
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
/// Version suffixes are ignored, so `python3.12` counts as `python`.
const RUNTIMES: &[&str] = &["node", "bun", "deno", "python"];

pub struct Proc {
    pub pid: Pid,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
    pub depth: usize,
    start: u64,
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
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for p in sys.processes().values() {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(p.pid());
        }
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }

    let kinds: HashMap<Pid, &'static str> = sys
        .processes()
        .values()
        .filter_map(|p| Some((p.pid(), agent_kind(p)?)))
        .collect();
    let mut agents: Vec<Agent> = kinds
        .iter()
        // Agents started by another agent belong to that agent's tree.
        .filter(|&(&pid, _)| {
            let parent = sys.process(pid).and_then(Process::parent);
            !std::iter::successors(parent, |&p| sys.process(p)?.parent())
                .any(|p| kinds.contains_key(&p))
        })
        .map(|(&pid, &kind)| {
            let mut procs = Vec::new();
            walk(sys, &children, pid, 0, &mut procs);
            Agent { kind, procs }
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
        name: proc_name(p),
        cpu: p.cpu_usage(),
        mem: p.memory(),
        depth,
        start: p.start_time(),
    });
    if let Some(kids) = children.get(&pid) {
        for &kid in kids {
            walk(sys, children, kid, depth + 1, out);
        }
    }
}

fn agent_kind(p: &Process) -> Option<&'static str> {
    let cmd = p.cmd();
    let program = cmd.first().map(|a| basename(a));
    if let Some(kind) = program
        .and_then(match_agent)
        .or_else(|| match_agent(p.name()))
    {
        return Some(kind);
    }
    let program = program?.to_string_lossy();
    let runtime = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !RUNTIMES.contains(&runtime) {
        return None;
    }
    // Only the script or `-m` module names the agent; later arguments are
    // its own, e.g. `python -m pytest tests/gemini`.
    let mut args = cmd[1..].iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-m") => return args.next().and_then(|m| match_agent(m)),
            // Python flags that take a separate value, e.g. `python -X dev`.
            Some("-X" | "-W") => {
                args.next();
            }
            // Flags, and the subcommand in `bun run` or `deno run`.
            Some(a) if a.starts_with('-') || a == "run" => {}
            _ => return match_agent(basename(arg)),
        }
    }
    None
}

fn match_agent(name: &OsStr) -> Option<&'static str> {
    AGENTS.iter().copied().find(|a| OsStr::new(a) == name)
}

/// sysinfo keeps a pid's name from before an exec, but cmd is re-read every
/// tick, so name the process after what it runs now.
pub fn proc_name(p: &Process) -> String {
    p.cmd()
        .first()
        .map_or(p.name(), |a| basename(a))
        .to_string_lossy()
        .into_owned()
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

        let mut current = HashSet::new();
        for agent in agents {
            for p in &agent.procs[1..] {
                current.insert(p.pid);
                self.seen.insert(p.pid, (agent.kind, p.start));
            }
        }
        // A nested agent whose parent agent exited is now a root, not an orphan.
        for agent in agents {
            self.seen.remove(&agent.procs[0].pid);
        }

        let mut orphans: Vec<Orphan> = self
            .seen
            .iter()
            .filter(|(pid, _)| !current.contains(pid))
            .filter_map(|(pid, (agent, _))| {
                let p = sys.process(*pid)?;
                Some(Orphan {
                    pid: *pid,
                    name: proc_name(p),
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
