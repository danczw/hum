use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table};
use sysinfo::Pid;

use crate::App;
use crate::agents::{self, Agent, Orphan};
use crate::theme::Theme;

// Numbers get fixed widths and text columns fill what is left, so a narrow
// pane truncates names, never numbers. Fill ranks below Length in ratatui's
// layout, Min ranks above it.
/// pid_max is at most 4194304 on 64-bit Linux: 7 digits.
const PID: Constraint = Constraint::Length(7);
/// `fmt_cores` up to 99.99 cores.
const CORES: Constraint = Constraint::Length(5);
/// `fmt_bytes` up to `1023.9M`.
const MEM: Constraint = Constraint::Length(7);
/// Agent process names, tree indent included, get at least this many chars.
const PROCESS: u16 = 12;
/// Project names get at least this many chars.
const PROJECT: u16 = 9;
/// The agents box without its name columns: pid, cores, mem, four gaps, two
/// borders and a blank column inside each.
const AGENTS_FIXED: u16 = 7 + 5 + 7 + 4 + 2 + 2;
/// The agents box with both names at their minimum.
const AGENTS_MIN: u16 = PROCESS + PROJECT + AGENTS_FIXED;
/// Orphan process names are cut to the width of their `process` header.
const ORPHAN_NAME: usize = 7;
/// Fits a whole orphan row such as `9120 node claude/hum 0.00 48.0M` and
/// the `process` header.
const SIDE_MIN: u16 = 45;
/// Narrower panes stack the sections; wider ones put orphans and ports in a
/// right-hand column.
const WIDE: u16 = AGENTS_MIN + SIDE_MIN;

/// From this height up a blank row separates the gauge line from the
/// sections. In shorter panes that row goes to data.
const AIRY: u16 = 12;

/// Rows a section's rounded box takes without data rows: top and bottom
/// border.
const BOX: u16 = 2;

/// What the user toggles while hum runs.
pub struct View {
    pub help: bool,
    pub theme: Theme,
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
    /// Draws the count in the title; `None` keeps it dim like the rest.
    count_style: Option<Style>,
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
    let theme = &view.theme;
    let pad = u16::from(frame.area().height >= AIRY);
    let [gauges, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(pad),
        Constraint::Fill(1),
    ])
    .areas(frame.area());
    draw_gauges(frame, screen, gauges, theme);

    let orphans = orphans_section(screen.orphans, theme);
    let ports = ports_section(&screen.ports);
    if body.width >= WIDE {
        let side = (body.width * 2 / 5).max(SIDE_MIN);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(side)]).areas(body);
        draw_section(
            frame,
            agents_section(screen.agents, left.width, theme),
            left,
            theme,
        );
        stack(frame, [orphans, ports], right, theme);
    } else {
        let agents = agents_section(screen.agents, body.width, theme);
        stack(frame, [agents, orphans, ports], body, theme);
    }

    if view.help {
        draw_help(frame);
    }
}

/// `cpu ⣿⣿⡆⠀ 34%   mem ⣿⣿⣿⠀ 12.1/31G   swp ⡀⠀⠀⠀ 0.0/8G` on one line. The
/// bars share one length so they compare at a glance, and stretch to fill
/// the line. The unused part stays blank, no track. Each bar takes one
/// color from its load.
fn draw_gauges(frame: &mut Frame, screen: &Screen, area: Rect, theme: &Theme) {
    let gauges = [
        (
            "cpu",
            screen.cpu,
            // as wide as `100%` so the bars do not jitter
            format!("{:>4}", format!("{:.0}%", screen.cpu * 100.0)),
        ),
        ("mem", ratio(screen.mem), usage_label(screen.mem)),
        ("swp", ratio(screen.swap), usage_label(screen.swap)),
    ];
    const GAP: u16 = 3;
    // name, space, bar, space, label
    let text = |(name, _, label): &(&str, f64, String)| name.len() + label.len() + 2;
    let fixed: usize = gauges.iter().map(text).sum::<usize>() + usize::from(GAP) * 2;
    let bar = usize::from(area.width).saturating_sub(fixed) / gauges.len();
    let widths = gauges
        .each_ref()
        .map(|g| Constraint::Length((text(g) + bar) as u16));
    let areas: [Rect; 3] = Layout::horizontal(widths).spacing(GAP).areas(area);
    for ((name, ratio, label), area) in gauges.into_iter().zip(areas) {
        let bar = braille_bar(ratio, bar);
        // a blank cell ends the used part
        let (used, unused) = bar.split_at(bar.find('⠀').unwrap_or(bar.len()));
        // every label ends in a one-char unit: `%` or `G`
        let (value, unit) = label.split_at(label.len() - 1);
        let line = Line::from(vec![
            Span::styled(name, theme.dim),
            Span::raw(" "),
            Span::styled(used.to_string(), theme.load(ratio)),
            Span::styled(unused.to_string(), theme.dim),
            Span::raw(" "),
            Span::raw(value.to_string()),
            Span::styled(unit.to_string(), theme.dim),
        ]);
        frame.render_widget(line, area);
    }
}

/// `width` is the whole box. Width beyond both names' minimum goes 3:1 to
/// process and project, and names longer than their column end in `…`.
fn agents_section(agents: &[Agent], width: u16, theme: &Theme) -> Section {
    let names = width.saturating_sub(AGENTS_FIXED);
    // below both minimums project gives way first
    let project =
        names.saturating_sub(PROCESS).min(PROJECT) + names.saturating_sub(PROCESS + PROJECT) / 4;
    let process = names - project;
    let (process_max, project_max) = (usize::from(process), usize::from(project));
    let rows = agents
        .iter()
        .flat_map(|agent| {
            let root = &agent.procs[0];
            let head = Row::new([
                Cell::from(cut(agent.kind, process_max)),
                Cell::from(cut(&agent.project, project_max)),
                num(root.pid.to_string()),
                num(fmt_cores(agent.cpu())),
                num(fmt_bytes(agent.mem())),
            ])
            .style(theme.accent);
            let kids = agent.procs[1..].iter().map(|p| {
                Row::new([
                    Cell::from(cut(
                        &format!("{}└ {}", "  ".repeat(p.depth - 1), p.name),
                        process_max,
                    )),
                    // Cell::default() spans zero columns and would shift the rest.
                    Cell::from(""),
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
        count_style: None,
        header: Row::new([
            Cell::from("process"),
            Cell::from("project"),
            num("pid".into()),
            num("cores".into()),
            num("mem".into()),
        ]),
        rows,
        widths: vec![
            Constraint::Length(process),
            Constraint::Length(project),
            PID,
            CORES,
            MEM,
        ],
    }
}

/// Orphan rows warn, except `from`: agent tags keep the default color.
fn orphans_section(orphans: &[Orphan], theme: &Theme) -> Section {
    let rows = orphans
        .iter()
        .map(|o| {
            Row::new([
                num(o.pid.to_string()).style(theme.warn),
                Cell::from(cut(&o.name, ORPHAN_NAME)).style(theme.warn),
                Cell::from(o.from.clone()),
                num(fmt_cores(o.cpu)).style(theme.warn),
                num(fmt_bytes(o.mem)).style(theme.warn),
            ])
        })
        .collect();
    Section {
        name: "orphans",
        count: orphans.len(),
        count_style: (!orphans.is_empty()).then_some(theme.warn),
        header: Row::new([
            num("pid".into()),
            Cell::from("process"),
            Cell::from("from"),
            num("cores".into()),
            num("mem".into()),
        ]),
        rows,
        // name as wide as its `process` header; `from` takes the rest
        widths: vec![
            PID,
            Constraint::Length(ORPHAN_NAME as u16),
            Constraint::Fill(1),
            CORES,
            MEM,
        ],
    }
}

fn ports_section(ports: &[PortRow]) -> Section {
    let rows = ports
        .iter()
        .map(|p| {
            Row::new([
                Cell::from(format!(":{}", p.port)),
                num(p.pid.map_or_else(|| "-".into(), |p| p.to_string())),
                Cell::from(p.name.clone()),
                Cell::from(p.owner.clone().unwrap_or_default()),
            ])
        })
        .collect();
    Section {
        name: "ports",
        count: ports.len(),
        count_style: None,
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
/// one row under it. Rows nobody needs are shared out so the boxes fill
/// the area.
fn stack<const N: usize>(frame: &mut Frame, sections: [Section; N], area: Rect, theme: &Theme) {
    let mut left = area.height;
    let mut heights = [0; N];
    for h in &mut heights {
        *h = BOX.min(left);
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
    let share = left / N as u16;
    for (i, h) in heights.iter_mut().enumerate() {
        // the remainder goes to the first sections
        *h += share + u16::from((i as u16) < left % N as u16);
    }
    let areas: [Rect; N] = Layout::vertical(heights.map(Constraint::Length)).areas(area);
    for (section, area) in sections.into_iter().zip(areas) {
        draw_section(frame, section, area, theme);
    }
}

/// A thin rounded dim box with the title and count in its top border.
fn draw_section(frame: &mut Frame, section: Section, area: Rect, theme: &Theme) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.dim)
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(format!(" {} ", section.name), theme.dim),
            Span::styled(
                section.count.to_string(),
                section.count_style.unwrap_or(theme.dim),
            ),
            Span::styled(" ", theme.dim),
        ]));
    let table = Table::new(section.rows, section.widths)
        .header(section.header.style(theme.dim))
        .block(block);
    frame.render_widget(table, area);
}

fn draw_help(frame: &mut Frame) {
    let lines = [
        "q        quit",
        "Esc      close help, else quit",
        "?        toggle this help",
        "p        flip palette (dev)",
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

/// `text` up to `max` chars, ending in `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
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
/// The total drops a trailing `.0` (`8G`) but keeps other decimals, so a
/// 400M swapfile reads `0.4G`, not `0G`.
fn usage_label((used, total): (u64, u64)) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let total = format!("{:.1}", total as f64 / GIB);
    let total = total.strip_suffix(".0").unwrap_or(&total);
    let label = |used: f64| format!("{used:.1}/{total}G");
    let width = label(total.parse().unwrap_or(0.0)).len();
    format!("{:>width$}", label(used as f64 / GIB))
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
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    use crate::agents::{self, ProcInfo};
    use crate::theme::Palette;

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

    fn child_proc(pid: u32, parent: u32, cmd: &str) -> (Pid, ProcInfo) {
        let (pid, mut info) = root_proc(pid, cmd, "/", 1 << 20);
        info.parent = Some(Pid::from_u32(parent));
        (pid, info)
    }

    /// The mockup from #6: three agents, one orphan, two ports.
    fn mockup() -> Vec<(Pid, ProcInfo)> {
        vec![
            root_proc(4123, "claude", "/x/hum", 310 << 20),
            root_proc(5871, "claude", "/x/infra", 280 << 20),
            root_proc(6002, "codex", "/x/dots", 190 << 20),
        ]
    }

    /// The mockup in a 74x9 pane.
    fn lines() -> Vec<String> {
        lines_at(9)
    }

    fn lines_at(height: u16) -> Vec<String> {
        draw_lines((74, height), mockup())
    }

    fn draw_lines((width, height): (u16, u16), procs: Vec<(Pid, ProcInfo)>) -> Vec<String> {
        let buf = draw((width, height), procs, Theme::palette(Palette::Ansi));
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn draw((width, height): (u16, u16), procs: Vec<(Pid, ProcInfo)>, theme: Theme) -> Buffer {
        let procs: HashMap<Pid, ProcInfo> = procs.into_iter().collect();
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
        let view = View { help: false, theme };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, &screen, &view)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The color of the first cell where `text` first shows, `skip` chars in.
    fn fg_at(buf: &Buffer, text: &str, skip: usize) -> Color {
        let rows = (0..buf.area.height).map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        });
        for (y, row) in rows.enumerate() {
            if let Some(i) = row.find(text) {
                let x = row[..i].chars().count() + skip;
                return buf[(x as u16, y as u16)].fg;
            }
        }
        panic!("{text:?} not drawn");
    }

    fn fg(style: Style) -> Color {
        style.fg.unwrap_or(Color::Reset)
    }

    #[test]
    fn roles_color_their_rows() {
        let theme = Theme::palette(Palette::Ansi);
        let buf = draw((74, 16), mockup(), theme);
        assert_eq!(fg_at(&buf, "agents 3", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "│ claude", 2), fg(theme.accent));
        assert_eq!(fg_at(&buf, "orphans 1", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "orphans 1", 8), fg(theme.warn));
        // the orphan row warns but its agent tag does not
        assert_eq!(fg_at(&buf, "9120 node", 0), fg(theme.warn));
        assert_eq!(fg_at(&buf, "node    claude/hum", 8), Color::Reset);
        // ports carry no role
        assert_eq!(fg_at(&buf, ":3000", 0), Color::Reset);
    }

    #[test]
    fn gauges_take_their_load_color() {
        let theme = Theme::palette(Palette::Ansi);
        let buf = draw((74, 9), mockup(), theme);
        // cpu at 34% is low
        assert_eq!(fg_at(&buf, "cpu ⣿", 4), fg(theme.load_low));
        assert_eq!(fg_at(&buf, "cpu", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "%", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "G ", 0), fg(theme.dim));
    }

    #[test]
    fn no_color_draws_no_colors() {
        let buf = draw((74, 16), mockup(), Theme::no_color());
        for cell in buf.content() {
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
        }
    }

    #[test]
    fn every_title_fits_the_small_pane() {
        let lines = lines();
        assert!(lines[0].starts_with("cpu ⣿"), "{}", lines[0]);
        for label in [" 34%", "   mem ⣿", " 12.1/31G", "   swp ⡀", " 0.0/8G"] {
            assert!(lines[0].contains(label), "{label:?} in {}", lines[0]);
        }
        assert_eq!(lines[1], format!("╭ agents 3 {}╮", "─".repeat(62)));
        assert!(lines[2].starts_with("│ process") && lines[2].ends_with("mem │"));
        assert!(lines[3].starts_with("│ claude") && lines[3].ends_with("310.0M │"));
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
        let lines = lines_at(16);
        assert!(lines[0].starts_with("cpu ⣿"), "{}", lines[0]);
        assert_eq!(lines[1].trim(), "");
        assert!(lines[2].starts_with("╭ agents 3 "), "{}", lines[2]);
    }

    #[test]
    fn sections_name_their_columns() {
        let lines = lines_at(16);
        let words = |l: &str| {
            l.split(['│', ' '])
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(words(&lines[3]), "process project pid cores mem");
        assert!(lines[4].starts_with("│ claude"), "{}", lines[4]);
    }

    #[test]
    fn long_names_are_cut_so_93_cols_fit_side_by_side() {
        let mut procs = mockup();
        procs.extend([
            child_proc(9169, 4123, "rust-analyzer"),
            child_proc(9342, 9169, "rust-analyzer-proc-macro-srv"),
            root_proc(7000, "claude", "/x/claude-code-plugins", 1 << 20),
        ]);
        let lines = draw_lines((93, 20), procs);
        // orphans sit right of agents
        assert!(
            lines[2].starts_with("╭ agents 4 ") && lines[2].contains("╭ orphans 1 "),
            "{}",
            lines[2]
        );
        assert!(
            lines.iter().any(|l| l.starts_with("│   └ rust-an… ")),
            "{lines:#?}"
        );
        let pid_end = |pid: &str| {
            let line = lines.iter().find(|l| l.contains(pid)).unwrap();
            line[..line.find(pid).unwrap()].chars().count() + pid.len()
        };
        assert_eq!(pid_end("9342"), pid_end("4123"), "{lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains(" claude-c… ")),
            "{lines:#?}"
        );
        // the orphan row keeps its whole `from`
        assert!(lines[4].contains("claude/hum"), "{}", lines[4]);
    }

    #[test]
    fn wider_panes_grow_process_more_than_project() {
        let mut procs = mockup();
        procs.extend([
            child_proc(9169, 4123, "rust-analyzer"),
            child_proc(9342, 9169, "rust-analyzer-proc-macro-srv"),
            root_proc(7000, "claude", "/x/claude-code-plugins", 1 << 20),
        ]);
        let lines = draw_lines((148, 20), procs);
        let header = &lines[3];
        let col = |w: &str| header[..header.find(w).unwrap()].chars().count();
        let (process, project) = (col("project") - col("process"), col("pid") - col("project"));
        assert!(process > project && project > 11, "{process} {project}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("└ rust-analyzer-proc-macro-srv")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.contains(" claude-code-plugins ")));
    }

    #[test]
    fn side_column_fills_the_height_like_agents() {
        let lines = draw_lines((93, 20), mockup());
        let last = lines.last().unwrap();
        // agents and ports both close on the bottom row
        assert_eq!(last.matches('╰').count(), 2, "{lines:#?}");
        // spare rows are shared, so orphans grow too
        let orphans_end = lines
            .iter()
            .position(|l| l.ends_with('╯') && l.matches('╰').count() == 1);
        assert!(orphans_end.is_some_and(|y| y > 5), "{lines:#?}");
    }

    #[test]
    fn narrow_panes_cut_project_before_process() {
        let lines = draw_lines((40, 9), mockup());
        assert!(lines[3].starts_with("│ claude "), "{lines:#?}");
    }
}
