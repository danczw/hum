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

    pub fn expand_all(&mut self, agents: &[Agent]) {
        self.open.extend(agents.iter().map(root));
    }

    pub fn collapse_all(&mut self) {
        self.open.clear();
    }

    /// Forgets the sessions that exited.
    pub fn retain(&mut self, agents: &[Agent]) {
        let roots: HashSet<Key> = agents.iter().map(root).collect();
        self.open.retain(|k| roots.contains(k));
    }
}

pub fn root(agent: &Agent) -> Key {
    let p = &agent.procs[0];
    key(p.pid, p.start)
}

/// The root of the tree that holds the process `of`.
pub fn root_of(agents: &[Agent], of: Key) -> Option<Key> {
    agents
        .iter()
        .find(|a| a.procs.iter().any(|p| key(p.pid, p.start) == of))
        .map(root)
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
    fn root_of_finds_the_tree_of_a_child() {
        let now = agents(&[10, 20]);
        let child = key(Pid::from_u32(21), 7);
        assert_eq!(root_of(&now, child), Some(root(&now[1])));
        assert_eq!(root_of(&now, key(Pid::from_u32(21), 8)), None);
    }
}
