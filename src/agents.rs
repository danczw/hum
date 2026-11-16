use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

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
    /// None when /proc/<pid>/cwd is unreadable.
    pub cwd: Option<PathBuf>,
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
                cwd: p.cwd().map(Path::to_path_buf),
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
    /// Named after the agent process's cwd, see [`project`].
    pub project: String,
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

pub fn scan(procs: &HashMap<Pid, ProcInfo>, home: Option<&Path>) -> Vec<Agent> {
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
            Agent {
                kind,
                project: project(procs[&pid].cwd.as_deref(), home),
                procs: tree,
            }
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

/// Names a session's project after its working directory: `~` for the home
/// directory, `?` when the cwd is unreadable. A deleted cwd keeps the
/// kernel's ` (deleted)` suffix, e.g. `old (deleted)`.
pub fn project(cwd: Option<&Path>, home: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return "?".into();
    };
    if home == Some(cwd) {
        return "~".into();
    }
    cwd.file_name()
        .map_or_else(|| cwd.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(cmd: &str) -> Option<&'static str> {
        let cmd: Vec<OsString> = cmd.split(' ').map(OsString::from).collect();
        agent_kind(OsStr::new(""), &cmd)
    }

    fn proc(pid: u32, parent: u32, start: u64, cmd: &str) -> (Pid, ProcInfo) {
        let cmd: Vec<OsString> = cmd.split(' ').map(OsString::from).collect();
        let info = ProcInfo {
            pid: Pid::from_u32(pid),
            parent: Some(Pid::from_u32(parent)),
            start,
            name: cmd[0].clone(),
            cmd,
            cpu: 0.0,
            mem: 0,
            cwd: None,
        };
        (info.pid, info)
    }

    fn in_dir((pid, mut info): (Pid, ProcInfo), cwd: &str) -> (Pid, ProcInfo) {
        info.cwd = Some(PathBuf::from(cwd));
        (pid, info)
    }

    fn home() -> Option<&'static Path> {
        Some(Path::new("/home/u"))
    }

    fn pids(orphans: &[Orphan]) -> Vec<u32> {
        orphans.iter().map(|o| o.pid.as_u32()).collect()
    }

    #[test]
    fn direct_binary() {
        assert_eq!(kind("/home/u/.local/bin/claude --resume"), Some("claude"));
        assert_eq!(kind("bash"), None);
    }

    #[test]
    fn script_under_runtime() {
        assert_eq!(kind("node /usr/bin/codex"), Some("codex"));
        assert_eq!(kind("bun run opencode"), Some("opencode"));
        assert_eq!(kind("python script.py"), None);
    }

    #[test]
    fn name_when_cmd_is_empty() {
        assert_eq!(agent_kind(OsStr::new("claude"), &[]), Some("claude"));
    }

    // aaf6466
    #[test]
    fn versioned_runtime_and_flags() {
        assert_eq!(kind("python3.12 -m aider"), Some("aider"));
        assert_eq!(kind("node --no-warnings /usr/bin/gemini"), Some("gemini"));
        assert_eq!(kind("python3.12 -X dev -m aider"), Some("aider"));
    }

    // 89a7f15
    #[test]
    fn cmd_wins_over_stale_name() {
        let cmd = [OsString::from("claude")];
        assert_eq!(agent_kind(OsStr::new("node"), &cmd), Some("claude"));
        assert_eq!(display_name(OsStr::new("node"), &cmd), "claude");
    }

    // 81e6388
    #[test]
    fn agent_name_in_script_arguments() {
        assert_eq!(kind("python -m pytest tests/gemini"), None);
        assert_eq!(kind("node server.js --model claude"), None);
    }

    #[test]
    fn nested_agent_joins_parent_tree() {
        let procs = HashMap::from([
            proc(10, 1, 100, "claude"),
            proc(11, 10, 101, "bash"),
            proc(12, 11, 102, "codex"),
        ]);
        let agents = scan(&procs, home());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].kind, "claude");
        let tree: Vec<(u32, usize)> = agents[0]
            .procs
            .iter()
            .map(|p| (p.pid.as_u32(), p.depth))
            .collect();
        assert_eq!(tree, [(10, 0), (11, 1), (12, 2)]);
    }

    #[test]
    fn sessions_named_after_root_cwd() {
        let procs = HashMap::from([
            in_dir(proc(10, 1, 100, "claude"), "/home/u/code/a"),
            in_dir(proc(11, 10, 101, "bash"), "/tmp"),
            in_dir(proc(20, 1, 200, "claude"), "/home/u/code/b"),
            proc(30, 1, 300, "codex"),
        ]);
        let projects: Vec<String> = scan(&procs, home())
            .into_iter()
            .map(|a| a.project)
            .collect();
        assert_eq!(projects, ["a", "b", "?"]);
    }

    #[test]
    fn child_left_behind_is_orphan() {
        let mut tracker = OrphanTracker::default();
        let procs = HashMap::from([proc(10, 1, 100, "claude"), proc(11, 10, 101, "vite")]);
        assert!(tracker.update(&procs, &scan(&procs, home())).is_empty());

        let procs = HashMap::from([proc(11, 1, 101, "vite")]);
        let orphans = tracker.update(&procs, &scan(&procs, home()));
        assert_eq!(pids(&orphans), [11]);
        assert_eq!(orphans[0].agent, "claude");
    }

    #[test]
    fn reused_pid_is_not_orphan() {
        let mut tracker = OrphanTracker::default();
        let procs = HashMap::from([proc(10, 1, 100, "claude"), proc(11, 10, 101, "vite")]);
        tracker.update(&procs, &scan(&procs, home()));

        let procs = HashMap::from([proc(11, 1, 500, "vite")]);
        assert!(tracker.update(&procs, &scan(&procs, home())).is_empty());
    }

    // b4a0036
    #[test]
    fn nested_agent_after_parent_exit_is_root() {
        let mut tracker = OrphanTracker::default();
        let procs = HashMap::from([
            proc(10, 1, 100, "claude"),
            proc(11, 10, 101, "bash"),
            proc(12, 11, 102, "codex"),
        ]);
        tracker.update(&procs, &scan(&procs, home()));

        let procs = HashMap::from([proc(11, 1, 101, "bash"), proc(12, 11, 102, "codex")]);
        let agents = scan(&procs, home());
        assert_eq!(agents[0].procs[0].pid.as_u32(), 12);
        assert_eq!(pids(&tracker.update(&procs, &agents)), [11]);
    }

    #[test]
    fn project_is_cwd_basename() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(project(Some(Path::new("/home/u/code/hum")), home), "hum");
        assert_eq!(project(Some(Path::new("/")), home), "/");
    }

    #[test]
    fn project_at_home_is_tilde() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(project(Some(Path::new("/home/u")), home), "~");
        assert_eq!(project(Some(Path::new("/home/u/")), home), "~");
        assert_eq!(project(Some(Path::new("/home/u")), None), "u");
    }

    #[test]
    fn unreadable_cwd_is_question_mark() {
        assert_eq!(project(None, Some(Path::new("/home/u"))), "?");
    }

    #[test]
    fn deleted_cwd_keeps_suffix() {
        let home = Some(Path::new("/home/u"));
        let cwd = Path::new("/home/u/code/old (deleted)");
        assert_eq!(project(Some(cwd), home), "old (deleted)");
        let cwd = Path::new("/home/u (deleted)");
        assert_eq!(project(Some(cwd), home), "u (deleted)");
    }
}
