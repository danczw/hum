mod agents;
mod ports;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use agents::{Agent, Orphan, OrphanTracker};
use ports::Listener;

const TICK: Duration = Duration::from_secs(1);
/// Ports change rarely and resolving owners reads every fd in /proc.
const PORT_TICKS: u32 = 5;

struct App {
    sys: System,
    home: Option<PathBuf>,
    ticks: u32,
    agents: Vec<Agent>,
    tracker: OrphanTracker,
    orphans: Vec<Orphan>,
    ports: Vec<Listener>,
}

impl App {
    fn new() -> io::Result<Self> {
        Ok(Self {
            sys: System::new().map_err(io::Error::other)?,
            home: std::env::var_os("HOME").map(PathBuf::from),
            ticks: 0,
            agents: Vec::new(),
            tracker: OrphanTracker::default(),
            orphans: Vec::new(),
            ports: Vec::new(),
        })
    }

    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                // sysinfo keeps a pid's name from before an exec, so re-read
                // cmd every tick to see what the process runs now.
                .with_cmd(UpdateKind::Always)
                // cwd only names the session, so read it once per pid to skip a
                // readlink per process per tick. A chdir or deletion after the
                // first read goes unseen.
                .with_cwd(UpdateKind::OnlyIfNotSet)
                .without_tasks(),
        );
        let procs = agents::snapshot(&self.sys);
        self.agents = agents::scan(&procs, self.home.as_deref());
        self.orphans = self.tracker.update(&procs, &self.agents);
        if self.ticks.is_multiple_of(PORT_TICKS) {
            self.ports = ports::listening(&self.sys);
        }
        self.ticks = self.ticks.wrapping_add(1);
    }
}

fn main() -> io::Result<()> {
    let mut app = App::new()?;
    let terminal = ratatui::init();
    let result = run(terminal, &mut app);
    ratatui::restore();
    result
}

fn run(mut terminal: DefaultTerminal, app: &mut App) -> io::Result<()> {
    app.refresh();
    let mut last_tick = Instant::now();

    loop {
        if last_tick.elapsed() >= TICK {
            app.refresh();
            last_tick = Instant::now();
        }

        terminal.draw(|frame| ui::draw(frame, app))?;

        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(());
        }
    }
}
