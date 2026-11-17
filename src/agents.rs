use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use sysinfo::{Pid, Process, System};

use crate::focus::{Key, key};

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

/// Names added with `--agent`, matched after [`AGENTS`].
static EXTRA: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Adds agent names to match; only the first call counts.
pub fn set_extra(names: Vec<&'static str>) {
    let _ = EXTRA.set(names);
}

/// Interpreters that may run an agent as a script, e.g. `node /usr/bin/codex`.
/// Version suffixes are ignored, so `python3.12` counts as `python`.
const RUNTIMES: &[&str] = &["node", "bun", "deno", "python"];

/// Env vars an agent sets in every process it spawns, holding the agent's
/// pid, by agent kind. Add one only once verified on a real session.
const MARKERS: &[(&str, &str)] = &[("claude", "CLAUDE_PID")];

/// An agent kind and the agent pid its marker holds.
type Marker = (&'static str, Pid);

/// Span of tree cpu averaged to call a session busy.
const BUSY_SPAN: Duration = Duration::from_secs(5);
/// Mean tree cpu, in cores, above which a session counts as busy. A claude
/// session idle at its prompt measured 0.006 to 0.024, one waiting on a tool
/// 0.04 to 0.07.
const BUSY_CORES: f32 = 0.03;

/// The process fields detection needs, copied out of sysinfo so detection
/// runs on plain data.
#[cfg_attr(test, derive(Clone))]
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
    pub start: u64,
}

pub struct Agent {
    pub kind: &'static str,
    /// Named after the agent process's cwd, see [`project`].
    pub project: String,
    /// The agent process first, then its descendants depth-first.
    pub procs: Vec<Proc>,
    /// Set by [`BusyTracker::update`]; false until a window of samples exists.
    pub busy: bool,
}

impl Agent {
    /// The session as `<kind>/<project>`, e.g. `claude/hum`.
    pub fn label(&self) -> String {
        format!("{}/{}", self.kind, self.project)
    }

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
                busy: false,
            }
        })
        .collect();
    // busiest first; pid keeps idle sessions in a steady order
    agents.sort_by(|a, b| {
        b.cpu()
            .total_cmp(&a.cpu())
            .then(a.procs[0].pid.cmp(&b.procs[0].pid))
    });
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
    agent_kind_in(name, cmd, EXTRA.get().map_or(&[], Vec::as_slice))
}

fn agent_kind_in(name: &OsStr, cmd: &[OsString], extra: &[&'static str]) -> Option<&'static str> {
    let match_agent = |name: &OsStr| match_agent(name, extra);
    let program = cmd.first().map(|a| basename(a));
    if let Some(kind) = program.and_then(match_agent).or_else(|| match_agent(name)) {
        return Some(kind);
    }
    if !is_runtime(&program?.to_string_lossy()) {
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

/// True for an interpreter in [`RUNTIMES`], with or without a version
/// suffix.
pub fn is_runtime(program: &str) -> bool {
    RUNTIMES.contains(&program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.'))
}

fn match_agent(name: &OsStr, extra: &[&'static str]) -> Option<&'static str> {
    AGENTS
        .iter()
        .chain(extra)
        .copied()
        .find(|a| OsStr::new(a) == name)
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

/// Resolves `$HOME` so a symlinked home still matches the kernel's resolved
/// cwd. Falls back to the raw value when it cannot be resolved.
pub fn canonical_home(raw: Option<OsString>) -> Option<PathBuf> {
    let raw = PathBuf::from(raw?);
    Some(raw.canonicalize().unwrap_or(raw))
}

/// Names a session's project after its working directory: `~` for the home
/// directory, `?` when the cwd is unreadable. A deleted cwd keeps the
/// kernel's ` (deleted)` suffix, e.g. `old (deleted)`.
fn project(cwd: Option<&Path>, home: Option<&Path>) -> String {
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
    /// The session that spawned it, see [`Agent::label`].
    pub from: String,
    pub start: u64,
    pub cpu: f32,
    pub mem: u64,
}

/// Remembers which processes agents spawned, to spot the ones left behind.
/// Only catches orphans whose agent tree changed while hum was running, so
/// it covers agents without a [`MARKERS`] entry.
#[derive(Default)]
pub struct OrphanTracker {
    /// pid -> (spawning session's label, start time); start time guards
    /// against pid reuse.
    /// The label is stored so it survives the agent exiting.
    seen: HashMap<Pid, (String, u64)>,
}

impl OrphanTracker {
    pub fn update(&mut self, procs: &HashMap<Pid, ProcInfo>, agents: &[Agent]) -> Vec<Orphan> {
        self.seen
            .retain(|pid, (_, start)| procs.get(pid).is_some_and(|p| p.start == *start));

        let mut current = HashSet::new();
        for agent in agents {
            let label = agent.label();
            for p in &agent.procs[1..] {
                current.insert(p.pid);
                self.seen.insert(p.pid, (label.clone(), p.start));
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
            .filter_map(|(pid, (from, start))| {
                let p = procs.get(pid)?;
                Some(Orphan {
                    pid: *pid,
                    name: display_name(&p.name, &p.cmd),
                    from: from.clone(),
                    start: *start,
                    cpu: p.cpu,
                    mem: p.mem,
                })
            })
            .collect();
        orphans.sort_by_key(|o| o.pid);
        orphans
    }
}

/// Finds orphans by the [`MARKERS`] their agent left in their environment,
/// including ones from before hum started.
pub struct EnvOrphans {
    /// hum itself inherits the markers when started from an agent's shell.
    me: Pid,
    /// pid -> (start time, marker); start time guards against pid reuse.
    /// The marker is None when unset or unreadable. The environment is
    /// fixed at exec, so each process is read once.
    markers: HashMap<Pid, (u64, Option<Marker>)>,
}

impl EnvOrphans {
    pub fn new(me: Pid) -> Self {
        Self {
            me,
            markers: HashMap::new(),
        }
    }

    /// Lists marked processes outside every agent tree, unsorted. `read`
    /// returns a pid's raw environ, see [`read_environ`].
    pub fn update(
        &mut self,
        procs: &HashMap<Pid, ProcInfo>,
        agents: &[Agent],
        home: Option<&Path>,
        mut read: impl FnMut(Pid) -> Option<Vec<u8>>,
    ) -> Vec<Orphan> {
        self.markers
            .retain(|pid, (start, _)| procs.get(pid).is_some_and(|p| p.start == *start));

        let tracked: HashSet<Pid> = agents
            .iter()
            .flat_map(|a| a.procs.iter().map(|p| p.pid))
            .collect();
        procs
            .values()
            .filter(|p| p.pid != self.me && !tracked.contains(&p.pid))
            .filter_map(|p| {
                let (kind, agent) = self
                    .markers
                    .entry(p.pid)
                    .or_insert_with(|| (p.start, read(p.pid).and_then(|env| marker(&env))))
                    .1?;
                // Still under its agent, which scan did not recognise as a
                // session, e.g. `node cli.js`.
                if std::iter::successors(p.parent, |p| procs.get(p)?.parent).any(|p| p == agent) {
                    return None;
                }
                // A reused pid is younger than the processes it marks.
                let agent = procs
                    .get(&agent)
                    .filter(|a| a.start <= p.start && agent_kind(&a.name, &a.cmd) == Some(kind));
                let project = agent.map_or_else(|| "?".into(), |a| project(a.cwd.as_deref(), home));
                Some(Orphan {
                    pid: p.pid,
                    name: display_name(&p.name, &p.cmd),
                    from: format!("{kind}/{project}"),
                    start: p.start,
                    cpu: p.cpu,
                    mem: p.mem,
                })
            })
            .collect()
    }
}

pub fn read_environ(pid: Pid) -> Option<Vec<u8>> {
    fs::read(format!("/proc/{pid}/environ")).ok()
}

/// The first [`MARKERS`] entry set in a raw `/proc/<pid>/environ`.
fn marker(environ: &[u8]) -> Option<Marker> {
    environ.split(|&b| b == 0).find_map(|var| {
        let var = std::str::from_utf8(var).ok()?;
        let (name, value) = var.split_once('=')?;
        let &(kind, _) = MARKERS.iter().find(|(_, m)| *m == name)?;
        Some((kind, Pid::from_u32(value.parse().ok()?)))
    })
}

/// Joins both orphan lists by pid. The tracker's entry wins: it keeps the
/// project after the agent exits.
pub fn merge(mut tracked: Vec<Orphan>, env: Vec<Orphan>) -> Vec<Orphan> {
    let seen: HashSet<Pid> = tracked.iter().map(|o| o.pid).collect();
    tracked.extend(env.into_iter().filter(|o| !seen.contains(&o.pid)));
    tracked.sort_by_key(|o| o.pid);
    tracked
}

/// Remembers each session's recent tree cpu to tell busy from idle.
pub struct BusyTracker {
    /// (root pid, start time) -> tree cpu per tick, newest last; start time
    /// guards against pid reuse.
    samples: HashMap<Key, VecDeque<f32>>,
    /// Ticks averaged, see [`busy_window`].
    window: usize,
}

/// Ticks that cover [`BUSY_SPAN`] at `tick`, at least one.
pub fn busy_window(tick: Duration) -> usize {
    (BUSY_SPAN.as_secs_f64() / tick.as_secs_f64())
        .ceil()
        .max(1.0) as usize
}

#[cfg(test)]
impl Default for BusyTracker {
    fn default() -> Self {
        Self::new(busy_window(Duration::from_secs(1)))
    }
}

impl BusyTracker {
    pub fn new(window: usize) -> Self {
        Self {
            samples: HashMap::new(),
            window,
        }
    }

    pub fn update(&mut self, agents: &mut [Agent]) {
        let roots: HashSet<Key> = agents
            .iter()
            .map(|a| key(a.procs[0].pid, a.procs[0].start))
            .collect();
        self.samples.retain(|root, _| roots.contains(root));
        for agent in agents {
            let root = &agent.procs[0];
            let window = self.samples.entry(key(root.pid, root.start)).or_default();
            if window.len() == self.window {
                window.pop_front();
            }
            window.push_back(agent.cpu());
            // cpu is in percent of one core
            let mean = window.iter().sum::<f32>() / window.len() as f32 / 100.0;
            agent.busy = window.len() == self.window && mean > BUSY_CORES;
        }
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
    fn extra_agent_as_binary_and_script() {
        let extra_kind = |cmd: &str| {
            let cmd: Vec<OsString> = cmd.split(' ').map(OsString::from).collect();
            agent_kind_in(OsStr::new(""), &cmd, &["goose"])
        };
        assert_eq!(extra_kind("goose"), Some("goose"));
        assert_eq!(extra_kind("node /x/goose"), Some("goose"));
        assert_eq!(kind("goose"), None);
    }

    #[test]
    fn busy_window_spans_five_seconds() {
        assert_eq!(busy_window(Duration::from_millis(500)), 10);
        assert_eq!(busy_window(Duration::from_secs(1)), 5);
        assert_eq!(busy_window(Duration::from_secs(10)), 1);
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
    fn busiest_session_sorts_first() {
        let busy = |(pid, mut info): (Pid, ProcInfo), cpu| {
            info.cpu = cpu;
            (pid, info)
        };
        let procs = HashMap::from([
            proc(10, 1, 100, "claude"),
            proc(20, 1, 200, "claude"),
            busy(proc(21, 20, 201, "cargo"), 50.0),
            proc(30, 1, 300, "codex"),
        ]);
        let roots: Vec<u32> = scan(&procs, home())
            .iter()
            .map(|a| a.procs[0].pid.as_u32())
            .collect();
        assert_eq!(roots, [20, 10, 30]);
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
        assert_eq!(orphans[0].from, "claude/?");
    }

    #[test]
    fn orphan_keeps_project_after_agent_exits() {
        let mut tracker = OrphanTracker::default();
        let procs = HashMap::from([
            in_dir(proc(10, 1, 100, "claude"), "/home/u/code/a"),
            proc(11, 10, 101, "vite"),
        ]);
        tracker.update(&procs, &scan(&procs, home()));

        let procs = HashMap::from([proc(11, 1, 101, "vite")]);
        let orphans = tracker.update(&procs, &scan(&procs, home()));
        assert_eq!(orphans[0].from, "claude/a");
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

    fn environ(vars: &[&str]) -> Vec<u8> {
        vars.join("\0").into_bytes()
    }

    /// Env orphans when `marked` pids carry `CLAUDE_PID=agent`; hum runs as
    /// pid 99.
    fn env_orphans(procs: &HashMap<Pid, ProcInfo>, marked: &[u32], agent: u32) -> Vec<Orphan> {
        let mut env = EnvOrphans::new(Pid::from_u32(99));
        let read = |pid: Pid| {
            let var = format!("CLAUDE_PID={agent}");
            marked
                .contains(&pid.as_u32())
                .then(|| environ(&["PATH=/bin", &var]))
        };
        env.update(procs, &scan(procs, home()), home(), read)
    }

    #[test]
    fn marker_names_agent_pid() {
        let pid = Some(("claude", Pid::from_u32(965)));
        assert_eq!(marker(&environ(&["A=1", "CLAUDE_PID=965", "B=2"])), pid);
        assert_eq!(marker(&environ(&["CLAUDECODE=1"])), None);
        assert_eq!(marker(&environ(&["CLAUDE_PID=x"])), None);
        assert_eq!(marker(b""), None);
    }

    #[test]
    fn marked_process_outside_tree_is_orphan() {
        let procs = HashMap::from([
            in_dir(proc(10, 1, 100, "claude"), "/home/u/code/a"),
            proc(11, 1, 101, "tail"),
        ]);
        let orphans = env_orphans(&procs, &[11], 10);
        assert_eq!(pids(&orphans), [11]);
        assert_eq!(orphans[0].from, "claude/a");
    }

    #[test]
    fn marked_process_of_closed_session_is_unknown() {
        let procs = HashMap::from([proc(11, 1, 101, "tail")]);
        assert_eq!(env_orphans(&procs, &[11], 10)[0].from, "claude/?");
    }

    #[test]
    fn reused_agent_pid_does_not_name_project() {
        // pid 10 now runs something else, or a claude started later
        for procs in [
            HashMap::from([
                in_dir(proc(10, 1, 100, "bash"), "/a"),
                proc(11, 1, 101, "tail"),
            ]),
            HashMap::from([
                in_dir(proc(10, 1, 200, "claude"), "/a"),
                proc(11, 1, 101, "tail"),
            ]),
        ] {
            assert_eq!(env_orphans(&procs, &[11], 10)[0].from, "claude/?");
        }
    }

    #[test]
    fn marked_hum_tree_and_roots_are_not_orphans() {
        let procs = HashMap::from([
            proc(10, 1, 100, "claude"),
            proc(11, 10, 101, "bash"),
            proc(20, 1, 200, "claude"),
            proc(99, 11, 199, "hum"),
        ]);
        assert!(env_orphans(&procs, &[11, 20, 99], 10).is_empty());
    }

    #[test]
    fn marked_child_of_unrecognised_claude_is_not_orphan() {
        let procs = HashMap::from([proc(20, 1, 200, "node cli.js"), proc(21, 20, 201, "bash")]);
        assert!(env_orphans(&procs, &[21], 20).is_empty());
    }

    #[test]
    fn unmarked_processes_are_not_orphans() {
        let procs = HashMap::from([proc(11, 1, 101, "tail")]);
        assert!(env_orphans(&procs, &[], 10).is_empty());
    }

    #[test]
    fn environ_read_once_per_process() {
        let mut env = EnvOrphans::new(Pid::from_u32(99));
        let mut reads = Vec::new();
        let mut tick = |env: &mut EnvOrphans, procs: &HashMap<Pid, ProcInfo>| {
            let agents = scan(procs, home());
            env.update(procs, &agents, home(), |pid| {
                reads.push(pid.as_u32());
                // 12 is another user's process
                (pid.as_u32() != 12).then(|| environ(&["CLAUDE_PID=10"]))
            });
        };
        let procs = HashMap::from([
            proc(10, 1, 100, "claude"),
            proc(11, 10, 101, "bash"),
            proc(12, 1, 102, "sshd"),
        ]);
        tick(&mut env, &procs);
        tick(&mut env, &procs);
        // 11 leaves the tree, 12 is a new process on a reused pid
        let procs = HashMap::from([proc(11, 1, 101, "bash"), proc(12, 1, 500, "tail")]);
        tick(&mut env, &procs);
        tick(&mut env, &procs);
        reads.sort_unstable();
        assert_eq!(reads, [11, 12, 12]);
    }

    #[test]
    fn tracked_orphan_wins_merge() {
        let orphan = |pid: u32, from: &str| Orphan {
            pid: Pid::from_u32(pid),
            name: "tail".into(),
            from: from.into(),
            start: 0,
            cpu: 0.0,
            mem: 0,
        };
        let merged = merge(
            vec![orphan(20, "claude/a")],
            vec![
                orphan(30, "claude/?"),
                orphan(20, "claude/?"),
                orphan(10, "claude/?"),
            ],
        );
        assert_eq!(pids(&merged), [10, 20, 30]);
        assert_eq!(merged[1].from, "claude/a");
    }

    /// One claude session at `pid` whose tree uses `cores`.
    fn session(pid: u32, start: u64, cores: f32) -> Vec<Agent> {
        let (pid, mut info) = proc(pid, 1, start, "claude");
        info.cpu = cores * 100.0;
        scan(&HashMap::from([(pid, info)]), home())
    }

    const WINDOW: usize = 5;

    /// Feeds `ticks` samples of `cores` and returns whether the last one
    /// called the session busy.
    fn busy_after(busy: &mut BusyTracker, ticks: usize, cores: f32) -> bool {
        let mut agents = Vec::new();
        for _ in 0..ticks {
            agents = session(10, 100, cores);
            busy.update(&mut agents);
        }
        agents[0].busy
    }

    #[test]
    fn new_session_idle_until_window_fills() {
        let mut busy = BusyTracker::new(WINDOW);
        assert!(!busy_after(&mut busy, WINDOW - 1, 1.0));
        assert!(busy_after(&mut busy, 1, 1.0));
    }

    #[test]
    fn quiet_session_turns_idle_within_window() {
        let mut busy = BusyTracker::new(WINDOW);
        assert!(busy_after(&mut busy, WINDOW, 1.0));
        assert!(!busy_after(&mut busy, WINDOW, 0.0));
        assert!(!busy_after(&mut busy, WINDOW, BUSY_CORES));
    }

    #[test]
    fn exited_session_drops_history() {
        let mut busy = BusyTracker::new(WINDOW);
        busy_after(&mut busy, WINDOW, 1.0);
        busy.update(&mut []);
        assert!(busy.samples.is_empty());
    }

    #[test]
    fn reused_pid_starts_fresh() {
        let mut busy = BusyTracker::new(WINDOW);
        busy_after(&mut busy, WINDOW, 1.0);
        let mut agents = session(10, 500, 1.0);
        busy.update(&mut agents);
        assert!(!agents[0].busy);
    }

    #[test]
    fn project_is_cwd_basename() {
        assert_eq!(project(Some(Path::new("/home/u/code/hum")), home()), "hum");
        assert_eq!(project(Some(Path::new("/")), home()), "/");
    }

    #[test]
    fn project_at_home_is_tilde() {
        assert_eq!(project(Some(Path::new("/home/u")), home()), "~");
        assert_eq!(project(Some(Path::new("/home/u/")), home()), "~");
        assert_eq!(project(Some(Path::new("/home/u")), None), "u");
    }

    #[test]
    fn symlinked_home_resolves_to_target() {
        let dir = std::env::temp_dir().join(format!("hum-home-{}", std::process::id()));
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // temp_dir itself may sit behind a symlink.
        let expected = real.canonicalize().unwrap();
        let resolved = canonical_home(Some(link.into_os_string()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(resolved, Some(expected));
    }

    #[test]
    fn unresolvable_home_kept_as_is() {
        let raw = OsString::from("/nonexistent/hum/home");
        assert_eq!(
            canonical_home(Some(raw)),
            Some(PathBuf::from("/nonexistent/hum/home"))
        );
        assert_eq!(canonical_home(None), None);
    }

    #[test]
    fn unreadable_cwd_is_question_mark() {
        assert_eq!(project(None, home()), "?");
    }

    #[test]
    fn deleted_cwd_keeps_suffix() {
        let cwd = Path::new("/home/u/code/old (deleted)");
        assert_eq!(project(Some(cwd), home()), "old (deleted)");
        let cwd = Path::new("/home/u (deleted)");
        assert_eq!(project(Some(cwd), home()), "u (deleted)");
    }
}
