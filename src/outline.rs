use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::agents::{Agent, Proc};
use crate::claude::Subagent;
use crate::focus::Key;

/// The agents section as rows: each agent, and below an open one its
/// subagents and processes. Only open roots are kept, so a new session
/// starts collapsed.
#[derive(Default)]
pub struct Outline {
    open: HashSet<Key>,
}

/// One row of the agents section and the key the cursor finds it by.
pub struct Entry<'a> {
    pub key: Key,
    pub line: Line<'a>,
}

pub enum Line<'a> {
    Root { agent: &'a Agent, open: bool },
    Subagent(&'a Subagent),
    Proc(&'a Proc),
}

/// What a tree key does to the tree under the agents cursor.
pub enum Fold {
    Toggle,
    Expand,
    /// On a child row, moves the cursor to its root and leaves the tree open.
    Collapse,
}

impl Outline {
    pub fn rows<'a>(&self, agents: &'a [Agent]) -> Vec<Entry<'a>> {
        let mut rows = Vec::new();
        for agent in agents {
            let root = agent.key();
            let open = self.open.contains(&root);
            rows.push(Entry {
                key: root,
                line: Line::Root { agent, open },
            });
            if !open {
                continue;
            }
            // above the processes, which can be many servers that would push
            // them out of a short pane. One level deep: a meta file does not
            // name the subagent that started it.
            rows.extend(agent.subagents.iter().map(|s| Entry {
                key: subagent_key(&s.id),
                line: Line::Subagent(s),
            }));
            rows.extend(agent.procs[1..].iter().map(|p| Entry {
                key: p.key(),
                line: Line::Proc(p),
            }));
        }
        rows
    }

    /// Expands or collapses the tree holding row `at`. Returns where the
    /// cursor goes when `at` no longer shows or the key moves it.
    pub fn fold(&mut self, agents: &[Agent], at: Key, fold: Fold) -> Option<Key> {
        let agent = tree_of(agents, at)?;
        let root = agent.key();
        match fold {
            Fold::Collapse if at != root => return Some(root),
            // kept closed, so children spawned later start collapsed
            _ if !agent.has_children() => {}
            Fold::Toggle => {
                if !self.open.remove(&root) {
                    self.open.insert(root);
                }
            }
            Fold::Expand => {
                self.open.insert(root);
            }
            Fold::Collapse => {
                self.open.remove(&root);
            }
        }
        self.hidden(at, root)
    }

    /// Forgets the sessions that exited or have no children left, so
    /// children spawned later start collapsed. Returns the root of `at` when
    /// its tree is closed: after `collapse_all`, or when the tree lost its
    /// children or `at` now sits under another root.
    pub fn sync(&mut self, agents: &[Agent], at: Option<Key>) -> Option<Key> {
        let roots: HashSet<Key> = agents
            .iter()
            .filter(|a| a.has_children())
            .map(Agent::key)
            .collect();
        self.open.retain(|k| roots.contains(k));
        let at = at?;
        self.hidden(at, tree_of(agents, at)?.key())
    }

    /// `root` when the row `at` below it sits in a closed tree.
    fn hidden(&self, at: Key, root: Key) -> Option<Key> {
        (at != root && !self.open.contains(&root)).then_some(root)
    }

    /// Childless roots stay closed, so children spawned later start
    /// collapsed.
    pub fn expand_all(&mut self, agents: &[Agent]) {
        self.open
            .extend(agents.iter().filter(|a| a.has_children()).map(Agent::key));
    }

    pub fn collapse_all(&mut self) {
        self.open.clear();
    }
}

/// `(u64::MAX, hash of the id)`: no pid reaches `u64::MAX`, so it never
/// names a process.
fn subagent_key(id: &str) -> Key {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    (u64::MAX, hasher.finish())
}

/// The session whose tree holds the process or subagent `of`.
fn tree_of(agents: &[Agent], of: Key) -> Option<&Agent> {
    agents
        .iter()
        .find(|a| a.holds(of) || a.subagents.iter().any(|s| subagent_key(&s.id) == of))
}

#[cfg(test)]
mod tests {
    use sysinfo::Pid;

    use super::*;
    use crate::focus::key;

    /// A session per root pid, each with one child at `root + 1`.
    fn agents(roots: &[u32]) -> Vec<Agent> {
        let proc = |pid: u32, depth| Proc {
            pid: Pid::from_u32(pid),
            name: String::new(),
            cpu: 0.0,
            mem: 0,
            depth,
            start: 7,
        };
        roots
            .iter()
            .map(|&pid| Agent {
                kind: "claude",
                project: String::new(),
                procs: vec![proc(pid, 0), proc(pid + 1, 1)],
                cpu: 0.0,
                cpu_busy: false,
                reported: None,
                session: None,
                session_id: None,
                subagents: Vec::new(),
            })
            .collect()
    }

    fn pid(pid: u32) -> Key {
        key(Pid::from_u32(pid), 7)
    }

    fn subagent(id: &str) -> Subagent {
        Subagent {
            id: id.into(),
            kind: "Explore".into(),
        }
    }

    /// The pid, or `a:` and the id, of each row.
    fn rows(outline: &Outline, agents: &[Agent]) -> Vec<String> {
        outline
            .rows(agents)
            .iter()
            .map(|e| match e.line {
                Line::Root { agent, .. } => agent.root().pid.to_string(),
                Line::Subagent(s) => format!("a:{}", s.id),
                Line::Proc(p) => p.pid.to_string(),
            })
            .collect()
    }

    #[test]
    fn sessions_start_collapsed() {
        let mut outline = Outline::default();
        outline.expand_all(&agents(&[10]));
        let now = agents(&[10, 20]);
        outline.sync(&now, None);
        assert_eq!(rows(&outline, &now), ["10", "11", "20"]);
    }

    #[test]
    fn exited_sessions_are_forgotten() {
        let mut outline = Outline::default();
        outline.expand_all(&agents(&[10, 20]));
        outline.sync(&agents(&[20]), None);
        // pid 10 comes back as a new session
        assert_eq!(rows(&outline, &agents(&[10, 20])), ["10", "20", "21"]);
    }

    #[test]
    fn a_tree_whose_children_exited_closes() {
        let mut outline = Outline::default();
        outline.expand_all(&agents(&[10]));
        let mut now = agents(&[10]);
        now[0].procs.truncate(1);
        outline.sync(&now, None);
        assert_eq!(rows(&outline, &agents(&[10])), ["10"]);
    }

    #[test]
    fn subagents_alone_keep_a_tree_open() {
        let mut outline = Outline::default();
        let mut now = agents(&[10]);
        now[0].procs.truncate(1);
        now[0].subagents.push(subagent("a1"));
        outline.expand_all(&now);
        outline.sync(&now, None);
        assert_eq!(rows(&outline, &now), ["10", "a:a1"]);
        now[0].subagents.clear();
        outline.sync(&now, None);
        now[0].subagents.push(subagent("a1"));
        assert_eq!(rows(&outline, &now), ["10"]);
    }

    #[test]
    fn subagents_show_above_the_processes_of_their_agent() {
        let mut outline = Outline::default();
        let mut now = agents(&[10]);
        now[0].subagents = vec![subagent("a1"), subagent("a2")];
        outline.expand_all(&now);
        assert_eq!(rows(&outline, &now), ["10", "a:a1", "a:a2", "11"]);
    }

    #[test]
    fn expand_all_leaves_childless_roots_closed() {
        let mut outline = Outline::default();
        let mut now = agents(&[10, 20]);
        now[1].procs.truncate(1);
        outline.expand_all(&now);
        // 20 gets a child: it starts collapsed
        let later = agents(&[10, 20]);
        assert_eq!(rows(&outline, &later), ["10", "11", "20"]);
    }

    #[test]
    fn collapse_then_expand_all_restores_every_tree() {
        let mut outline = Outline::default();
        let now = agents(&[10, 20]);
        outline.expand_all(&now);
        outline.collapse_all();
        assert_eq!(rows(&outline, &now), ["10", "20"]);
        outline.expand_all(&now);
        assert_eq!(rows(&outline, &now), ["10", "11", "20", "21"]);
    }

    #[test]
    fn toggle_flips_one_tree() {
        let mut outline = Outline::default();
        let now = agents(&[10, 20]);
        assert_eq!(outline.fold(&now, pid(10), Fold::Toggle), None);
        assert_eq!(rows(&outline, &now), ["10", "11", "20"]);
        assert_eq!(outline.fold(&now, pid(10), Fold::Toggle), None);
        assert_eq!(rows(&outline, &now), ["10", "20"]);
    }

    #[test]
    fn expand_and_collapse_on_a_root_keep_the_cursor() {
        let mut outline = Outline::default();
        let now = agents(&[10]);
        assert_eq!(outline.fold(&now, pid(10), Fold::Expand), None);
        assert_eq!(outline.fold(&now, pid(10), Fold::Expand), None);
        assert_eq!(rows(&outline, &now), ["10", "11"]);
        assert_eq!(outline.fold(&now, pid(10), Fold::Collapse), None);
        assert_eq!(rows(&outline, &now), ["10"]);
    }

    #[test]
    fn childless_roots_stay_closed() {
        let mut outline = Outline::default();
        let mut now = agents(&[10]);
        now[0].procs.truncate(1);
        outline.fold(&now, pid(10), Fold::Toggle);
        // a child spawns later: it starts collapsed
        assert_eq!(rows(&outline, &agents(&[10])), ["10"]);
    }

    #[test]
    fn collapse_on_a_child_moves_the_cursor_to_its_root() {
        let mut outline = Outline::default();
        let now = agents(&[10, 20]);
        outline.expand_all(&now);
        assert_eq!(outline.fold(&now, pid(21), Fold::Collapse), Some(pid(20)));
        assert_eq!(rows(&outline, &now), ["10", "11", "20", "21"]);
    }

    #[test]
    fn collapse_on_a_subagent_moves_the_cursor_to_its_root() {
        let mut outline = Outline::default();
        let mut now = agents(&[10, 20]);
        now[1].subagents.push(subagent("a1"));
        outline.expand_all(&now);
        let a1 = subagent_key("a1");
        assert_eq!(outline.fold(&now, a1, Fold::Collapse), Some(pid(20)));
        assert_eq!(outline.fold(&now, subagent_key("a2"), Fold::Collapse), None);
    }

    #[test]
    fn toggle_on_a_child_closes_its_tree_and_moves_the_cursor_at_once() {
        let mut outline = Outline::default();
        let now = agents(&[10, 20]);
        outline.expand_all(&now);
        assert_eq!(outline.fold(&now, pid(11), Fold::Toggle), Some(pid(10)));
        assert_eq!(rows(&outline, &now), ["10", "20", "21"]);
    }

    #[test]
    fn expand_on_a_child_keeps_the_cursor() {
        let mut outline = Outline::default();
        let now = agents(&[10]);
        outline.expand_all(&now);
        assert_eq!(outline.fold(&now, pid(11), Fold::Expand), None);
    }

    #[test]
    fn fold_on_an_unknown_row_does_nothing() {
        let mut outline = Outline::default();
        let now = agents(&[10]);
        // pid 11 with another start time is not this session's child
        let stranger = key(Pid::from_u32(11), 8);
        assert_eq!(outline.fold(&now, stranger, Fold::Toggle), None);
        assert_eq!(rows(&outline, &now), ["10"]);
    }

    #[test]
    fn sync_moves_a_cursor_on_a_collapsed_child_to_its_root() {
        let mut outline = Outline::default();
        let now = agents(&[10, 20]);
        outline.expand_all(&now);
        assert_eq!(outline.sync(&now, Some(pid(21))), None);
        outline.collapse_all();
        assert_eq!(outline.sync(&now, Some(pid(21))), Some(pid(20)));
        assert_eq!(outline.sync(&now, Some(pid(20))), None);
        assert_eq!(outline.sync(&now, None), None);
    }

    #[test]
    fn open_trees_stay_open_across_ticks() {
        let mut outline = Outline::default();
        outline.fold(&agents(&[10, 20]), pid(10), Fold::Toggle);
        for _ in 0..3 {
            outline.sync(&agents(&[10, 20]), Some(pid(10)));
        }
        outline.fold(&agents(&[10, 20]), pid(20), Fold::Expand);
        assert_eq!(rows(&outline, &agents(&[10, 20])), ["10", "11", "20", "21"]);
    }
}
