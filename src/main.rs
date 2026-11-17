mod agents;
mod cli;
mod focus;
mod ports;
mod theme;
mod trees;
mod ui;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use agents::{Agent, BusyTracker, EnvOrphans, Orphan, OrphanTracker};
use cli::Action;
use focus::Focus;
use ports::Listener;
use theme::Theme;
use trees::Trees;
use ui::{Fold, View};

/// Ports change rarely and resolving owners reads every fd in /proc.
const PORT_TICKS: u32 = 5;
/// sysinfo reports cpu as 0 until a second read, which needs this gap.
const CPU_SETTLE: Duration = Duration::from_millis(200);

struct App {
    sys: System,
    home: Option<PathBuf>,
    ticks: u32,
    agents: Vec<Agent>,
    busy: BusyTracker,
    tracker: OrphanTracker,
    env: EnvOrphans,
    orphans: Vec<Orphan>,
    ports: Vec<Listener>,
    tick: Duration,
}

impl App {
    fn new(tick: Duration) -> io::Result<Self> {
        Ok(Self {
            sys: System::new().map_err(io::Error::other)?,
            home: agents::canonical_home(std::env::var_os("HOME")),
            ticks: 0,
            agents: Vec::new(),
            busy: BusyTracker::new(agents::busy_window(tick)),
            tracker: OrphanTracker::default(),
            env: EnvOrphans::new(Pid::from_u32(std::process::id())),
            orphans: Vec::new(),
            ports: Vec::new(),
            tick,
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
        self.busy.update(&mut self.agents);
        let home = self.home.as_deref();
        self.orphans = agents::merge(
            self.tracker.update(&procs, &self.agents),
            self.env
                .update(&procs, &self.agents, home, agents::read_environ),
        );
        if self.ticks.is_multiple_of(PORT_TICKS) {
            self.ports = ports::listening(&self.sys);
        }
        self.ticks = self.ticks.wrapping_add(1);
    }
}

fn main() -> io::Result<ExitCode> {
    let opts = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Action::Run(opts)) => opts,
        Ok(Action::Help) => {
            println!("{}", cli::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        Ok(Action::Version) => {
            println!("hum {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Err(msg) => {
            eprintln!("hum: {msg}");
            return Ok(ExitCode::from(2));
        }
    };
    // leaked once; agent kinds are &'static str
    agents::set_extra(opts.agents.into_iter().map(|a| &*a.leak()).collect());
    let mut app = App::new(opts.interval)?;
    let mut view = View {
        help: false,
        help_scroll: 0,
        cores: false,
        all_ports: false,
        theme: Theme::from_env(),
        focus: Focus::default(),
        trees: Trees::default(),
    };
    let terminal = ratatui::init();
    let result = run(terminal, &mut app, &mut view);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

fn run(mut terminal: DefaultTerminal, app: &mut App, view: &mut View) -> io::Result<()> {
    // stamped before the refresh so its run time does not stretch the period
    let mut last_tick = Instant::now();
    app.refresh();
    // the second refresh comes early so cpu shows before a long tick ends
    let mut period = app.tick.min(CPU_SETTLE);

    loop {
        if last_tick.elapsed() >= period {
            last_tick = Instant::now();
            app.refresh();
            period = app.tick;
        }

        terminal.draw(|frame| ui::draw(frame, app, view))?;

        let timeout = period.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let focus = &mut view.focus;
            let agents = &app.agents;
            match key.code {
                KeyCode::Esc if view.help => view.help = false,
                KeyCode::Esc if focus.clear() => {}
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('?') => {
                    view.help = !view.help;
                    view.help_scroll = 0;
                }
                KeyCode::Char('j') | KeyCode::Down if view.help => view.help_scroll += 1,
                KeyCode::Char('k') | KeyCode::Up if view.help => {
                    view.help_scroll = view.help_scroll.saturating_sub(1)
                }
                // the cursor stays put behind the help
                _ if view.help => {}
                KeyCode::Char('j') | KeyCode::Down => focus.step(1),
                KeyCode::Char('k') | KeyCode::Up => focus.step(-1),
                KeyCode::Char('g') => focus.first(),
                KeyCode::Char('G') => focus.last(),
                KeyCode::PageDown => focus.page(true),
                KeyCode::PageUp => focus.page(false),
                KeyCode::Tab => focus.cycle(1),
                KeyCode::BackTab => focus.cycle(-1),
                KeyCode::Enter | KeyCode::Char(' ') => view.fold(agents, Fold::Toggle),
                KeyCode::Char('l') | KeyCode::Right => view.fold(agents, Fold::Expand),
                KeyCode::Char('h') | KeyCode::Left => view.fold(agents, Fold::Collapse),
                KeyCode::Char('e') => view.trees.expand_all(agents),
                KeyCode::Char('c') => view.trees.collapse_all(),
                KeyCode::Char('1') => view.cores = !view.cores,
                KeyCode::Char('a') => view.all_ports = !view.all_ports,
                _ => {}
            }
        }
    }
}
