use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
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

/// The process fields detection needs, copied out of sysinfo so detection
/// runs on plain data.
pub struct ProcInfo {
    pub pid: Pid,
    pub parent: Option<Pid>,
    pub start: u64,
    pub name: OsString,
    pub cmd: Vec<OsString>,
    pub cpu: f32,
    pub mem: u64,
}

pub fn snapshot(sys: &System) -> HashMap<Pid, ProcInfo> {
    sys.processes()
        .values()
        .map(|p| {
            let info = ProcInfo {
                pid: p.pid(),
                parent: p.parent(),
                start: p.start_time(),
                name: p.name().to_owned(),
                cmd: p.cmd().to_vec(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
            };
            (info.pid, info)
        })
        .collect()
}

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

pub fn scan(procs: &HashMap<Pid, ProcInfo>) -> Vec<Agent> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for p in procs.values() {
        if let Some(parent) = p.parent {
            children.entry(parent).or_default().push(p.pid);
        }
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }

    let kinds: HashMap<Pid, &'static str> = procs
        .values()
        .filter_map(|p| Some((p.pid, agent_kind(&p.name, &p.cmd)?)))
        .collect();
    let mut agents: Vec<Agent> = kinds
        .iter()
        // Agents started by another agent belong to that agent's tree.
        .filter(|&(&pid, _)| {
            let parent = procs.get(&pid).and_then(|p| p.parent);
            !std::iter::successors(parent, |p| procs.get(p)?.parent).any(|p| kinds.contains_key(&p))
        })
        .map(|(&pid, &kind)| {
            let mut tree = Vec::new();
            walk(procs, &children, pid, 0, &mut tree);
            Agent { kind, procs: tree }
        })
        .collect();
    agents.sort_by_key(|a| a.procs[0].pid);
    agents
}

fn walk(
    procs: &HashMap<Pid, ProcInfo>,
    children: &HashMap<Pid, Vec<Pid>>,
    pid: Pid,
    depth: usize,
    out: &mut Vec<Proc>,
) {
    let Some(p) = procs.get(&pid) else { return };
    out.push(Proc {
        pid,
        name: display_name(&p.name, &p.cmd),
        cpu: p.cpu,
        mem: p.mem,
        depth,
        start: p.start,
    });
    if let Some(kids) = children.get(&pid) {
        for &kid in kids {
            walk(procs, children, kid, depth + 1, out);
        }
    }
}

fn agent_kind(name: &OsStr, cmd: &[OsString]) -> Option<&'static str> {
    let program = cmd.first().map(|a| basename(a));
    if let Some(kind) = program.and_then(match_agent).or_else(|| match_agent(name)) {
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
    display_name(p.name(), p.cmd())
}

fn display_name(name: &OsStr, cmd: &[OsString]) -> String {
    cmd.first()
        .map_or(name, |a| basename(a))
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
    pub fn update(&mut self, procs: &HashMap<Pid, ProcInfo>, agents: &[Agent]) -> Vec<Orphan> {
        self.seen
            .retain(|pid, (_, start)| procs.get(pid).is_some_and(|p| p.start == *start));

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
                let p = procs.get(pid)?;
                Some(Orphan {
                    pid: *pid,
                    name: display_name(&p.name, &p.cmd),
                    agent,
                    cpu: p.cpu,
                    mem: p.mem,
                })
            })
            .collect();
        orphans.sort_by_key(|o| o.pid);
        orphans
    }
}
