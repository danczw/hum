mod agents;
mod claude;
mod cli;
mod focus;
mod host;
mod label;
mod meminfo;
mod outline;
mod ports;
mod theme;
mod ui;

use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use agents::Sessions;
use cli::Action;
use host::{Host, ProcHost};
use outline::Fold;
use ports::Ports;
use theme::Theme;
use ui::{Gauges, Snapshot, View};

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
    let mut view = View::new(Theme::from_env());
    let terminal = ratatui::init();
    let result = run(terminal, &mut app, &mut view);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

fn run(mut terminal: DefaultTerminal, app: &mut App, view: &mut View) -> io::Result<()> {
    // stamped before the refresh so its run time does not stretch the period
    let mut last_tick = Instant::now();
    let mut snapshot = app.refresh();
    // the second refresh comes early so cpu shows before a long tick ends
    let mut period = app.tick.min(CPU_SETTLE);

    loop {
        if last_tick.elapsed() >= period {
            last_tick = Instant::now();
            snapshot = app.refresh();
            period = app.tick;
            // a refresh slower than the tick restarts it, so hum still idles a full tick
            if last_tick.elapsed() >= period {
                last_tick = Instant::now();
            }
        }

        terminal.draw(|frame| ui::draw(frame, &snapshot, view))?;

        let timeout = period.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // the cursor stays put behind the help
            if let Some(scroll) = &mut view.help {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc | KeyCode::Char('?') => view.help = None,
                    KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
                    KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    _ => {}
                }
                continue;
            }
            let focus = &mut view.focus;
            let agents = &snapshot.agents;
            match key.code {
                KeyCode::Esc if focus.clear() => {}
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('?') => view.help = Some(0),
                KeyCode::Char('j') | KeyCode::Down => focus.step(1),
                KeyCode::Char('k') | KeyCode::Up => focus.step(-1),
                KeyCode::Char('g') => focus.jump(0),
                KeyCode::Char('G') => focus.jump(usize::MAX),
                KeyCode::PageDown => focus.page(true),
                KeyCode::PageUp => focus.page(false),
                KeyCode::Tab => focus.cycle(1),
                KeyCode::BackTab => focus.cycle(-1),
                KeyCode::Enter | KeyCode::Char(' ') => view.fold(agents, Fold::Toggle),
                KeyCode::Char('l') | KeyCode::Right => view.fold(agents, Fold::Expand),
                KeyCode::Char('h') | KeyCode::Left => view.fold(agents, Fold::Collapse),
                KeyCode::Char('e') => view.outline.expand_all(agents),
                KeyCode::Char('c') => view.outline.collapse_all(),
                KeyCode::Char('1') => view.cores = !view.cores,
                KeyCode::Char('a') => view.all_ports = !view.all_ports,
                _ => {}
            }
        }
    }
}
