mod agents;
mod claude;
mod cli;
mod host;
mod label;
mod meminfo;
mod nav;
mod ports;
mod theme;
mod ui;

use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{DefaultTerminal, Frame};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use agents::Sessions;
use cli::Action;
use host::{Host, ProcHost};
use nav::{Intent, Nav, Outcome};
use ports::Ports;
use theme::Theme;
use ui::{Gauges, Snapshot};

/// sysinfo reports cpu as 0 until a second read, which needs this gap.
const CPU_SETTLE: Duration = Duration::from_millis(200);

struct App {
    sys: System,
    host: ProcHost,
    sessions: Sessions,
    ports: Ports,
    tick: Duration,
}

impl App {
    fn new(tick: Duration, agents: Vec<&'static str>) -> io::Result<Self> {
        let home = agents::canonical_home(std::env::var_os("HOME"));
        let claude = claude::config(std::env::var_os("CLAUDE_CONFIG_DIR"), home.as_deref());
        Ok(Self {
            sys: System::new().map_err(io::Error::other)?,
            host: ProcHost,
            sessions: Sessions::new(home, claude, agents, tick, ProcHost),
            ports: Ports::new(ProcHost, tick),
            tick,
        })
    }

    fn refresh(&mut self) -> Snapshot {
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
        let at = Instant::now();
        let (agents, orphans) = self.sessions.observe(&procs, at, SystemTime::now());
        let ports = self.ports.observe(&procs, at, &agents, &orphans);
        Snapshot {
            gauges: Gauges::read(&self.sys, &self.host),
            agents,
            orphans,
            ports,
        }
    }
}

impl Gauges {
    /// The only read of `System` for drawing. Reads every core each tick so
    /// the grid shows as soon as it is toggled.
    fn read(sys: &System, host: &impl Host) -> Self {
        Self {
            cpu: f64::from(sys.global_cpu_usage()) / 100.0,
            cores: sys
                .cpus()
                .iter()
                .map(|c| f64::from(c.usage()) / 100.0)
                .collect(),
            mem: (sys.used_memory(), sys.total_memory()),
            swap: host
                .meminfo()
                .and_then(|m| meminfo::swap(&m))
                .unwrap_or_else(|| (sys.used_swap(), sys.total_swap())),
        }
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
    let agents = opts.agents.into_iter().map(|a| &*a.leak()).collect();
    let mut app = App::new(opts.interval, agents)?;
    let theme = Theme::from_env();
    let terminal = ratatui::init();
    let result = run(terminal, &mut app, &theme);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

fn run(mut terminal: DefaultTerminal, app: &mut App, theme: &Theme) -> io::Result<()> {
    let mut nav = Nav::default();
    // stamped before the refresh so its run time does not stretch the period
    let mut last_tick = Instant::now();
    let mut snapshot = app.refresh();
    nav.observe(&snapshot);
    // the second refresh comes early so cpu shows before a long tick ends
    let mut period = app.tick.min(CPU_SETTLE);

    loop {
        if last_tick.elapsed() >= period {
            last_tick = Instant::now();
            snapshot = app.refresh();
            nav.observe(&snapshot);
            period = app.tick;
            // a refresh slower than the tick restarts it, so hum still idles a full tick
            if last_tick.elapsed() >= period {
                last_tick = Instant::now();
            }
        }

        terminal.draw(|frame| render(frame, &snapshot, &mut nav, theme))?;

        let timeout = period.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(intent) = intent(key.code)
            && nav.handle(intent, &snapshot) == Outcome::Quit
        {
            return Ok(());
        }
    }
}

/// Finds the cursors' rows, lays out the frame, fits the cursors to it,
/// then paints.
fn render(frame: &mut Frame, snapshot: &Snapshot, nav: &mut Nav, theme: &Theme) {
    let lists = nav.lists(snapshot);
    nav.sync(&lists);
    let plan = ui::plan(frame.area(), snapshot, &lists, nav);
    let windows = nav.frame(&plan.lines());
    ui::draw(frame, snapshot, &lists, nav, theme, plan, &windows);
}

fn intent(code: KeyCode) -> Option<Intent> {
    Some(match code {
        KeyCode::Esc => Intent::Back,
        KeyCode::Char('q') => Intent::Quit,
        KeyCode::Char('?') => Intent::Help,
        KeyCode::Char('j') | KeyCode::Down => Intent::Down,
        KeyCode::Char('k') | KeyCode::Up => Intent::Up,
        KeyCode::Char('g') => Intent::First,
        KeyCode::Char('G') => Intent::Last,
        KeyCode::PageDown => Intent::PageDown,
        KeyCode::PageUp => Intent::PageUp,
        KeyCode::Tab => Intent::Next,
        KeyCode::BackTab => Intent::Prev,
        KeyCode::Enter | KeyCode::Char(' ') => Intent::Toggle,
        KeyCode::Char('l') | KeyCode::Right => Intent::Expand,
        KeyCode::Char('h') | KeyCode::Left => Intent::Collapse,
        KeyCode::Char('e') => Intent::ExpandAll,
        KeyCode::Char('c') => Intent::CollapseAll,
        KeyCode::Char('1') => Intent::Cores,
        KeyCode::Char('a') => Intent::AllPorts,
        _ => return None,
    })
}
