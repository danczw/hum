mod agents;
mod ports;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use agents::{Agent, Orphan, OrphanTracker};
use ports::Listener;

const TICK: Duration = Duration::from_secs(1);

struct App {
    sys: System,
    agents: Vec<Agent>,
    tracker: OrphanTracker,
    orphans: Vec<Orphan>,
    ports: Vec<Listener>,
}

impl App {
    fn new() -> io::Result<Self> {
        Ok(Self {
            sys: System::new().map_err(io::Error::other)?,
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
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        self.agents = agents::scan(&self.sys);
        self.orphans = self.tracker.update(&self.sys, &self.agents);
        self.ports = ports::listening();
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
    let mut last_tick = Instant::now() - TICK;

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
