use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState,
};

use crate::agents::{self, Agent, Orphan, Owner};
use crate::label::cut;
use crate::nav::outline::{self, Entry};
use crate::nav::{AGENTS, Lines, Lists, Nav, ORPHANS, PORTS, SECTIONS, Window};
use crate::ports::Listener;
use crate::theme::Theme;

// Numbers get fixed widths and text columns fill what is left, so a narrow
// pane truncates names, never numbers. Fill ranks below Length in ratatui's
// layout, Min ranks above it.
/// pid_max is at most 4194304 on 64-bit Linux: 7 digits.
const PID_WIDTH: u16 = 7;
const PID: Constraint = Constraint::Length(PID_WIDTH);
/// `fmt_cores` up to 99.99 cores.
const CORES_WIDTH: u16 = 5;
const CORES: Constraint = Constraint::Length(CORES_WIDTH);
/// Agents and orphans show cores as the 4th of 5 columns. Panes too narrow
/// for their name columns drop it and its gap.
const CORES_COL: usize = 3;
const CORES_SPAN: u16 = CORES_WIDTH + 1;
/// `fmt_bytes` up to `1023.9M`.
const MEM_WIDTH: u16 = 7;
const MEM: Constraint = Constraint::Length(MEM_WIDTH);
/// `:65535*`
const PORT_WIDTH: u16 = 7;
/// Agent process names, tree indent included, get at least this many chars.
const PROCESS: u16 = 12;
/// Project names get at least this many chars.
const PROJECT: u16 = 9;
/// The agents box without its name columns: pid, cores, mem, four gaps, two
/// borders and a blank column inside each.
const AGENTS_FIXED: u16 = PID_WIDTH + CORES_WIDTH + MEM_WIDTH + 4 + 2 + 2;
/// The agents box with both names at their minimum.
const AGENTS_MIN: u16 = PROCESS + PROJECT + AGENTS_FIXED;
/// Orphan process names are cut to the width of their `process` header.
const ORPHAN_NAME: usize = 7;
/// The orphans box without `from`: pid, name, cores, mem, four gaps, two
/// borders and a blank column inside each.
const ORPHANS_FIXED: u16 = PID_WIDTH + ORPHAN_NAME as u16 + CORES_WIDTH + MEM_WIDTH + 4 + 2 + 2;
/// Fits a whole orphan row such as `9120 node claude/hum 0.00 48.0M` and
/// the `process` header. Narrower orphan sections drop cores.
const SIDE_MIN: u16 = ORPHANS_FIXED + "claude/hum".len() as u16;
/// The ports box without process and agent: port, pid, three gaps, two
/// borders and a blank column inside each.
const PORTS_FIXED: u16 = PORT_WIDTH + PID_WIDTH + 3 + 2 + 2;
/// Narrower panes stack the sections; wider ones put orphans and ports in a
/// right-hand column.
const WIDE: u16 = AGENTS_MIN + SIDE_MIN;

/// From this height up a blank row separates the gauge line from the
/// sections and each section is a box with a header. Shorter panes are
/// compact: that row goes to data and the sections of a column share one
/// box, see [`stack`].
const AIRY: u16 = 12;

/// Between the gauges on their line.
const GAP: u16 = 3;
/// Between columns of core bars; their brackets already set them apart.
const CORE_GAP: u16 = 2;
/// Per-core bars get at least this many cells.
const CORE_BAR: u16 = 8;

/// Below this size only the gauge line and a hint show.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 6;

/// Rows a section's rounded box takes without data rows: top and bottom
/// border.
const BOX: u16 = 2;

/// The top of a section below another in a compact box: `├ orphans: 1 ─┤`.
const DIVIDER: border::Set = border::Set {
    top_left: "├",
    top_right: "┤",
    ..border::ROUNDED
};

/// Everything one frame shows, filled once per tick so drawing reads no
/// `System`.
pub struct Snapshot {
    pub gauges: Gauges,
    pub agents: Vec<Agent>,
    pub orphans: Vec<Orphan>,
    pub ports: Vec<Listener>,
}

/// The gauge line and the per-core grid.
pub struct Gauges {
    /// 0.0 to 1.0.
    pub cpu: f64,
    /// Each core's use, 0.0 to 1.0.
    pub cores: Vec<f64>,
    /// (used, total) bytes.
    pub mem: (u64, u64),
    pub swap: (u64, u64),
}

/// A titled list of rows under a dim header naming the columns.
struct Section {
    name: &'static str,
    count: usize,
    /// Rows before a filter, the 7 in `ports: 2/7`; `None` when it hid none.
    total: Option<usize>,
    count_style: Style,
    header: Row<'static>,
    rows: Vec<Row<'static>>,
    widths: Vec<Constraint>,
    /// The rows as short entries, set in place of `rows` for a section that
    /// folds into the bottom border of a compact box; only ports do.
    inline: Option<Vec<Line<'static>>>,
}

/// Where one frame puts each part, worked out before drawing so [`Nav`]
/// can fit its cursors to the lines each section gets.
pub struct Plan {
    gauges: Rect,
    /// Below [`MIN_WIDTH`] x [`MIN_HEIGHT`], the line for a hint instead.
    body: Result<Body, Rect>,
}

/// Everything below the gauge line.
struct Body {
    grid: Option<(CoreGrid, Rect)>,
    compact: bool,
    /// Indexed by [`AGENTS`], [`ORPHANS`] and [`PORTS`].
    slots: [Slot; SECTIONS],
    help: Option<HelpBox>,
}

/// Where a section draws.
enum Slot {
    Rows(Rows),
    /// Folded into the bottom border of a compact box, see [`draw_footer`].
    Footer(Rect),
}

/// A box, or a part of a compact one, with `lines` data lines below a
/// header when `header`.
struct Rows {
    area: Rect,
    borders: Borders,
    /// Opens with `├`, below another section in a compact box.
    divider: bool,
    header: bool,
    lines: u16,
}

impl Slot {
    fn width(&self) -> u16 {
        match self {
            Self::Rows(Rows { area, .. }) | Self::Footer(area) => area.width,
        }
    }
}

/// A section's rounded box, or its part of a compact one.
fn block(borders: Borders, divider: bool) -> Block<'static> {
    Block::new()
        .borders(borders)
        .border_set(if divider { DIVIDER } else { border::ROUNDED })
}

impl Plan {
    /// The lines each section and the help get, for [`Nav::frame`].
    pub fn lines(&self) -> Lines {
        let Ok(Body { slots, help, .. }) = &self.body else {
            return Lines::default();
        };
        Lines {
            sections: slots.each_ref().map(|slot| match slot {
                Slot::Rows(rows) => Some(usize::from(rows.lines)),
                Slot::Footer(_) => None,
            }),
            help_overflow: help.as_ref().map_or(0, |h| h.lines.len() - h.shown),
        }
    }
}

pub fn plan(area: Rect, snapshot: &Snapshot, lists: &Lists, nav: &Nav) -> Plan {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let [gauges, hint] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
        return Plan {
            gauges,
            body: Err(hint),
        };
    }
    let grid = nav
        .cores()
        .then(|| CoreGrid::new(snapshot.gauges.cores.len(), area.width));
    let wide = area.width >= WIDE;
    // the rows compact sections need for their titles: three stacked, two
    // side by side
    let titles = if wide { 2 } else { 3 };
    let grid_rows = grid.map_or(0, |g| {
        g.rows.min(area.height / 2).min(area.height - 1 - titles)
    });
    // the sections lay out like a pane as tall as the rows the grid leaves
    let compact = area.height - grid_rows < AIRY;
    let [gauges, cores, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(grid_rows),
        Constraint::Length(u16::from(!compact)),
        Constraint::Fill(1),
    ])
    .areas(area);

    let rows = [lists.agents.len(), lists.orphans.len(), lists.ports.len()];
    let shows = [AGENTS, ORPHANS, PORTS].map(|s| nav.shows(s));
    // ports end the stack they are in, so a compact box folds them
    let slots = if wide {
        let side = (body.width * 2 / 5).max(SIDE_MIN);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(side)]).areas(body);
        let [agents] = stack([rows[AGENTS]], [shows[AGENTS]], left, compact, false);
        let [orphans, ports] = stack(
            [rows[ORPHANS], rows[PORTS]],
            [shows[ORPHANS], shows[PORTS]],
            right,
            compact,
            compact,
        );
        [agents, orphans, ports]
    } else {
        stack(rows, shows, body, compact, compact)
    };
    Plan {
        gauges,
        body: Ok(Body {
            grid: grid.map(|g| (g, cores)),
            compact,
            slots,
            help: nav.help().map(|_| HelpBox::new(area)),
        }),
    }
}

/// Paints `plan` with the rows `windows` shows of each section.
pub fn draw(
    frame: &mut Frame,
    snapshot: &Snapshot,
    lists: &Lists,
    nav: &Nav,
    theme: &Theme,
    plan: Plan,
    windows: &[Window; SECTIONS],
) {
    draw_gauges(frame, &snapshot.gauges, plan.gauges, theme);
    let Body {
        grid,
        compact,
        slots,
        help,
    } = match plan.body {
        Ok(body) => body,
        Err(hint) => {
            frame.render_widget(Line::styled("too small", theme.dim), hint);
            return;
        }
    };
    if let Some((grid, area)) = grid {
        draw_cores(frame, &snapshot.gauges.cores, grid, area, theme);
    }
    let sections = [
        agents_section(&lists.agents, slots[AGENTS].width(), theme),
        orphans_section(lists.orphans, slots[ORPHANS].width(), theme),
        ports_section(
            &lists.ports,
            snapshot.ports.len(),
            slots[PORTS].width(),
            compact,
            theme,
        ),
    ];
    let parts = sections.into_iter().zip(slots).zip(windows);
    for (i, ((section, slot), window)) in parts.enumerate() {
        match slot {
            Slot::Rows(rows) => draw_section(frame, section, rows, nav.shows(i), window, theme),
            Slot::Footer(area) => draw_footer(frame, section, area, theme),
        }
    }
    if let (Some(help), Some(scroll)) = (help, nav.help()) {
        draw_help(frame, help, scroll);
    }
}

/// `cpu ⎣⣿⣿⡆⠀  34%⎦   mem ⎣⣿⣿⣿⠀ 12.1/31G⎦   swp ⎣⡀⠀⠀⠀ 0.0/8G⎦` on one
/// line, bracketed as in htop. The bars share one length so they compare at
/// a glance, and stretch to fill the line. The unused part stays blank, no
/// track. Each bar takes one color from its load.
fn draw_gauges(frame: &mut Frame, gauges: &Gauges, area: Rect, theme: &Theme) {
    let gauges = [
        (
            "cpu",
            gauges.cpu,
            // as wide as `100` so the bars do not jitter
            format!("{:>3.0}", gauges.cpu * 100.0),
            "%",
        ),
        ("mem", ratio(gauges.mem), usage_label(gauges.mem), "G"),
        ("swp", ratio(gauges.swap), usage_label(gauges.swap), "G"),
    ];
    // name, ` ⎣`, bar, space, value, unit, `⎦`
    let text = |(name, _, value, unit): &(&str, f64, String, &str)| {
        name.len() + value.len() + unit.len() + 4
    };
    let fixed: usize = gauges.iter().map(text).sum::<usize>() + usize::from(GAP) * 2;
    let bar = usize::from(area.width).saturating_sub(fixed) / gauges.len();
    let widths = gauges
        .each_ref()
        .map(|g| Constraint::Length((text(g) + bar) as u16));
    let areas: [Rect; 3] = Layout::horizontal(widths).spacing(GAP).areas(area);
    for ((name, ratio, value, unit), area) in gauges.into_iter().zip(areas) {
        let (used, unused) = braille_bar(ratio, bar);
        let line = Line::from(vec![
            Span::styled(name, theme.dim),
            Span::styled(" ⎣", theme.dim),
            Span::styled(used, theme.load(ratio)),
            Span::styled(unused, theme.dim),
            Span::raw(" "),
            Span::raw(value),
            Span::styled(unit, theme.dim),
            Span::styled("⎦", theme.dim),
        ]);
        frame.render_widget(line, area);
    }
}

/// How per-core bars pack into a pane's width: as many columns as fit bars
/// of [`CORE_BAR`] cells, then as few as fill the rows, so no column is
/// empty. The bars stretch to fill the line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CoreGrid {
    cols: u16,
    rows: u16,
    /// Bar width in cells.
    bar: u16,
    /// Digits of the highest core index.
    index: u16,
}

impl CoreGrid {
    fn new(cores: usize, width: u16) -> Self {
        let index = cores.saturating_sub(1).to_string().len() as u16;
        let cores = u16::try_from(cores).unwrap_or(u16::MAX);
        if cores == 0 {
            return Self {
                cols: 0,
                rows: 0,
                bar: 0,
                index,
            };
        }
        let fixed = Self::fixed(index);
        let fit = (width + CORE_GAP) / (fixed + CORE_BAR + CORE_GAP);
        let rows = cores.div_ceil(fit.clamp(1, cores));
        let cols = cores.div_ceil(rows);
        let bar = (width.saturating_sub((cols - 1) * CORE_GAP) / cols).saturating_sub(fixed);
        Self {
            cols,
            rows,
            bar,
            index,
        }
    }

    /// A cell without its bar: index, ` ⎣`, ` 77%⎦`.
    fn fixed(index: u16) -> u16 {
        index + 2 + 6
    }

    /// One core's cell: index, space, bar and percent in brackets.
    fn cell(&self) -> u16 {
        Self::fixed(self.index) + self.bar
    }
}

/// ` 5 ⎣⣿⣿⣿⣿⣿⣿⡆⠀⠀  77%⎦` per core, as in htop, filling down each column
/// first. The bar takes its load color and the rest stays dim, to set the
/// grid apart from the gauge line. When `area` is shorter than the grid, the
/// last cell counts the cores left out as `+N cores`.
fn draw_cores(frame: &mut Frame, cores: &[f64], grid: CoreGrid, area: Rect, theme: &Theme) {
    let rows = usize::from(area.height);
    if rows == 0 {
        return;
    }
    let slots = rows * usize::from(grid.cols);
    let shown = if cores.len() > slots {
        slots - 1
    } else {
        cores.len()
    };
    let cols: Vec<Rect> =
        Layout::horizontal(vec![Constraint::Length(grid.cell()); grid.cols.into()])
            .spacing(CORE_GAP)
            .split(area)
            .to_vec();
    let at = |i: usize| Rect {
        y: area.y + (i % rows) as u16,
        height: 1,
        ..cols[i / rows]
    };
    for (i, &ratio) in cores.iter().enumerate().take(shown) {
        let (used, unused) = braille_bar(ratio, grid.bar.into());
        let line = Line::from(vec![
            Span::styled(format!("{i:>w$} ⎣", w = usize::from(grid.index)), theme.dim),
            Span::styled(used, theme.load(ratio)),
            Span::styled(unused, theme.dim),
            Span::styled(format!(" {:>3.0}%⎦", ratio * 100.0), theme.dim),
        ]);
        frame.render_widget(line, at(i));
    }
    if shown < cores.len() {
        let more = format!("+{} cores", cores.len() - shown);
        frame.render_widget(Line::styled(more, theme.dim), at(shown));
    }
}

/// `width` is the whole box. Width beyond both names' minimum goes 3:1 to
/// process and project, and names longer than their column end in `…`.
/// Below both minimums the cores column goes first, then project gives way.
fn agents_section(entries: &[Entry], width: u16, theme: &Theme) -> Section {
    let cores = width >= AGENTS_MIN;
    let names = name_room(width, AGENTS_FIXED, cores);
    let project =
        names.saturating_sub(PROCESS).min(PROJECT) + names.saturating_sub(PROCESS + PROJECT) / 4;
    let process = names - project;
    let (process_max, project_max) = (usize::from(process), usize::from(project));
    let rows = entries
        .iter()
        .map(|entry| match entry.line {
            outline::Line::Root { agent, open } => {
                // `▸ ● claude (14)` collapsed, `▾ ● claude` open; a long kind
                // is cut before the count
                let (marker, count) = match agent.children() {
                    0 => (' ', String::new()),
                    _ if open => ('▾', String::new()),
                    n => ('▸', format!(" ({n})")),
                };
                let (status, status_style) = match agent.busy() {
                    true => ('●', theme.accent),
                    false => ('○', theme.dim),
                };
                let kind = cut(agent.kind, process_max.saturating_sub(4 + count.len()));
                let name = Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled(status.to_string(), status_style),
                    Span::raw(format!(" {kind}{count}")),
                ]);
                Row::new(columns(
                    [
                        Cell::from(name),
                        Cell::from(agent.label().fit_project(project_max)),
                        num(agent.root().pid.to_string()),
                        num(fmt_cores(agent.cpu)),
                        num(fmt_bytes(agent.mem())),
                    ],
                    cores,
                ))
                .style(theme.accent)
            }
            outline::Line::Subagent(s) => {
                let mut cells = [""; 5].map(Cell::from);
                cells[0] = Cell::from(cut(&branch(1, &format!("◇ {}", s.kind)), process_max));
                Row::new(columns(cells, cores))
            }
            outline::Line::Proc(p) => Row::new(columns(
                [
                    Cell::from(cut(&branch(p.depth, &p.name), process_max)),
                    // Cell::default() spans zero columns and would shift the rest.
                    Cell::from(""),
                    num(p.pid.to_string()),
                    num(fmt_cores(p.cpu)),
                    num(fmt_bytes(p.mem)),
                ],
                cores,
            )),
        })
        .collect();
    Section {
        name: "agents",
        count: entries
            .iter()
            .filter(|e| matches!(e.line, outline::Line::Root { .. }))
            .count(),
        total: None,
        count_style: theme.dim,
        header: Row::new(columns(
            [
                Cell::from("process"),
                Cell::from("project"),
                num("pid".into()),
                num("cores".into()),
                num("mem".into()),
            ],
            cores,
        )),
        rows,
        widths: columns(
            [
                Constraint::Length(process),
                Constraint::Length(project),
                PID,
                CORES,
                MEM,
            ],
            cores,
        ),
        inline: None,
    }
}

/// Orphan rows warn, except `from`: agent tags keep the default color.
/// `width` is the whole box.
fn orphans_section(orphans: &[Orphan], width: u16, theme: &Theme) -> Section {
    let cores = width >= SIDE_MIN;
    let from_max = usize::from(name_room(width, ORPHANS_FIXED, cores));
    let rows = orphans
        .iter()
        .map(|o| {
            let from = o.from.fit(from_max);
            Row::new(columns(
                [
                    num(o.pid.to_string()),
                    Cell::from(cut(&o.name, ORPHAN_NAME)),
                    Cell::from(from).style(Style::reset()),
                    num(fmt_cores(o.cpu)),
                    num(fmt_bytes(o.mem)),
                ],
                cores,
            ))
            .style(theme.warn)
        })
        .collect();
    Section {
        name: "orphans",
        count: orphans.len(),
        total: None,
        count_style: if orphans.is_empty() {
            theme.dim
        } else {
            theme.warn
        },
        header: Row::new(columns(
            [
                num("pid".into()),
                Cell::from("process"),
                Cell::from("from"),
                num("cores".into()),
                num("mem".into()),
            ],
            cores,
        )),
        rows,
        // name as wide as its `process` header; `from` takes the rest
        widths: columns(
            [
                PID,
                Constraint::Length(ORPHAN_NAME as u16),
                Constraint::Fill(1),
                CORES,
                MEM,
            ],
            cores,
        ),
        inline: None,
    }
}

/// Width a box of `width` leaves its name columns once `fixed` is taken,
/// counting the cores column back in when the box drops it.
fn name_room(width: u16, fixed: u16, cores: bool) -> u16 {
    (width + if cores { 0 } else { CORES_SPAN }).saturating_sub(fixed)
}

/// The columns of an agents or orphans row, without cores unless `cores`.
fn columns<T>(columns: [T; 5], cores: bool) -> Vec<T> {
    let mut columns = Vec::from(columns);
    if !cores {
        columns.remove(CORES_COL);
    }
    columns
}

/// `name` at `depth` below its agent, 1 for a direct child, past the
/// agent's status dot, so an agent without a fold marker never reads as a
/// child. Subagents and processes share it, so neither reads as a child
/// of the other.
fn branch(depth: usize, name: &str) -> String {
    format!("{}└ {name}", "  ".repeat(depth + 1))
}

/// `shown` of `total` ports, see [`Nav::ports`]. A port bound beyond
/// loopback gets a `*`, which warns when an agent or orphan owns it.
/// `width` is the whole box. `compact`, the ports fold into its bottom
/// border, see [`draw_footer`].
fn ports_section(
    shown: &[&Listener],
    total: usize,
    width: u16,
    compact: bool,
    theme: &Theme,
) -> Section {
    // process and agent split the rest 1:2; the floor never overshoots
    let agent_max = usize::from(width.saturating_sub(PORTS_FIXED)) * 2 / 3;
    // `:3000*`
    let port = |p: &Listener| {
        let star = match (p.public, p.owner.is_some()) {
            (false, _) => None,
            (true, true) => Some(Span::styled("*", theme.warn)),
            (true, false) => Some(Span::raw("*")),
        };
        let mut spans = vec![Span::raw(format!(":{}", p.port))];
        spans.extend(star);
        spans
    };
    // `:3000* node claude/hum`, or `:5432 postgres` without an owner
    let inline = |p: &&Listener| {
        let mut spans = port(p);
        spans.extend(
            std::iter::once(p.name().to_string())
                .chain(p.owner.as_ref().map(Owner::to_string))
                .filter(|s| !s.is_empty())
                .map(|s| Span::raw(format!(" {s}"))),
        );
        Line::from(spans)
    };
    let row = |p: &&Listener| {
        Row::new([
            Cell::from(Line::from(port(p))),
            num(p.pid().map_or_else(|| "-".into(), |p| p.to_string())),
            Cell::from(p.name().to_string()),
            Cell::from(
                p.owner
                    .as_ref()
                    .map_or_else(String::new, |o| o.fit(agent_max)),
            ),
        ])
    };
    let (rows, inline) = if compact {
        (Vec::new(), Some(shown.iter().map(inline).collect()))
    } else {
        (shown.iter().map(row).collect(), None)
    };
    Section {
        name: "ports",
        count: shown.len(),
        total: (shown.len() < total).then_some(total),
        count_style: theme.dim,
        header: Row::new([
            Cell::from("port"),
            num("pid".into()),
            Cell::from("process"),
            Cell::from("agent"),
        ]),
        rows,
        widths: vec![
            Constraint::Length(PORT_WIDTH),
            PID,
            Constraint::Fill(1),
            Constraint::Fill(2),
        ],
        inline,
    }
}

/// Stacks sections of `rows` rows top to bottom. Every section keeps its
/// title. A section that `shows` its cursor first gets a line for it, then
/// data rows go to the earlier sections first. Rows nobody needs are shared
/// out so the boxes fill the area.
///
/// Each section is a box, and a header shows over at least one row or in
/// an empty section. `compact`, the sections share one box split by
/// `├ name ─┤` lines and drop their headers. `footer`, the last section
/// folds into the bottom border of a compact box.
fn stack<const N: usize>(
    mut rows: [usize; N],
    shows: [bool; N],
    area: Rect,
    compact: bool,
    footer: bool,
) -> [Slot; N] {
    debug_assert!(compact || !footer, "only a compact box folds a footer");
    if footer {
        // its entries go in the border, not in rows
        rows[N - 1] = 0;
    }
    let header = u16::from(!compact);
    let mut left = area.height;
    let mut heights = [0; N];
    for (i, h) in heights.iter_mut().enumerate() {
        // compact: a title line each, and a bottom border unless the footer
        // is one
        let chrome = if compact {
            1 + u16::from(i == N - 1 && !footer)
        } else {
            BOX
        };
        *h = chrome.min(left);
        left -= *h;
    }
    // header and data lines; a section given one line shows a row without
    // its header
    let mut lines = [0; N];
    for ((l, &shows), &n) in lines.iter_mut().zip(&shows).zip(&rows) {
        if shows && n > 0 {
            *l = left.min(1 + header);
            left -= *l;
        }
    }
    for (l, &n) in lines.iter_mut().zip(&rows) {
        if n == 0 {
            continue;
        }
        let want = u16::try_from(n).unwrap_or(u16::MAX).saturating_add(header);
        let rows = want.saturating_sub(*l).min(left);
        *l += rows;
        left -= rows;
    }
    // the footer stays one line
    let growing = N as u16 - u16::from(footer);
    let share = left / growing;
    for (i, (h, l)) in heights.iter_mut().zip(lines).enumerate() {
        if (i as u16) < growing {
            // the remainder goes to the first sections
            *h += l + share + u16::from((i as u16) < left % growing);
        }
    }
    let areas: [Rect; N] = Layout::vertical(heights.map(Constraint::Length)).areas(area);
    std::array::from_fn(|i| {
        let area = areas[i];
        let (borders, divider) = if !compact {
            (Borders::ALL, false)
        } else if i == N - 1 && footer {
            return Slot::Footer(area);
        } else {
            let bottom = if i == N - 1 {
                Borders::BOTTOM
            } else {
                Borders::NONE
            };
            (
                Borders::TOP | Borders::LEFT | Borders::RIGHT | bottom,
                i > 0,
            )
        };
        let inner = block(borders, divider).inner(area).height;
        // a single line goes to a row, not to a header with nothing under it
        let header = !compact && (inner >= 2 || rows[i] == 0);
        Slot::Rows(Rows {
            area,
            borders,
            divider,
            header,
            // an empty box squeezed to its borders has no line for it
            lines: inner.saturating_sub(u16::from(header)),
        })
    })
}

/// The section's box at `at`, with the title and count in its top border
/// and a dim frame, showing the rows of `window`. A last `+N more` line
/// counts the rows below. When the section `shows` its cursor, its name
/// takes the accent.
fn draw_section(
    frame: &mut Frame,
    section: Section,
    at: Rows,
    shows: bool,
    window: &Window,
    theme: &Theme,
) {
    let Rows {
        area,
        borders,
        divider,
        header,
        ..
    } = at;
    let mut title = title(&section, shows, theme);
    title.push(Span::styled(" ", theme.dim));
    let block = block(borders, divider)
        .border_style(theme.dim)
        .padding(Padding::horizontal(1))
        .title(Line::from(title));
    let inner = block.inner(area);
    let rows = section
        .rows
        .into_iter()
        .skip(window.offset)
        .take(window.rows);
    let mut table = Table::new(rows, section.widths)
        .row_highlight_style(theme.selected)
        .block(block);
    if header {
        table = table.header(section.header.style(theme.dim));
    }
    let mut state = TableState::new()
        .with_selected((shows && window.rows > 0).then(|| window.selected - window.offset));
    frame.render_stateful_widget(table, area, &mut state);
    if window.more > 0 {
        let last = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        frame.render_widget(
            Line::styled(format!("+{} more", window.more), theme.dim),
            last,
        );
    }
}

/// A section folded into the bottom border of a compact box:
/// `╰ ports: 2 · :3000 node claude/hum :5432 postgres ──╯`. Entries that do
/// not fit are counted as `+N`. `Tab` passes over the section.
fn draw_footer(frame: &mut Frame, section: Section, area: Rect, theme: &Theme) {
    let entries = section.inline.as_deref().unwrap_or_default();
    let mut spans = title(&section, false, theme);
    // corners, the closing space and at least one rule
    let width = spans.iter().map(Span::width).sum::<usize>();
    let mut room = usize::from(area.width).saturating_sub(width + 4);
    let sep = |i| if i == 0 { " · " } else { "  " };
    let mut shown = 0;
    for (i, entry) in entries.iter().enumerate() {
        let sep = sep(i);
        let len = sep.chars().count() + entry.width();
        // keep room for `  +N` after this entry
        let rest = entries.len() - i - 1;
        let tail = if rest == 0 {
            0
        } else {
            3 + rest.to_string().len()
        };
        if len + tail > room {
            break;
        }
        room -= len;
        spans.push(Span::styled(sep, theme.dim));
        // reset: title cells keep the border's dim color
        spans.extend(
            entry
                .spans
                .iter()
                .map(|s| s.clone().style(Style::reset().patch(s.style))),
        );
        shown += 1;
    }
    if shown < entries.len() {
        spans.push(Span::styled(sep(shown), theme.dim));
        spans.push(Span::styled(
            format!("+{}", entries.len() - shown),
            theme.dim,
        ));
    }
    spans.push(Span::styled(" ", theme.dim));
    let block = Block::new()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_type(BorderType::Rounded)
        .border_style(theme.dim)
        .title_bottom(Line::from(spans));
    frame.render_widget(block, area);
}

/// ` agents: 3` for a section's border, without the closing space.
fn title(section: &Section, shows: bool, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(" ", theme.dim),
        Span::styled(section.name, if shows { theme.accent } else { theme.dim }),
        Span::styled(": ", theme.dim),
        Span::styled(
            match section.total {
                Some(total) => format!("{}/{total}", section.count),
                None => section.count.to_string(),
            },
            section.count_style,
        ),
    ]
}

const HELP: [&str; 14] = [
    "j k ↑ ↓  move cursor, scroll help",
    "g G      first, last row",
    "PgUp/Dn  move a page",
    "Tab      next section",
    "S-Tab    previous section",
    "Enter ␣  toggle agent tree",
    "l →      expand agent tree",
    "h ←      collapse, or go to root",
    "e c      expand, collapse all",
    "1        per-core cpu",
    "a        all or agent ports",
    "Esc      close help/cursor or quit",
    "q        quit",
    "?        toggle this help",
];

/// `HELP` in `n` columns, read down then across.
fn help_columns(n: usize) -> Vec<String> {
    let columns: Vec<&[&str]> = HELP.chunks(HELP.len().div_ceil(n)).collect();
    let widths: Vec<usize> = columns
        .iter()
        .map(|c| c.iter().map(|l| l.chars().count()).max().unwrap_or(0))
        .collect();
    (0..columns[0].len())
        .map(|row| {
            let cells: Vec<String> = columns
                .iter()
                .zip(&widths)
                .filter_map(|(c, w)| c.get(row).map(|l| format!("{l:<w$}")))
                .collect();
            cells.join("  ").trim_end().to_string()
        })
        .collect()
}

/// Where the help shows over a pane.
struct HelpBox {
    area: Rect,
    /// [`HELP`] in as many columns as fit.
    lines: Vec<String>,
    /// Lines that fit at once.
    shown: usize,
}

impl HelpBox {
    fn new(pane: Rect) -> Self {
        let rows = (pane.height as usize).saturating_sub(2).max(1);
        // text plus border and padding
        let width =
            |lines: &[String]| lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 4;
        // enough columns to show every line, fewer when they do not fit across
        let lines = (1..=HELP.len().div_ceil(rows))
            .rev()
            .map(help_columns)
            .find(|lines| width(lines) <= pane.width as usize)
            .unwrap_or_else(|| help_columns(1));
        let shown = lines.len().min(rows);
        let [area] = Layout::horizontal([Constraint::Length(width(&lines) as u16)])
            .flex(Flex::Center)
            .areas(pane);
        let [area] = Layout::vertical([Constraint::Length(shown as u16 + 2)])
            .flex(Flex::Center)
            .areas(area);
        Self { area, lines, shown }
    }
}

/// The help scrolled down `scroll` lines, as [`Nav::frame`] clamps it.
fn draw_help(frame: &mut Frame, help: HelpBox, scroll: usize) {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(if scroll > 0 { " help ↑ " } else { " help " })
        .padding(Padding::horizontal(1));
    if scroll + help.shown < help.lines.len() {
        block = block.title_bottom(" ↓ ");
    }
    frame.render_widget(Clear, help.area);
    frame.render_widget(
        Paragraph::new(help.lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .block(block)
            .scroll((scroll as u16, 0)),
        help.area,
    );
}

/// Right-aligned, so numbers line up by their last digit.
fn num(text: String) -> Cell<'static> {
    Cell::from(Line::from(text).alignment(Alignment::Right))
}

fn ratio((used, total): (u64, u64)) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
    }
}

/// `12.1/31` in GiB, padded to the width of a full one so the bar does not
/// jitter. The total drops a trailing `.0` (`8`) but keeps other decimals,
/// so a 400M swapfile reads `0.4`, not `0`.
fn usage_label((used, total): (u64, u64)) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let total = format!("{:.1}", total as f64 / GIB);
    let total = total.strip_suffix(".0").unwrap_or(&total);
    let label = |used: f64| format!("{used:.1}/{total}");
    let width = label(total.parse().unwrap_or(0.0)).len();
    format!("{:>width$}", label(used as f64 / GIB))
}

/// A bar `width` cells wide in eighths of a cell: each braille cell fills its
/// left column bottom-up, then its right column. Any use above zero shows at
/// least one dot. Returns the used cells and the blank rest.
fn braille_bar(ratio: f64, width: usize) -> (String, String) {
    const PARTIAL: [char; 8] = ['⠀', '⡀', '⡄', '⡆', '⡇', '⣇', '⣧', '⣷'];
    let steps = width * 8;
    let ratio = ratio.clamp(0.0, 1.0);
    let mut filled = (ratio * steps as f64).round() as usize;
    if ratio > 0.0 {
        filled = filled.max(1).min(steps);
    }
    let (full, part) = (filled / 8, filled % 8);
    let mut used = "⣿".repeat(full);
    if part > 0 {
        used.push(PARTIAL[part]);
    }
    let unused = "⠀".repeat(width - full - usize::from(part > 0));
    (used, unused)
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

fn fmt_cores(pct: f32) -> String {
    format!("{:.2}", agents::cores(pct))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};
    use sysinfo::Pid;

    use crate::agents::{self, BusyTracker, ProcInfo};
    use crate::claude::Subagent;
    use crate::host::MemHost;
    use crate::label::Label;
    use crate::nav::Intent;
    use crate::ports::Holder;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn root_proc(pid: u32, cmd: &str, dir: &str, mem: u64) -> (Pid, ProcInfo) {
        let (pid, mut info) = agents::fixture::proc(pid, 1, 0, cmd);
        info.cpu = 12.0;
        info.mem = mem;
        info.cwd = Some(PathBuf::from(dir));
        (pid, info)
    }

    fn child_proc(pid: u32, parent: u32, cmd: &str) -> (Pid, ProcInfo) {
        let (pid, mut info) = root_proc(pid, cmd, "/", 1 << 20);
        info.parent = Some(Pid::from_u32(parent));
        (pid, info)
    }

    /// The mockup from #6: three agents, one orphan, three ports.
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
        text(&draw((width, height), procs, Theme::ayu_mirage()))
    }

    /// Draws with every tree expanded.
    fn draw(size: (u16, u16), procs: Vec<(Pid, ProcInfo)>, theme: Theme) -> Buffer {
        let mut view = View { theme, ..view() };
        expand_all(&mut view, &procs);
        draw_view(size, procs, &mut view)
    }

    /// Takes in `procs` and expands every tree, before the first draw.
    fn expand_all(view: &mut View, procs: &[(Pid, ProcInfo)]) {
        view.load(snapshot(scan(procs), eight_cores()));
        view.press(Intent::ExpandAll);
    }

    fn scan(procs: &[(Pid, ProcInfo)]) -> Vec<Agent> {
        agents::scan(
            &procs.iter().cloned().collect(),
            None,
            &[],
            None,
            &MemHost::default(),
        )
    }

    /// Draws with `view`, so its cursor carries over to the next frame.
    fn draw_view(size: (u16, u16), procs: Vec<(Pid, ProcInfo)>, view: &mut View) -> Buffer {
        draw_cores_view(size, procs, eight_cores(), view)
    }

    /// One core pegged, one at the mid load, the rest low.
    fn eight_cores() -> Vec<f64> {
        vec![0.21, 0.06, 1.0, 0.17, 0.0, 0.7, 0.3, 0.12]
    }

    fn claude(project: &str, session: Option<&str>) -> Label {
        Label {
            kind: "claude",
            project: Some(project.into()),
            session: session.map(Into::into),
        }
    }

    fn agent_owner(project: &str, session: Option<&str>) -> Option<Owner> {
        Some(Owner {
            label: claude(project, session),
            orphan: false,
        })
    }

    /// The mockup's gauge line over `cores`.
    fn gauges(cores: Vec<f64>) -> Gauges {
        Gauges {
            cpu: 0.34,
            cores,
            mem: (GIB * 121 / 10, 31 * GIB),
            swap: (GIB / 1000, 8 * GIB),
        }
    }

    fn port(port: u16, pid: u32, name: &str, owner: Option<Owner>) -> Listener {
        Listener {
            port,
            public: false,
            holder: Some(Holder {
                pid: Pid::from_u32(pid),
                start: 0,
                name: name.into(),
            }),
            owner,
        }
    }

    /// Draws every port in `ports` with no agents or orphans.
    fn draw_ports(size: (u16, u16), ports: Vec<Listener>) -> Vec<String> {
        let mut view = view();
        view.press(Intent::AllPorts);
        view.load(Snapshot { ports, ..empty() });
        text(&render(size, &mut view))
    }

    fn draw_cores_view(
        (width, height): (u16, u16),
        procs: Vec<(Pid, ProcInfo)>,
        cores: Vec<f64>,
        view: &mut View,
    ) -> Buffer {
        let procs: HashMap<Pid, ProcInfo> = procs.into_iter().collect();
        draw_agents(
            (width, height),
            agents::scan(&procs, None, &[], None, &MemHost::default()),
            cores,
            view,
        )
    }

    fn draw_agents(
        size: (u16, u16),
        agents: Vec<Agent>,
        cores: Vec<f64>,
        view: &mut View,
    ) -> Buffer {
        view.load(snapshot(agents, cores));
        render(size, view)
    }

    /// Draws the snapshot `view` holds, as `run` does.
    fn render((width, height): (u16, u16), view: &mut View) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| crate::render(f, &view.snapshot, &mut view.nav, &view.theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The mockup's gauges, orphan and ports with `agents`.
    fn snapshot(agents: Vec<Agent>, cores: Vec<f64>) -> Snapshot {
        Snapshot {
            gauges: gauges(cores),
            agents,
            orphans: vec![Orphan {
                pid: Pid::from_u32(9120),
                name: "node".into(),
                from: claude("hum", None),
                start: 0,
                cpu: 0.0,
                mem: 48 << 20,
            }],
            ports: vec![
                Listener {
                    public: true,
                    ..port(3000, 9120, "node", agent_owner("hum", None))
                },
                port(5432, 1342, "postgres", None),
                port(8000, 5871, "python", agent_owner("infra", None)),
            ],
        }
    }

    fn empty() -> Snapshot {
        Snapshot {
            gauges: gauges(eight_cores()),
            agents: Vec::new(),
            orphans: Vec::new(),
            ports: Vec::new(),
        }
    }

    /// What `run` carries from frame to frame.
    struct View {
        nav: Nav,
        theme: Theme,
        /// The snapshot drawn last, which keys act on.
        snapshot: Snapshot,
    }

    impl View {
        fn press(&mut self, intent: Intent) {
            self.nav.handle(intent, &self.snapshot);
        }

        /// Takes in `snapshot` as a refresh does.
        fn load(&mut self, snapshot: Snapshot) {
            self.snapshot = snapshot;
            self.nav.observe(&self.snapshot);
        }
    }

    fn view() -> View {
        View {
            nav: Nav::default(),
            theme: Theme::ayu_mirage(),
            snapshot: empty(),
        }
    }

    fn text(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// The line the selected row's tint starts on.
    fn selected_line(buf: &Buffer, theme: &Theme) -> Option<String> {
        let lines = text(buf);
        (0..buf.area.height)
            .find(|&y| {
                let cell = &buf[(2, y)];
                theme
                    .selected
                    .bg
                    .map_or(cell.modifier.contains(Modifier::REVERSED), |bg| {
                        cell.bg == bg
                    })
            })
            .map(|y| lines[usize::from(y)].clone())
    }

    /// One claude session with `kids` children: 1 + `kids` agent rows.
    fn big_tree(kids: u32) -> Vec<(Pid, ProcInfo)> {
        let mut procs = vec![root_proc(4000, "claude", "/x/hum", 1 << 20)];
        procs.extend((1..=kids).map(|i| child_proc(5000 + i, 4000, "mcp")));
        procs
    }

    /// The color of the first cell where `text` first shows, `skip` chars in.
    fn fg_at(buf: &Buffer, needle: &str, skip: usize) -> Color {
        for (y, row) in text(buf).into_iter().enumerate() {
            if let Some(i) = row.find(needle) {
                let x = row[..i].chars().count() + skip;
                return buf[(x as u16, y as u16)].fg;
            }
        }
        panic!("{needle:?} not drawn");
    }

    fn fg(style: Style) -> Color {
        style.fg.unwrap_or_default()
    }

    #[test]
    fn sessions_sharing_a_project_show_their_names_in_74x10() {
        let named = |pid, name: &str| {
            let (pid, mut info) = root_proc(pid, "claude", "/x/hum", 100 << 20);
            info.cmd = ["claude", "--name", name]
                .map(std::ffi::OsString::from)
                .to_vec();
            (pid, info)
        };
        let procs = vec![
            named(4123, "gate"),
            named(4124, "exec"),
            named(4125, "watch"),
        ];
        let lines = draw_lines((74, 10), procs);
        for label in ["hum·gate", "hum·exec", "hum·watch"] {
            assert!(
                lines.iter().any(|l| l.contains(label)),
                "{label} {lines:#?}"
            );
        }
    }

    #[test]
    fn roles_color_their_rows() {
        let theme = Theme::ayu_mirage();
        let buf = draw((74, 16), mockup(), theme);
        assert_eq!(fg_at(&buf, "agents: 3", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "│   ○ claude", 6), fg(theme.accent));
        assert_eq!(fg_at(&buf, "│   ○ claude", 4), fg(theme.dim));
        assert_eq!(fg_at(&buf, "orphans: 1", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "orphans: 1", 9), fg(theme.warn));
        // the orphan row warns but its agent tag does not
        assert_eq!(fg_at(&buf, "9120 node", 0), fg(theme.warn));
        assert_eq!(fg_at(&buf, "node    claude/hum", 8), Default::default());
        // ports carry no role
        assert_eq!(fg_at(&buf, ":3000", 0), Default::default());
    }

    #[test]
    fn busy_sessions_mark_their_root_in_accent() {
        let theme = Theme::ayu_mirage();
        let mut procs = mockup();
        procs[2].1.cpu = 0.0;
        let procs: HashMap<Pid, ProcInfo> = procs.into_iter().collect();
        let mut agents = agents::scan(&procs, None, &[], None, &MemHost::default());
        let mut busy = BusyTracker::default();
        for _ in 0..10 {
            busy.update(&mut agents);
        }
        let mut view = View { theme, ..view() };
        let buf = draw_agents((74, 16), agents, eight_cores(), &mut view);
        assert_eq!(fg_at(&buf, "│   ● claude", 4), fg(theme.accent));
        assert_eq!(fg_at(&buf, "│   ○ codex", 4), fg(theme.dim));
    }

    #[test]
    fn gauges_take_their_load_color() {
        let theme = Theme::ayu_mirage();
        let buf = draw((74, 9), mockup(), theme);
        // cpu at 34% is low
        assert_eq!(fg_at(&buf, "cpu ⎣⣿", 5), fg(theme.load_low));
        assert_eq!(fg_at(&buf, "cpu", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "%", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "G⎦", 0), fg(theme.dim));
    }

    #[test]
    fn no_color_draws_no_colors() {
        let buf = draw((74, 16), mockup(), Theme::no_color());
        for cell in buf.content() {
            assert_eq!((cell.fg, cell.bg), Default::default(), "{cell:?}");
        }
    }

    #[test]
    fn small_pane_draws_the_mockup() {
        let lines = lines();
        assert!(lines[0].starts_with("cpu ⎣⣿"), "{}", lines[0]);
        for label in [" 34%⎦", "   mem ⎣⣿", " 12.1/31G⎦", "   swp ⎣⡀", " 0.0/8G⎦"] {
            assert!(lines[0].contains(label), "{label:?} in {}", lines[0]);
        }
        assert_eq!(lines[1], format!("╭ agents: 3 {}╮", "─".repeat(61)));
        for (line, pid) in lines[2..5].iter().zip(["4123", "5871", "6002"]) {
            assert!(
                line.starts_with("│   ○ c") && line.contains(pid),
                "{lines:#?}"
            );
            assert!(line.ends_with("M │"), "{lines:#?}");
        }
        // the spare row stays inside the box
        assert!(lines[5].trim_matches(['│', ' ']).is_empty(), "{lines:#?}");
        assert_eq!(lines[6], format!("├ orphans: 1 {}┤", "─".repeat(60)));
        assert!(lines[7].starts_with("│    9120 node "), "{lines:#?}");
        // postgres has no agent and stays hidden
        let ports = "╰ ports: 2/3 · :3000* node claude/hum  :8000 python claude/infra ";
        assert_eq!(
            lines[8],
            format!("{ports}{}╯", "─".repeat(73 - ports.chars().count()))
        );
    }

    #[test]
    fn ports_that_do_not_fit_the_border_are_counted() {
        let buf = draw((50, 9), mockup(), Theme::ayu_mirage());
        let last = text(&buf).pop().unwrap();
        assert!(
            last.starts_with("╰ ports: 2/3 · :3000* node claude/hum  +1 ─"),
            "{last}"
        );
        // entries keep the default color, not the border's; `*` warns
        assert_eq!(fg_at(&buf, ":3000", 0), Default::default());
        assert_eq!(
            fg_at(&buf, ":3000*", 5),
            Theme::ayu_mirage().warn.fg.unwrap()
        );
    }

    #[test]
    fn a_shows_ports_without_an_agent() {
        let mut view = view();
        view.press(Intent::AllPorts);
        let buf = draw_view((74, 20), mockup(), &mut view);
        let lines = text(&buf);
        assert!(
            lines.iter().any(|l| l.starts_with("╭ ports: 3 ")),
            "{lines:#?}"
        );
        let postgres = lines.iter().find(|l| l.contains("postgres")).unwrap();
        assert!(postgres.starts_with("│ :5432 "), "{lines:#?}");
    }

    #[test]
    fn unreadable_holder_shows_a_dash_and_no_name() {
        let lines = draw_ports(
            (74, 20),
            vec![Listener {
                port: 5432,
                public: false,
                holder: None,
                owner: None,
            }],
        );
        let row = lines.iter().find(|l| l.contains(":5432")).unwrap();
        assert_eq!(
            row.trim_end_matches([' ', '│']),
            "│ :5432         -",
            "{lines:#?}"
        );
    }

    #[test]
    fn public_port_without_an_agent_gets_a_plain_star() {
        let ports = [Listener {
            port: 22,
            public: true,
            holder: None,
            owner: None,
        }];
        let theme = Theme::ayu_mirage();
        let inline = ports_section(&[&ports[0]], 1, 74, true, &theme)
            .inline
            .unwrap();
        assert_eq!(inline[0].spans[1].content, "*");
        assert_eq!(inline[0].spans[1].style, Style::default());
    }

    #[test]
    fn narrow_ports_box_keeps_the_session() {
        let port = |p, session| port(p, 9120, "node", agent_owner("hum", Some(session)));
        // 40 wide leaves the agent column 12 chars, less than `claude/hum·gate`
        let lines = draw_ports((40, 20), vec![port(3000, "gate"), port(3001, "exec")]);
        for label in ["claude…·gate", "claude…·exec"] {
            assert!(
                lines.iter().any(|l| l.contains(label)),
                "{label} {lines:#?}"
            );
        }
    }

    #[test]
    fn narrow_ports_box_keeps_the_orphan_prefix() {
        let owner = Owner {
            label: claude("hum", Some("gate")),
            orphan: true,
        };
        // the 12-char agent column fits `orphan:` and the session, not the
        // label between them
        let lines = draw_ports((40, 20), vec![port(3000, 9120, "node", Some(owner))]);
        assert!(
            lines.iter().any(|l| l.contains(" orphan:·gate ")),
            "{lines:#?}"
        );
    }

    #[test]
    fn filter_hiding_nothing_counts_plainly() {
        let ports = [Listener {
            public: true,
            ..port(3000, 9120, "node", agent_owner("hum", None))
        }];
        let shown: Vec<_> = ports.iter().collect();
        let section = ports_section(&shown, ports.len(), 74, false, &Theme::ayu_mirage());
        assert_eq!((section.count, section.total), (1, None));
    }

    #[test]
    fn empty_section_keeps_its_divider() {
        let lines = draw_lines((74, 9), big_tree(29));
        let at = lines.iter().position(|l| l.starts_with("├ orphans: 1 "));
        // agents take every row before orphans
        assert_eq!(at, Some(7), "{lines:#?}");
        assert!(lines[8].starts_with("╰ ports: 2/3 "), "{lines:#?}");
    }

    #[test]
    fn tiny_panes_show_only_the_gauges() {
        for size in [(39, 9), (74, 5)] {
            let lines = draw_lines(size, mockup());
            assert!(lines[0].starts_with("cpu "), "{lines:#?}");
            assert_eq!(lines[1].trim_end(), "too small");
            assert!(lines[2..].iter().all(|l| l.trim().is_empty()), "{lines:#?}");
        }
    }

    #[test]
    fn wide_compact_panes_fold_ports_into_the_side_box() {
        let lines = draw_lines((148, 9), mockup());
        assert!(
            lines[1].starts_with("╭ agents: 3 ") && lines[1].contains("╮╭ orphans: 1 "),
            "{lines:#?}"
        );
        assert!(lines[8].contains("╯╰ ports: 2/3 · :3000* "), "{lines:#?}");
    }

    #[test]
    fn compact_panes_give_folded_ports_no_lines() {
        let mut view = view();
        view.load(snapshot(scan(&mockup()), eight_cores()));
        for (size, ports) in [((74, 9), None), ((92, 9), None), ((74, 16), Some(1))] {
            let area = Rect::new(0, 0, size.0, size.1);
            let lines = plan(
                area,
                &view.snapshot,
                &view.nav.lists(&view.snapshot),
                &view.nav,
            )
            .lines();
            assert_eq!(lines.sections[PORTS], ports, "{size:?}");
        }
        let lines = plan(
            Rect::new(0, 0, 30, 9),
            &view.snapshot,
            &view.nav.lists(&view.snapshot),
            &view.nav,
        )
        .lines();
        assert_eq!(lines.sections, [None; SECTIONS]);
    }

    #[test]
    fn empty_box_squeezed_to_its_borders_gets_no_lines() {
        let area = Rect::new(0, 0, 74, 14);
        let slots = stack([30, 0, 0], [false; 3], area, false, false);
        for slot in &slots[1..] {
            assert!(matches!(slot, Slot::Rows(Rows { lines: 0, .. })));
        }
    }

    #[test]
    fn side_column_starts_at_92_cols() {
        let side = |width| draw_lines((width, 16), mockup())[2].contains("╭ orphans: 1 ");
        assert!(side(92));
        assert!(!side(91));
    }

    #[test]
    fn narrow_panes_drop_the_cores_column() {
        let cores = |width| draw_lines((width, 16), mockup())[3].contains("cores");
        assert!(cores(48));
        assert!(!cores(47));
        let lines = draw_lines((40, 16), mockup());
        // project and orphan `from` keep their room
        assert!(lines[4].contains(" hum "), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("claude/hum")), "{lines:#?}");
    }

    fn bar(ratio: f64, width: usize) -> String {
        let (used, unused) = braille_bar(ratio, width);
        used + &unused
    }

    #[test]
    fn braille_bar_splits_after_the_last_dot() {
        assert_eq!(braille_bar(3.0 / 32.0, 4), ("⡆".into(), "⠀⠀⠀".into()));
        assert_eq!(braille_bar(0.5, 4), ("⣿⣿".into(), "⠀⠀".into()));
        assert_eq!(braille_bar(0.0, 2), ("".into(), "⠀⠀".into()));
    }

    #[test]
    fn braille_bar_steps_in_eighths_of_a_cell() {
        assert_eq!(bar(0.0, 4), "⠀⠀⠀⠀");
        assert_eq!(bar(1.0, 4), "⣿⣿⣿⣿");
        // 4 cells = 32 steps; 0.5 fills 2 cells, 3/32 adds three left dots.
        assert_eq!(bar(0.5, 4), "⣿⣿⠀⠀");
        assert_eq!(bar(3.0 / 32.0, 4), "⡆⠀⠀⠀");
        assert_eq!(bar(7.0 / 32.0, 4), "⣷⠀⠀⠀");
    }

    #[test]
    fn braille_bar_shows_any_use_and_clamps() {
        assert_eq!(bar(0.001, 4), "⡀⠀⠀⠀");
        assert_eq!(bar(1.5, 2), "⣿⣿");
        assert_eq!(bar(-1.0, 2), "⠀⠀");
        assert_eq!(bar(0.5, 0), "");
    }

    #[test]
    fn gauges_get_a_blank_row_below_when_rows_allow() {
        let lines = lines_at(16);
        assert!(lines[0].starts_with("cpu ⎣⣿"), "{}", lines[0]);
        assert_eq!(lines[1].trim(), "");
        assert!(lines[2].starts_with("╭ agents: 3 "), "{}", lines[2]);
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
        assert!(lines[4].starts_with("│   ○ claude"), "{}", lines[4]);
    }

    #[test]
    fn long_names_are_cut_so_92_cols_fit_side_by_side() {
        let mut procs = mockup();
        procs.extend([
            child_proc(9169, 4123, "rust-analyzer"),
            child_proc(9342, 9169, "rust-analyzer-proc-macro-srv"),
            root_proc(7000, "claude", "/x/claude-code-plugins", 1 << 20),
        ]);
        let lines = draw_lines((92, 20), procs);
        // orphans sit right of agents
        assert!(
            lines[2].starts_with("╭ agents: 4 ") && lines[2].contains("╭ orphans: 1 "),
            "{}",
            lines[2]
        );
        assert!(
            lines.iter().any(|l| l.starts_with("│     └ rust-… ")),
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
    fn agents_below_an_open_tree_keep_their_column() {
        let mut procs = mockup();
        procs.extend([
            child_proc(9169, 4123, "bash"),
            child_proc(9342, 9169, "cargo"),
        ]);
        let lines = draw_lines((74, 16), procs);
        let col = |line: &str, glyphs: &[char]| line.chars().position(|c| glyphs.contains(&c));
        let dots: Vec<usize> = lines.iter().filter_map(|l| col(l, &['●', '○'])).collect();
        assert_eq!(dots.len(), 3, "{lines:#?}");
        assert!(dots.iter().all(|&c| c == dots[0]), "{lines:#?}");
        let kids: Vec<usize> = lines.iter().filter_map(|l| col(l, &['└'])).collect();
        assert_eq!(kids.len(), 2, "{lines:#?}");
        assert!(kids.iter().all(|&c| c > dots[0]), "{lines:#?}");
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
        let lines = draw_lines((40, 16), mockup());
        assert!(lines[4].starts_with("│   ○ claude "), "{lines:#?}");
    }

    #[test]
    fn glance_view_shows_no_cursor_and_counts_hidden_rows() {
        let theme = Theme::ayu_mirage();
        let buf = draw((74, 16), big_tree(29), theme);
        assert_eq!(selected_line(&buf, &theme), None);
        let lines = text(&buf);
        let shown = lines.iter().filter(|l| l.contains(" 500")).count();
        let more = lines.iter().find_map(|l| {
            l.strip_prefix("│ +")?
                .split(' ')
                .next()?
                .parse::<usize>()
                .ok()
        });
        assert!(lines[3].starts_with("│ process"), "{lines:#?}");
        assert!(lines[4].starts_with("│ ▾ ○ claude"), "{lines:#?}");
        assert_eq!(Some(29 - shown), more, "{lines:#?}");
    }

    #[test]
    fn cursor_reaches_the_last_of_30_agent_rows_in_74x9() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        expand_all(&mut view, &big_tree(29));
        draw_view((74, 9), big_tree(29), &mut view);
        view.press(Intent::Down);
        for _ in 0..29 {
            draw_view((74, 9), big_tree(29), &mut view);
            view.press(Intent::Down);
        }
        let buf = draw_view((74, 9), big_tree(29), &mut view);
        let line = selected_line(&buf, &theme).unwrap();
        assert!(line.contains("5029"), "{line}");
        assert!(!text(&buf).iter().any(|l| l.contains("more")));
    }

    #[test]
    fn focused_section_keeps_a_line_for_its_cursor() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        expand_all(&mut view, &big_tree(29));
        draw_view((74, 9), big_tree(29), &mut view);
        view.press(Intent::Next);
        view.press(Intent::Next);
        let buf = draw_view((74, 9), big_tree(29), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("9120"));
    }

    #[test]
    fn cursor_moved_off_an_emptied_section_shows_in_the_same_frame() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        expand_all(&mut view, &big_tree(29));
        draw_view((74, 12), big_tree(29), &mut view);
        view.press(Intent::Next);
        view.press(Intent::Next);
        draw_view((74, 12), big_tree(29), &mut view);
        let mut gone = snapshot(scan(&big_tree(29)), eight_cores());
        gone.orphans.clear();
        view.load(gone);
        let buf = render((74, 12), &mut view);
        assert_eq!(fg_at(&buf, "ports", 1), fg(theme.accent));
        assert!(selected_line(&buf, &theme).unwrap().contains("3000"));
    }

    #[test]
    fn short_panes_show_a_row_without_its_header() {
        let theme = Theme::ayu_mirage();
        for height in [8, 10] {
            let mut view = View { theme, ..view() };
            expand_all(&mut view, &big_tree(29));
            draw_view((74, height), big_tree(29), &mut view);
            view.press(Intent::Next);
            view.press(Intent::Next);
            let buf = draw_view((74, height), big_tree(29), &mut view);
            let lines = text(&buf);
            assert!(
                selected_line(&buf, &theme).unwrap().contains("9120"),
                "{lines:#?}"
            );
            for (i, line) in lines.iter().enumerate() {
                if line.starts_with("│ process") {
                    assert!(!lines[i + 1].starts_with('╰'), "{lines:#?}");
                }
            }
        }
    }

    #[test]
    fn tab_moves_the_cursor_to_orphans_and_leaves_agents() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        draw_view((74, 16), mockup(), &mut view);
        // the first Tab shows the cursor in agents
        view.press(Intent::Next);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("4123"));
        view.press(Intent::Next);
        view.press(Intent::Down);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("9120"));
        assert_eq!(fg_at(&buf, "orphans: 1", 1), fg(theme.accent));
        assert_eq!(fg_at(&buf, "agents: 3", 0), fg(theme.dim));
        view.press(Intent::Prev);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("4123"));
    }

    /// The mockup's sessions with children: 4123 has two, 5871 one.
    fn sessions() -> Vec<(Pid, ProcInfo)> {
        let mut procs = mockup();
        procs.extend([
            child_proc(4124, 4123, "bash"),
            child_proc(4125, 4123, "mcp"),
            child_proc(5872, 5871, "bash"),
        ]);
        procs
    }

    /// [`sessions`] with two subagents under the hum session.
    fn with_subagents() -> Vec<Agent> {
        let mut agents = scan(&sessions());
        agents[0].subagents = vec![
            Subagent {
                id: "a1".into(),
                kind: "Explore".into(),
            },
            Subagent {
                id: "a2".into(),
                kind: "code-review".into(),
            },
        ];
        agents
    }

    #[test]
    fn subagents_show_above_the_processes_of_their_agent() {
        let mut view = view();
        let buf = draw_agents((74, 16), with_subagents(), eight_cores(), &mut view);
        let lines = text(&buf);
        assert!(lines[4].starts_with("│ ▸ ● claude (4) "), "{lines:#?}");
        view.press(Intent::ExpandAll);
        let lines = text(&draw_agents(
            (74, 16),
            with_subagents(),
            eight_cores(),
            &mut view,
        ));
        assert!(lines[5].starts_with("│     └ ◇ Explore  "), "{lines:#?}");
        assert!(
            lines[6].starts_with("│     └ ◇ code-review  "),
            "{lines:#?}"
        );
        assert!(lines[7].starts_with("│     └ bash "), "{lines:#?}");
        // no pid, cores or mem of its own
        assert!(
            lines[5].trim_end_matches([' ', '│']).ends_with("Explore"),
            "{lines:#?}"
        );
    }

    #[test]
    fn subagents_fit_the_glance_pane() {
        let mut view = view();
        view.load(snapshot(with_subagents(), eight_cores()));
        view.press(Intent::ExpandAll);
        let lines = text(&draw_agents(
            (74, 10),
            with_subagents(),
            eight_cores(),
            &mut view,
        ));
        // the processes branch from the session beside the subagents
        let rows = [
            "│     └ ◇ Explore ",
            "│     └ ◇ code-review ",
            "│     └ bash ",
        ];
        assert!(
            lines
                .windows(3)
                .any(|w| w.iter().zip(rows).all(|(l, row)| l.starts_with(row))),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_session_with_only_subagents_expands() {
        let agents = || {
            let mut agents = scan(&mockup());
            agents[0].subagents = with_subagents().remove(0).subagents;
            agents
        };
        let mut view = view();
        view.load(snapshot(agents(), eight_cores()));
        view.press(Intent::ExpandAll);
        let lines = text(&draw_agents((74, 16), agents(), eight_cores(), &mut view));
        assert!(lines[4].starts_with("│ ▾ ● claude "), "{lines:#?}");
        assert!(lines[5].starts_with("│     └ ◇ Explore "), "{lines:#?}");
    }

    fn shows(buf: &Buffer, pid: &str) -> bool {
        text(buf).iter().any(|l| l.contains(pid))
    }

    #[test]
    fn help_fits_a_short_pane_in_two_columns() {
        let mut view = view();
        view.press(Intent::Help);
        // too short for one column: two, with every line shown
        for (size, two) in [
            ((74, 10), true),
            ((74, 15), true),
            ((74, 16), false),
            ((74, 30), false),
        ] {
            let lines = text(&draw_view(size, mockup(), &mut view));
            let side_by_side = lines.iter().any(|l| l.contains("g G") && l.contains("e c"));
            assert_eq!(side_by_side, two, "{size:?} {lines:#?}");
            for help in HELP {
                assert!(
                    lines.iter().any(|l| l.contains(help)),
                    "{size:?} {help} {lines:#?}"
                );
            }
        }
        // too narrow for two columns: one, scrolled
        let lines = text(&draw_view((60, 12), mockup(), &mut view));
        assert!(
            !lines.iter().any(|l| l.contains("g G") && l.contains("e c")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.contains(HELP[0])), "{lines:#?}");
    }

    #[test]
    fn help_takes_more_columns_in_a_wide_short_pane() {
        let mut view = view();
        view.press(Intent::Help);
        // 4 rows need 4 columns, 134 wide; 5 rows fit in 3
        for size in [(134, 6), (120, 7)] {
            let lines = text(&draw_view(size, mockup(), &mut view));
            for help in HELP {
                assert!(
                    lines.iter().any(|l| l.contains(help)),
                    "{size:?} {help} {lines:#?}"
                );
            }
            assert!(!lines.iter().any(|l| l.contains("╰ ↓ ")), "{lines:#?}");
        }
    }

    #[test]
    fn help_scrolls_when_too_short_and_narrow() {
        let mut view = view();
        view.press(Intent::Help);
        let last = HELP[HELP.len() - 1];
        // two columns of 7 rows in 6: the last row is below
        let lines = text(&draw_view((74, 8), mockup(), &mut view));
        assert!(!lines.iter().any(|l| l.contains(last)), "{lines:#?}");
        assert!(
            lines[7].contains("╰ ↓ ") && !lines[0].contains('↑'),
            "{lines:#?}"
        );
        for _ in 0..99 {
            view.press(Intent::Down);
        }
        let lines = text(&draw_view((74, 8), mockup(), &mut view));
        assert_eq!(view.nav.help(), Some(1));
        assert!(lines.iter().any(|l| l.contains(last)), "{lines:#?}");
        assert!(
            lines[0].contains(" help ↑ ") && !lines[7].contains('↓'),
            "{lines:#?}"
        );
        // a pane with room shows it all, back at the top
        let lines = text(&draw_view((74, 16), mockup(), &mut view));
        assert_eq!(view.nav.help(), Some(0));
        assert!(lines.iter().any(|l| l.contains(HELP[0])), "{lines:#?}");
    }

    #[test]
    fn trees_start_collapsed_with_their_size() {
        let buf = draw_view((74, 16), sessions(), &mut view());
        let lines = text(&buf);
        assert!(lines[4].starts_with("│ ▸ ○ claude (2) "), "{lines:#?}");
        assert!(lines[5].starts_with("│ ▸ ○ claude (1) "), "{lines:#?}");
        // a session without children has nothing to expand
        assert!(lines[6].starts_with("│   ○ codex "), "{lines:#?}");
        assert!(!shows(&buf, "4124") && !shows(&buf, "5872"));
    }

    #[test]
    fn no_color_reverses_the_selected_row() {
        let mut view = View {
            theme: Theme::no_color(),
            ..view()
        };
        draw_view((74, 16), mockup(), &mut view);
        view.press(Intent::Down);
        let buf = draw_view((74, 16), mockup(), &mut view);
        let y = text(&buf).iter().position(|l| l.contains("4123")).unwrap();
        // the tint spans the row, numbers and gaps alike
        for x in 2..72 {
            let cell = &buf[(x, y as u16)];
            assert!(cell.modifier.contains(Modifier::REVERSED), "{x}");
            assert_eq!((cell.fg, cell.bg), Default::default());
        }
    }

    fn cores_on() -> View {
        let mut view = view();
        view.press(Intent::Cores);
        view
    }

    fn draw_cores_lines(size: (u16, u16), cores: Vec<f64>) -> Vec<String> {
        text(&draw_cores_view(size, mockup(), cores, &mut cores_on()))
    }

    #[test]
    fn one_toggles_a_bar_per_core_down_each_column() {
        let lines = draw_cores_lines((74, 16), eight_cores());
        let starts: Vec<_> = lines[1..3]
            .iter()
            .map(|l| {
                l.split_whitespace()
                    .step_by(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(starts, ["0 2 4 6", "1 3 5 7"], "{lines:#?}");
        assert!(lines[2].ends_with(" 12%⎦"), "{lines:#?}");
        assert_eq!(lines[3].trim(), "");
        assert!(lines[4].starts_with("╭ agents: 3 "), "{lines:#?}");
        // off again, the sections move back up under the gauges
        let off = draw_cores_view((74, 16), mockup(), eight_cores(), &mut view());
        assert!(text(&off)[2].starts_with("╭ agents: 3 "));
    }

    #[test]
    fn core_bars_take_their_load_color_and_the_rest_is_dim() {
        let theme = Theme::ayu_mirage();
        let buf = draw_cores_view((74, 16), mockup(), eight_cores(), &mut cores_on());
        assert_eq!(fg_at(&buf, "2 ⎣⣿", 3), fg(theme.load_high));
        assert_eq!(fg_at(&buf, "5 ⎣⣿", 3), fg(theme.load_mid));
        assert_eq!(fg_at(&buf, "7 ⎣⣿", 3), fg(theme.load_low));
        assert_eq!(fg_at(&buf, "2 ⎣⣿", 0), fg(theme.dim));
        assert_eq!(fg_at(&buf, "100%⎦", 0), fg(theme.dim));
    }

    #[test]
    fn grid_packs_columns_without_an_empty_one() {
        let grid = CoreGrid::new(32, 148);
        assert_eq!((grid.cols, grid.rows), (7, 5));
        assert!(grid.cols * (grid.cell() + CORE_GAP) - CORE_GAP <= 148);
        // more columns fit than there are cores
        assert_eq!(CoreGrid::new(8, 200).cols, 8);
        assert_eq!(CoreGrid::new(8, 200).rows, 1);
        assert_eq!(CoreGrid::new(0, 74).rows, 0);
    }

    #[test]
    fn grid_takes_at_most_half_the_pane_and_counts_the_rest() {
        let cores = vec![0.1; 64];
        let lines = draw_cores_lines((74, 9), cores);
        assert!(lines[1..5].iter().all(|l| l.starts_with(' ')), "{lines:#?}");
        // 4 rows of 3 columns, the last slot counting 64 - 11
        assert!(lines[4].contains("  +53 cores"), "{lines:#?}");
        assert!(lines[5].starts_with("╭ agents: 3 "), "{lines:#?}");
    }

    #[test]
    fn grid_leaves_every_section_its_title() {
        let lines = draw_cores_lines((74, 6), vec![0.1; 16]);
        assert!(lines[3].starts_with("╭ agents: 3 "), "{lines:#?}");
        assert!(lines[4].starts_with("├ orphans: 1 "), "{lines:#?}");
        assert!(lines[5].starts_with("╰ ports: 2/3 "), "{lines:#?}");
        let lines = draw_cores_lines((148, 6), vec![0.1; 64]);
        assert!(lines[4].starts_with("╭ agents: 3 "), "{lines:#?}");
        assert!(lines[5].contains("╯╰ ports: 2/3 "), "{lines:#?}");
    }

    #[test]
    fn tiny_panes_show_no_cores() {
        let lines = draw_cores_lines((74, 5), eight_cores());
        assert_eq!(lines[1].trim_end(), "too small");
    }

    #[test]
    fn no_color_draws_no_colors_in_the_grid() {
        let mut view = View {
            theme: Theme::no_color(),
            ..cores_on()
        };
        let buf = draw_cores_view((74, 16), mockup(), eight_cores(), &mut view);
        for cell in buf.content() {
            assert_eq!((cell.fg, cell.bg), Default::default(), "{cell:?}");
        }
    }
}
