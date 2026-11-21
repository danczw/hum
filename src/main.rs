mod agents;
mod claude;
mod cli;
mod focus;
mod label;
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
use focus::Focus;
use outline::{Fold, Outline};
use ports::Listener;
use theme::Theme;
use ui::{Gauges, Port, Snapshot, View};

/// Ports change rarely and resolving owners reads every fd in /proc.
const PORT_TICKS: u32 = 5;
/// sysinfo reports cpu as 0 until a second read, which needs this gap.
const CPU_SETTLE: Duration = Duration::from_millis(200);

struct App {
    sys: System,
    ticks: u32,
    sessions: Sessions,
    ports: Vec<Listener>,
    tick: Duration,
}

impl App {
    fn new(tick: Duration) -> io::Result<Self> {
        let home = agents::canonical_home(std::env::var_os("HOME"));
        let claude = claude::config(std::env::var_os("CLAUDE_CONFIG_DIR"), home.as_deref());
        Ok(Self {
            sys: System::new().map_err(io::Error::other)?,
            ticks: 0,
            sessions: Sessions::new(home, claude, tick),
            ports: Vec::new(),
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
        let (agents, orphans) = self.sessions.observe(&procs, SystemTime::now());
        if self.ticks.is_multiple_of(PORT_TICKS) {
            self.ports = ports::listening(&procs);
        } else {
            ports::join(&mut self.ports, &procs);
        }
        self.ticks = self.ticks.wrapping_add(1);
        let ports = self
            .ports
            .iter()
            .map(|l| Port {
                port: l.port,
                public: l.public,
                holder: l.holder.clone(),
                owner: l
                    .holder
                    .as_ref()
                    .and_then(|h| agents::owner(&agents, &orphans, h.pid, h.start)),
            })
            .collect();
        Snapshot {
            gauges: Gauges::read(&self.sys),
            agents,
            orphans,
            ports,
        }
    }
}

impl Gauges {
    /// The only read of `System` for drawing. Reads every core each tick so
    /// the grid shows as soon as it is toggled.
    fn read(sys: &System) -> Self {
        Self {
            cpu: f64::from(sys.global_cpu_usage()) / 100.0,
            cores: sys
                .cpus()
                .iter()
                .map(|c| f64::from(c.usage()) / 100.0)
                .collect(),
            mem: (sys.used_memory(), sys.total_memory()),
            swap: (sys.used_swap(), sys.total_swap()),
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
    agents::set_extra(opts.agents.into_iter().map(|a| &*a.leak()).collect());
    let mut app = App::new(opts.interval)?;
    let mut view = View {
        help: false,
        help_scroll: 0,
        cores: false,
        all_ports: false,
        theme: Theme::from_env(),
        focus: Focus::default(),
        outline: Outline::default(),
    };
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
            let focus = &mut view.focus;
            let agents = &snapshot.agents;
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
                KeyCode::Char('e') => view.outline.expand_all(agents),
                KeyCode::Char('c') => view.outline.collapse_all(),
                KeyCode::Char('1') => view.cores = !view.cores,
                KeyCode::Char('a') => view.all_ports = !view.all_ports,
                _ => {}
            }
        }
    }
}
