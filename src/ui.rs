use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table};
use sysinfo::Pid;

use crate::App;
use crate::agents::{self, Agent, Orphan};

// Numbers get fixed widths and text columns fill what is left, so a narrow
// pane truncates names, never numbers. Fill ranks below Length in ratatui's
// layout, Min ranks above it.
/// pid_max is at most 4194304 on 64-bit Linux: 7 digits.
const PID: Constraint = Constraint::Length(7);
/// `fmt_cores` up to 99.99 cores.
const CORES: Constraint = Constraint::Length(5);
/// `fmt_bytes` up to `1023.9M`.
const MEM: Constraint = Constraint::Length(7);
/// Narrower panes stack the sections; wider ones put orphans and ports in a
/// right-hand column.
const WIDE: u16 = 100;

/// From this height up a blank row separates the gauge line from the
/// sections. In shorter panes that row goes to data.
const AIRY: u16 = 12;

const DIM: Style = Style::new().fg(Color::DarkGray);

/// How sections are framed. Both looks stay selectable until one is picked
/// on real data, then the other goes (#4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chrome {
    /// A one-line dim title with count and a rule: `agents 3 ────`.
    Borderless,
    /// A thin rounded dim box with the title in its top border.
    Rounded,
}

impl Chrome {
    /// `HUM_STYLE=borderless|rounded`; rounded when unset or unknown.
    pub fn from_env() -> Self {
        match std::env::var("HUM_STYLE").as_deref() {
            Ok("borderless") => Self::Borderless,
            _ => Self::Rounded,
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Borderless => Self::Rounded,
            Self::Rounded => Self::Borderless,
        }
    }

    /// Rows a section's title and frame take, without data rows.
    fn overhead(self) -> u16 {
        match self {
            Self::Borderless => 1,
            Self::Rounded => 2,
        }
    }
}

/// What the user toggles while hum runs.
pub struct View {
    pub chrome: Chrome,
    pub help: bool,
}

/// Everything one frame shows, read out of [`App`] so drawing needs no
/// `System`.
struct Screen<'a> {
    /// 0.0 to 1.0.
    cpu: f64,
    /// (used, total) bytes.
    mem: (u64, u64),
    swap: (u64, u64),
    agents: &'a [Agent],
    orphans: &'a [Orphan],
    ports: Vec<PortRow>,
}

struct PortRow {
    port: u16,
    pid: Option<Pid>,
    name: String,
    /// The session or orphan holding the port.
    owner: Option<String>,
}

/// A titled list of rows under a dim header naming the columns.
struct Section {
    name: &'static str,
    count: usize,
    header: Row<'static>,
    rows: Vec<Row<'static>>,
    widths: Vec<Constraint>,
}

pub fn draw(frame: &mut Frame, app: &App) {
    render(frame, &screen(app), &app.view);
}

fn screen(app: &App) -> Screen<'_> {
    let sys = &app.sys;
    let ports = app
        .ports
        .iter()
        .map(|l| PortRow {
            port: l.port,
            pid: l.pid,
            name: l
                .pid
                .and_then(|pid| sys.process(pid))
                .map(agents::proc_name)
                .unwrap_or_default(),
            owner: l.pid.and_then(|p| owner(app, p)),
        })
        .collect();
    Screen {
        cpu: f64::from(sys.global_cpu_usage()) / 100.0,
        mem: (sys.used_memory(), sys.total_memory()),
        swap: (sys.used_swap(), sys.total_swap()),
        agents: &app.agents,
        orphans: &app.orphans,
        ports,
    }
}

fn render(frame: &mut Frame, screen: &Screen, view: &View) {
    let pad = u16::from(frame.area().height >= AIRY);
    let [gauges, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(pad),
        Constraint::Fill(1),
    ])
    .areas(frame.area());
    draw_gauges(frame, screen, gauges);

    let agents = agents_section(screen.agents);
    let orphans = orphans_section(screen.orphans);
    let ports = ports_section(&screen.ports);
    if body.width >= WIDE {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);
        draw_section(frame, agents, left, view.chrome);
        stack(frame, [orphans, ports], right, view.chrome);
    } else {
        stack(frame, [agents, orphans, ports], body, view.chrome);
    }

    if view.help {
        draw_help(frame);
    }
}

/// `cpu ⣿⣿⡆⠀ 34%   mem ⣿⣿⣿⠀ 12.1/31G   swp ⡀⠀⠀⠀ 0.0/8G` on one line. The
/// bars share one length so they compare at a glance, and stretch to fill
/// the line. The unused part stays blank, no track.
fn draw_gauges(frame: &mut Frame, screen: &Screen, area: Rect) {
    let gauges = [
        (
            "cpu",
            screen.cpu,
            format!("{:>3.0}%", screen.cpu * 100.0),
            Color::Cyan,
        ),
        (
            "mem",
            ratio(screen.mem),
            usage_label(screen.mem),
            Color::Magenta,
        ),
        (
            "swp",
            ratio(screen.swap),
            usage_label(screen.swap),
            Color::Yellow,
        ),
    ];
    const GAP: u16 = 3;
    // name, space, bar, space, label
    let text = |(name, _, label, _): &(&str, f64, String, Color)| name.len() + label.len() + 2;
    let fixed: usize = gauges.iter().map(text).sum::<usize>() + usize::from(GAP) * 2;
    let bar = usize::from(area.width).saturating_sub(fixed) / gauges.len();
    let widths = gauges
        .each_ref()
        .map(|g| Constraint::Length((text(g) + bar) as u16));
    let areas: [Rect; 3] = Layout::horizontal(widths).spacing(GAP).areas(area);
    for ((name, ratio, label, color), area) in gauges.into_iter().zip(areas) {
        let line = Line::from(vec![
            Span::styled(name, DIM),
            Span::raw(" "),
            Span::styled(braille_bar(ratio, bar), Style::new().fg(color)),
            Span::raw(" "),
            Span::raw(label),
        ]);
        frame.render_widget(line, area);
    }
}

fn agents_section(agents: &[Agent]) -> Section {
    let rows = agents
        .iter()
        .flat_map(|agent| {
            let root = &agent.procs[0];
            let head = Row::new([
                Cell::from(agent.kind),
                Cell::from(agent.project.clone()),
                num(root.pid.to_string()),
                num(fmt_cores(agent.cpu())),
                num(fmt_bytes(agent.mem())),
            ])
            .style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            let kids = agent.procs[1..].iter().map(|p| {
                Row::new([
                    Cell::from(format!("{}└ {}", "  ".repeat(p.depth - 1), p.name)),
                    Cell::default(),
                    num(p.pid.to_string()),
                    num(fmt_cores(p.cpu)),
                    num(fmt_bytes(p.mem)),
                ])
            });
            std::iter::once(head).chain(kids)
        })
        .collect();
    Section {
        name: "agents",
        count: agents.len(),
        header: Row::new([
            Cell::from("process"),
            Cell::from("project"),
            num("pid".into()),
            num("cores".into()),
            num("mem".into()),
        ]),
        rows,
        widths: vec![Constraint::Fill(2), Constraint::Fill(1), PID, CORES, MEM],
    }
}

fn orphans_section(orphans: &[Orphan]) -> Section {
    let rows = orphans
        .iter()
        .map(|o| {
            Row::new([
                num(o.pid.to_string()),
                Cell::from(o.name.clone()),
                Cell::from(o.from.clone()),
                num(fmt_cores(o.cpu)),
                num(fmt_bytes(o.mem)),
            ])
            .style(Style::new().fg(Color::Yellow))
        })
        .collect();
    Section {
        name: "orphans",
        count: orphans.len(),
        header: Row::new([
            num("pid".into()),
            Cell::from("process"),
            Cell::from("from"),
            num("cores".into()),
            num("mem".into()),
        ]),
        rows,
        widths: vec![PID, Constraint::Fill(1), Constraint::Fill(2), CORES, MEM],
    }
}

fn ports_section(ports: &[PortRow]) -> Section {
    let rows = ports
        .iter()
        .map(|p| {
            let style = if p.owner.is_some() {
                Style::new().fg(Color::Cyan)
            } else {
                Style::new()
            };
            Row::new([
                Cell::from(format!(":{}", p.port)),
                num(p.pid.map_or_else(|| "-".into(), |p| p.to_string())),
                Cell::from(p.name.clone()),
                Cell::from(p.owner.clone().unwrap_or_default()),
            ])
            .style(style)
        })
        .collect();
    Section {
        name: "ports",
        count: ports.len(),
        header: Row::new([
            Cell::from("port"),
            num("pid".into()),
            Cell::from("process"),
            Cell::from("agent"),
        ]),
        rows,
        widths: vec![
            Constraint::Length(6),
            PID,
            Constraint::Fill(1),
            Constraint::Fill(2),
        ],
    }
}

/// Stacks sections top to bottom. Every section keeps its title; data rows
/// go to the earlier sections first, and a header only shows with at least
/// one row under it.
fn stack<const N: usize>(frame: &mut Frame, sections: [Section; N], area: Rect, chrome: Chrome) {
    let mut left = area.height;
    let mut heights = [0; N];
    for h in &mut heights {
        *h = chrome.overhead().min(left);
        left -= *h;
    }
    for (h, section) in heights.iter_mut().zip(&sections) {
        if section.rows.is_empty() || left < 2 {
            continue;
        }
        let rows = u16::try_from(section.rows.len() + 1)
            .unwrap_or(u16::MAX)
            .min(left);
        *h += rows;
        left -= rows;
    }
    let areas: [Rect; N] = Layout::vertical(heights.map(Constraint::Length)).areas(area);
    for (section, area) in sections.into_iter().zip(areas) {
        draw_section(frame, section, area, chrome);
    }
}

fn draw_section(frame: &mut Frame, section: Section, area: Rect, chrome: Chrome) {
    let title = format!("{} {}", section.name, section.count);
    let table = Table::new(section.rows, section.widths).header(section.header.style(DIM));
    match chrome {
        Chrome::Borderless => {
            let [head, body] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            let rule = "─".repeat(usize::from(head.width).saturating_sub(title.len() + 1));
            frame.render_widget(Line::styled(format!("{title} {rule}"), DIM), head);
            frame.render_widget(table, body);
        }
        Chrome::Rounded => {
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(DIM)
                .title(Line::styled(format!(" {title} "), DIM));
            frame.render_widget(table.block(block), area);
        }
    }
}

fn draw_help(frame: &mut Frame) {
    let lines = [
        "q        quit",
        "Esc      close help, else quit",
        "?        toggle this help",
        "b        toggle borders (dev)",
    ];
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 4;
    let [area] = Layout::horizontal([Constraint::Length(width as u16)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(lines.len() as u16 + 2)])
        .flex(Flex::Center)
        .areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" help ")
        .padding(Padding::horizontal(1));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines.map(Line::from).to_vec()).block(block),
        area,
    );
}

/// Right-aligned, so numbers line up by their last digit.
fn num(text: String) -> Cell<'static> {
    Cell::from(Line::from(text).alignment(Alignment::Right))
}

/// Which agent a pid belongs to, for tagging ports.
fn owner(app: &App, pid: Pid) -> Option<String> {
    if let Some(agent) = app
        .agents
        .iter()
        .find(|a| a.procs.iter().any(|p| p.pid == pid))
    {
        return Some(agent.label());
    }
    let orphan = app.orphans.iter().find(|o| o.pid == pid)?;
    Some(format!("orphan:{}", orphan.from))
}

fn ratio((used, total): (u64, u64)) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
    }
}

/// `12.1/31G`, padded to the width of a full one so the bar does not jitter.
fn usage_label((used, total): (u64, u64)) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let label = |used: u64| format!("{:.1}/{:.0}G", used as f64 / GIB, total as f64 / GIB);
    let width = label(total).len();
    format!("{:>width$}", label(used))
}

/// A bar `width` cells wide in eighths of a cell: each braille cell fills its
/// left column bottom-up, then its right column. Any use above zero shows at
/// least one dot.
fn braille_bar(ratio: f64, width: usize) -> String {
    const PARTIAL: [char; 8] = ['⠀', '⡀', '⡄', '⡆', '⡇', '⣇', '⣧', '⣷'];
    let steps = width * 8;
    let ratio = ratio.clamp(0.0, 1.0);
    let mut filled = (ratio * steps as f64).round() as usize;
    if ratio > 0.0 {
        filled = filled.max(1).min(steps);
    }
    let (full, part) = (filled / 8, filled % 8);
    let mut bar = "⣿".repeat(full);
    if full < width {
        bar.push(PARTIAL[part]);
        bar.push_str(&"⠀".repeat(width - full - 1));
    }
    bar
}

fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// sysinfo reports process CPU as percent of one core; show it as cores used.
fn fmt_cores(pct: f32) -> String {
    format!("{:.2}", pct / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::agents::{self, ProcInfo};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn root_proc(pid: u32, cmd: &str, dir: &str, mem: u64) -> (Pid, ProcInfo) {
        let info = ProcInfo {
            pid: Pid::from_u32(pid),
            parent: Some(Pid::from_u32(1)),
            start: 0,
            name: cmd.into(),
            cmd: vec![cmd.into()],
            cpu: 12.0,
            mem,
            cwd: Some(PathBuf::from(dir)),
        };
        (info.pid, info)
    }

    /// The 74x9 mockup from #6: three agents, one orphan, two ports.
    fn lines(chrome: Chrome) -> Vec<String> {
        lines_at(chrome, 9)
    }

    fn lines_at(chrome: Chrome, height: u16) -> Vec<String> {
        let procs: HashMap<Pid, ProcInfo> = [
            root_proc(4123, "claude", "/x/hum", 310 << 20),
            root_proc(5871, "claude", "/x/infra", 280 << 20),
            root_proc(6002, "codex", "/x/dots", 190 << 20),
        ]
        .into();
        let agents = agents::scan(&procs, None);
        let orphans = [Orphan {
            pid: Pid::from_u32(9120),
            name: "node".into(),
            from: "claude/hum".into(),
            cpu: 0.0,
            mem: 48 << 20,
        }];
        let screen = Screen {
            cpu: 0.34,
            mem: (GIB * 121 / 10, 31 * GIB),
            swap: (GIB / 1000, 8 * GIB),
            agents: &agents,
            orphans: &orphans,
            ports: vec![
                PortRow {
                    port: 3000,
                    pid: Some(Pid::from_u32(9120)),
                    name: "node".into(),
                    owner: Some("claude/hum".into()),
                },
                PortRow {
                    port: 5432,
                    pid: None,
                    name: "postgres".into(),
                    owner: None,
                },
            ],
        };
        let view = View {
            chrome,
            help: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(74, height)).unwrap();
        terminal.draw(|f| render(f, &screen, &view)).unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..74).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn borderless_fits_the_small_pane() {
        let lines = lines(Chrome::Borderless);
        assert!(lines[0].starts_with("cpu ⣿"), "{}", lines[0]);
        for label in [" 34%", "   mem ⣿", " 12.1/31G", "   swp ⡀", " 0.0/8G"] {
            assert!(lines[0].contains(label), "{label:?} in {}", lines[0]);
        }
        assert_eq!(lines[1], format!("agents 3 {}", "─".repeat(65)));
        assert!(lines[2].starts_with("process") && lines[2].ends_with("mem"));
        assert!(lines[3].starts_with("claude") && lines[3].ends_with("310.0M"));
        assert!(lines[5].starts_with("codex") && lines[5].contains("dots"));
        // One row left is too few for a header and a data row.
        assert_eq!(lines[6], format!("orphans 1 {}", "─".repeat(64)));
        assert_eq!(lines[7], format!("ports 2 {}", "─".repeat(66)));
    }

    #[test]
    fn rounded_keeps_every_title_in_the_small_pane() {
        let lines = lines(Chrome::Rounded);
        assert!(lines[0].starts_with("cpu ⣿"), "{}", lines[0]);
        assert_eq!(lines[1], format!("╭ agents 3 {}╮", "─".repeat(62)));
        assert!(lines[2].starts_with("│process") && lines[2].ends_with("mem│"));
        assert!(lines[3].starts_with("│claude") && lines[3].ends_with("310.0M│"));
        assert_eq!(lines[4], format!("╰{}╯", "─".repeat(72)));
        assert_eq!(lines[5], format!("╭ orphans 1 {}╮", "─".repeat(61)));
        assert_eq!(lines[7], format!("╭ ports 2 {}╮", "─".repeat(63)));
        assert_eq!(lines[8], format!("╰{}╯", "─".repeat(72)));
    }

    #[test]
    fn braille_bar_steps_in_eighths_of_a_cell() {
        assert_eq!(braille_bar(0.0, 4), "⠀⠀⠀⠀");
        assert_eq!(braille_bar(1.0, 4), "⣿⣿⣿⣿");
        // 4 cells = 32 steps; 0.5 fills 2 cells, 3/32 adds three left dots.
        assert_eq!(braille_bar(0.5, 4), "⣿⣿⠀⠀");
        assert_eq!(braille_bar(3.0 / 32.0, 4), "⡆⠀⠀⠀");
        assert_eq!(braille_bar(7.0 / 32.0, 4), "⣷⠀⠀⠀");
    }

    #[test]
    fn braille_bar_shows_any_use_and_clamps() {
        assert_eq!(braille_bar(0.001, 4), "⡀⠀⠀⠀");
        assert_eq!(braille_bar(1.5, 2), "⣿⣿");
        assert_eq!(braille_bar(-1.0, 2), "⠀⠀");
        assert_eq!(braille_bar(0.5, 0), "");
    }

    #[test]
    fn gauges_get_a_blank_row_below_when_rows_allow() {
        let lines = lines_at(Chrome::Rounded, 16);
        assert!(lines[0].starts_with("cpu ⣿"), "{}", lines[0]);
        assert_eq!(lines[1].trim(), "");
        assert!(lines[2].starts_with("╭ agents 3 "), "{}", lines[2]);
    }

    #[test]
    fn sections_name_their_columns() {
        let lines = lines_at(Chrome::Rounded, 16);
        let words = |l: &str| {
            l.split(['│', ' '])
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(words(&lines[3]), "process project pid cores mem");
        assert!(lines[4].starts_with("│claude"), "{}", lines[4]);
    }
}
