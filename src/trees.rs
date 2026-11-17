use std::collections::HashSet;

use crate::agents::Agent;
use crate::focus::{Key, key};

/// Which agent trees show their descendants, by root process. Only expanded
/// roots are kept, so a new session starts collapsed.
#[derive(Default)]
pub struct Trees {
    open: HashSet<Key>,
}

impl Trees {
    pub fn is_open(&self, root: Key) -> bool {
        self.open.contains(&root)
    }

    pub fn toggle(&mut self, root: Key) {
        if !self.open.remove(&root) {
            self.open.insert(root);
        }
    }

    pub fn expand(&mut self, root: Key) {
        self.open.insert(root);
    }

    pub fn collapse(&mut self, root: Key) {
        self.open.remove(&root);
    }

    /// Childless roots stay closed, so children spawned later start
    /// collapsed.
    pub fn expand_all(&mut self, agents: &[Agent]) {
        self.open
            .extend(agents.iter().filter(|a| a.procs.len() > 1).map(root));
    }

    pub fn collapse_all(&mut self) {
        self.open.clear();
    }

    /// Forgets the sessions that exited or have no children left, so
    /// children spawned later start collapsed.
    pub fn retain(&mut self, agents: &[Agent]) {
        let roots: HashSet<Key> = agents
            .iter()
            .filter(|a| a.procs.len() > 1)
            .map(root)
            .collect();
        self.open.retain(|k| roots.contains(k));
    }
}

pub fn root(agent: &Agent) -> Key {
    let p = &agent.procs[0];
    key(p.pid, p.start)
}

/// The session whose tree holds the process `of`.
pub fn tree_of(agents: &[Agent], of: Key) -> Option<&Agent> {
    agents
        .iter()
        .find(|a| a.procs.iter().any(|p| key(p.pid, p.start) == of))
}

#[cfg(test)]
mod tests {
    use sysinfo::Pid;

    use super::*;
    use crate::agents::Proc;

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
                busy: false,
            })
            .collect()
    }

    #[test]
    fn sessions_start_collapsed() {
        let mut trees = Trees::default();
        trees.expand_all(&agents(&[10]));
        let now = agents(&[10, 20]);
        trees.retain(&now);
        assert!(trees.is_open(root(&now[0])));
        assert!(!trees.is_open(root(&now[1])));
    }

    #[test]
    fn exited_sessions_are_forgotten() {
        let mut trees = Trees::default();
        trees.expand_all(&agents(&[10, 20]));
        trees.retain(&agents(&[20]));
        // pid 10 comes back as a new session
        let now = agents(&[10, 20]);
        assert!(!trees.is_open(root(&now[0])));
        assert!(trees.is_open(root(&now[1])));
    }

    #[test]
    fn a_tree_whose_children_exited_closes() {
        let mut trees = Trees::default();
        trees.expand_all(&agents(&[10]));
        let mut now = agents(&[10]);
        now[0].procs.truncate(1);
        trees.retain(&now);
        assert!(!trees.is_open(root(&agents(&[10])[0])));
    }

    #[test]
    fn toggle_flips_one_tree() {
        let mut trees = Trees::default();
        let now = agents(&[10, 20]);
        trees.toggle(root(&now[0]));
        assert!(trees.is_open(root(&now[0])));
        assert!(!trees.is_open(root(&now[1])));
        trees.toggle(root(&now[0]));
        assert!(!trees.is_open(root(&now[0])));
    }

    #[test]
    fn tree_of_finds_the_session_of_a_child() {
        let now = agents(&[10, 20]);
        let child = key(Pid::from_u32(21), 7);
        assert_eq!(tree_of(&now, child).map(root), Some(root(&now[1])));
        assert!(tree_of(&now, key(Pid::from_u32(21), 8)).is_none());
    }

    #[test]
    fn expand_all_leaves_childless_roots_closed() {
        let mut trees = Trees::default();
        let mut now = agents(&[10, 20]);
        now[1].procs.truncate(1);
        trees.expand_all(&now);
        assert!(trees.is_open(root(&now[0])));
        assert!(!trees.is_open(root(&now[1])));
    }
}
