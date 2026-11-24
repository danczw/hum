//! What the user moves and toggles while hum runs: the cursor of each
//! section, open agent trees, the help and its scroll, and the display
//! toggles. Keys come in as [`Intent`]s; each frame [`Nav::frame`] fits the
//! cursors to the room the layout gives each section.

pub mod cursor;
pub mod outline;

use sysinfo::Pid;

use crate::agents::{Agent, Orphan, ProcKey};
use crate::ports::Listener;
use crate::ui::Snapshot;
use cursor::Focus;
pub use cursor::{AGENTS, ORPHANS, PORTS, SECTIONS, Window};
use outline::{Entry, Fold, Outline};

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

/// What a key asks for, apart from which key it is.
#[derive(Clone, Copy, Debug)]
pub enum Intent {
    /// Closes the help, else hides the cursor, else quits.
    Back,
    Quit,
    Help,
    Up,
    Down,
    First,
    Last,
    PageUp,
    PageDown,
    Next,
    Prev,
    Toggle,
    Expand,
    Collapse,
    ExpandAll,
    CollapseAll,
    Cores,
    AllPorts,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Continue,
    Quit,
}

/// The room one frame gives each part, from [`crate::ui::Plan::lines`].
#[derive(Debug, Default)]
pub struct Lines {
    /// Data lines per section, indexed by [`AGENTS`], [`ORPHANS`] and
    /// [`PORTS`]. None when its rows fold into a border or the pane is too
    /// small, so `Tab` passes over it; a section squeezed to `Some(0)` keeps
    /// its rows and its place in `Tab`.
    pub sections: [Option<usize>; SECTIONS],
    /// Help lines that do not fit, the furthest the help scrolls.
    pub help_overflow: usize,
}

/// Each section's rows in one frame, built once for layout, cursors and
/// paint.
pub struct Lists<'a> {
    /// Open trees expanded.
    pub agents: Vec<Entry<'a>>,
    pub orphans: &'a [Orphan],
    /// Those of agents and orphans, or every port once `a` toggles them all.
    pub ports: Vec<&'a Listener>,
}

#[derive(Default)]
pub struct Nav {
    focus: Focus,
    outline: Outline,
    help: Option<usize>,
    /// [`Lines::help_overflow`] of the last frame.
    help_overflow: usize,
    cores: bool,
    all_ports: bool,
}

impl Nav {
    pub fn handle(&mut self, intent: Intent, snapshot: &Snapshot) -> Outcome {
        use Intent::*;
        // the cursor stays put behind the help
        if let Some(scroll) = &mut self.help {
            match intent {
                Quit => return Outcome::Quit,
                Back | Help => self.help = None,
                Down => *scroll = (*scroll + 1).min(self.help_overflow),
                Up => *scroll = scroll.saturating_sub(1),
                _ => {}
            }
            return Outcome::Continue;
        }
        let focus = &mut self.focus;
        let agents = &snapshot.agents;
        match intent {
            Back if focus.clear() => {}
            Quit | Back => return Outcome::Quit,
            Help => self.help = Some(0),
            Down => focus.step(1),
            Up => focus.step(-1),
            First => focus.jump(0),
            Last => focus.jump(usize::MAX),
            PageDown => focus.page(true),
            PageUp => focus.page(false),
            Next => focus.cycle(1),
            Prev => focus.cycle(-1),
            Toggle => self.fold(agents, Fold::Toggle),
            Expand => self.fold(agents, Fold::Expand),
            Collapse => self.fold(agents, Fold::Collapse),
            ExpandAll => self.outline.expand_all(agents),
            CollapseAll => {
                self.outline.collapse_all();
                self.observe(snapshot);
            }
            Cores => self.cores = !self.cores,
            AllPorts => self.all_ports = !self.all_ports,
        }
        Outcome::Continue
    }

    /// Takes in a new snapshot: forgets exited sessions and moves a cursor
    /// on a child whose tree closed to its root.
    pub fn observe(&mut self, snapshot: &Snapshot) {
        let at = self.focus.selected(AGENTS);
        if let Some(to) = self.outline.sync(&snapshot.agents, at) {
            self.focus.select(AGENTS, to);
        }
    }

    pub fn lists<'a>(&self, snapshot: &'a Snapshot) -> Lists<'a> {
        Lists {
            agents: self.outline.rows(&snapshot.agents),
            orphans: &snapshot.orphans,
            ports: snapshot
                .ports
                .iter()
                .filter(|p| self.all_ports || p.owner.is_some())
                .collect(),
        }
    }

    /// Finds each cursor's row in `lists`, then moves focus off a section
    /// left without rows. Runs before layout, so the section focus lands on
    /// gets a line for its cursor.
    pub fn sync(&mut self, lists: &Lists) {
        let mut keys: [Vec<Key>; SECTIONS] = Default::default();
        keys[AGENTS] = lists.agents.iter().map(|e| e.key.clone()).collect();
        keys[ORPHANS] = lists.orphans.iter().map(|o| Key::Proc(o.key())).collect();
        keys[PORTS] = lists
            .ports
            .iter()
            .map(|p| Key::Port {
                port: p.port,
                pid: p.pid(),
            })
            .collect();
        for (section, keys) in keys.into_iter().enumerate() {
            self.focus.pane(section).sync(keys);
        }
        self.focus.settle();
    }

    /// Scrolls each cursor into the lines `lines` gives its section, then
    /// moves focus off a section that folded away. A hidden section gets an
    /// empty window.
    pub fn frame(&mut self, lines: &Lines) -> [Window; SECTIONS] {
        let mut windows: [Window; SECTIONS] = Default::default();
        for (section, window) in windows.iter_mut().enumerate() {
            let pane = self.focus.pane(section);
            match lines.sections[section] {
                Some(height) => *window = pane.frame(height),
                None => pane.hide(),
            }
        }
        self.focus.settle();
        self.help_overflow = lines.help_overflow;
        if let Some(scroll) = &mut self.help {
            *scroll = (*scroll).min(self.help_overflow);
        }
        windows
    }

    pub fn shows(&self, section: usize) -> bool {
        self.focus.shows(section)
    }

    /// Lines the help is scrolled down; None when it is closed.
    pub fn help(&self) -> Option<usize> {
        self.help
    }

    /// Per-core bars under the gauge line.
    pub fn cores(&self) -> bool {
        self.cores
    }

    /// Expands or collapses the tree under the agents cursor.
    fn fold(&mut self, agents: &[Agent], fold: Fold) {
        if !self.focus.shows(AGENTS) {
            return;
        }
        let Some(at) = self.focus.selected(AGENTS) else {
            return;
        };
        if let Some(to) = self.outline.fold(agents, at, fold) {
            self.focus.select(AGENTS, to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Owner, Proc};
    use crate::label::Label;
    use crate::ports::Holder;
    use crate::ui::Gauges;
    use Intent::*;

    fn proc(pid: u32, depth: usize) -> Proc {
        Proc {
            pid: Pid::from_u32(pid),
            name: String::new(),
            cpu: 0.0,
            mem: 0,
            depth,
            start: 7,
        }
    }

    /// A session at `root` with `kids` children at `root + 1` on.
    fn agent(root: u32, kids: u32) -> Agent {
        let mut procs = vec![proc(root, 0)];
        procs.extend((1..=kids).map(|i| proc(root + i, 1)));
        Agent {
            kind: "claude",
            project: String::new(),
            procs,
            cpu: 0.0,
            cpu_busy: false,
            reported: None,
            session: None,
            session_id: None,
            subagents: Vec::new(),
        }
    }

    fn label() -> Label {
        Label {
            kind: "claude",
            project: Some("hum".into()),
            session: None,
        }
    }

    /// `agents`, an orphan at each of `orphans` and an agent's port at each
    /// of `ports`.
    fn snapshot(agents: Vec<Agent>, orphans: &[u32], ports: &[u16]) -> Snapshot {
        Snapshot {
            gauges: Gauges {
                cpu: 0.0,
                cores: Vec::new(),
                mem: (0, 0),
                swap: (0, 0),
            },
            agents,
            orphans: orphans
                .iter()
                .map(|&pid| Orphan {
                    pid: Pid::from_u32(pid),
                    name: "node".into(),
                    from: label(),
                    start: 7,
                    cpu: 0.0,
                    mem: 0,
                })
                .collect(),
            ports: ports
                .iter()
                .map(|&port| Listener {
                    port,
                    public: false,
                    holder: Some(Holder {
                        pid: Pid::from_u32(9),
                        start: 7,
                        name: "node".into(),
                    }),
                    owner: Some(Owner {
                        label: label(),
                        orphan: false,
                    }),
                })
                .collect(),
        }
    }

    /// Room for 5 rows in each section, ports folded when `ports` is false.
    fn lines(ports: bool) -> Lines {
        Lines {
            sections: [Some(5), Some(5), ports.then_some(5)],
            help_overflow: 0,
        }
    }

    fn pid(pid: u32) -> Key {
        Key::Proc(ProcKey {
            pid: Pid::from_u32(pid),
            start: 7,
        })
    }

    fn port(port: u16) -> Key {
        Key::Port {
            port,
            pid: Some(Pid::from_u32(9)),
        }
    }

    /// One frame of `snapshot` as `render` runs it, with `lines` for layout.
    fn frame(nav: &mut Nav, snapshot: &Snapshot, lines: &Lines) -> [Window; SECTIONS] {
        nav.sync(&nav.lists(snapshot));
        nav.frame(lines)
    }

    fn press(nav: &mut Nav, snapshot: &Snapshot, intents: &[Intent]) {
        for &intent in intents {
            assert_eq!(nav.handle(intent, snapshot), Outcome::Continue);
        }
    }

    #[test]
    fn esc_closes_the_help_then_hides_the_cursor_then_quits() {
        let now = snapshot(vec![agent(10, 0)], &[], &[]);
        let mut nav = Nav::default();
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Down, Help, Back]);
        assert_eq!(nav.help(), None);
        assert!(nav.shows(AGENTS));
        press(&mut nav, &now, &[Back]);
        assert!(!nav.shows(AGENTS));
        assert_eq!(nav.handle(Back, &now), Outcome::Quit);
    }

    #[test]
    fn help_scrolls_to_its_last_line_over_a_still_cursor() {
        let now = snapshot(vec![agent(10, 0), agent(20, 0)], &[], &[]);
        let mut nav = Nav::default();
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Down, Help]);
        let help = Lines {
            help_overflow: 2,
            ..lines(true)
        };
        frame(&mut nav, &now, &help);
        press(&mut nav, &now, &[Down, Down, Down]);
        assert_eq!(nav.help(), Some(2));
        press(&mut nav, &now, &[Up]);
        assert_eq!(nav.help(), Some(1));
        // a taller pane fits it all
        frame(&mut nav, &now, &lines(true));
        assert_eq!(nav.help(), Some(0));
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(10)));
        assert_eq!(nav.handle(Quit, &now), Outcome::Quit);
    }

    #[test]
    fn folding_a_child_moves_the_cursor_to_its_root() {
        let now = snapshot(vec![agent(10, 2), agent(20, 1), agent(30, 0)], &[], &[]);
        let mut nav = Nav::default();
        press(&mut nav, &now, &[ExpandAll]);
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Down, Down, Down]);
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(12)));
        press(&mut nav, &now, &[Toggle]);
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(10)));
        assert_eq!(frame(&mut nav, &now, &lines(true))[AGENTS].selected, 0);
        // so does collapsing every tree, before the next frame
        press(&mut nav, &now, &[ExpandAll]);
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Last]);
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Up]);
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(21)));
        press(&mut nav, &now, &[CollapseAll]);
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(20)));
    }

    #[test]
    fn cursor_follows_its_process_when_a_session_above_exits() {
        let mut nav = Nav::default();
        let now = snapshot(vec![agent(10, 0), agent(20, 0)], &[], &[]);
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Down, Down]);
        let later = snapshot(vec![agent(20, 0)], &[], &[]);
        nav.observe(&later);
        assert_eq!(frame(&mut nav, &later, &lines(true))[AGENTS].selected, 0);
        assert_eq!(nav.focus.selected(AGENTS), Some(&pid(20)));
    }

    #[test]
    fn folded_ports_take_no_cursor_and_keep_theirs() {
        let now = snapshot(vec![agent(10, 0)], &[9120], &[3000, 8000]);
        let mut nav = Nav::default();
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Prev, Prev, Down]);
        assert!(nav.shows(PORTS));
        assert_eq!(frame(&mut nav, &now, &lines(true))[PORTS].selected, 1);
        // folded, focus moves on to agents and Tab passes over ports
        frame(&mut nav, &now, &lines(false));
        assert!(nav.shows(AGENTS));
        press(&mut nav, &now, &[Next, Next]);
        assert!(nav.shows(AGENTS));
        frame(&mut nav, &now, &lines(true));
        press(&mut nav, &now, &[Prev]);
        assert!(nav.shows(PORTS));
        assert_eq!(nav.focus.selected(PORTS), Some(&port(8000)));
    }

    #[test]
    fn a_counts_every_port_as_a_row() {
        let mut now = snapshot(Vec::new(), &[], &[3000]);
        now.ports[0].owner = None;
        let mut nav = Nav::default();
        assert!(nav.lists(&now).ports.is_empty());
        press(&mut nav, &now, &[AllPorts]);
        assert_eq!(nav.lists(&now).ports.len(), 1);
    }
}
