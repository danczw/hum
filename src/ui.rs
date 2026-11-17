use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Table, TableState,
};
use sysinfo::Pid;

use crate::App;
use crate::agents::{self, Agent, Orphan};
use crate::focus::{self, Focus, Key, key};
use crate::theme::Theme;
use crate::trees::{self, Trees};

// Numbers get fixed widths and text columns fill what is left, so a narrow
// pane truncates names, never numbers. Fill ranks below Length in ratatui's
// layout, Min ranks above it.
/// pid_max is at most 4194304 on 64-bit Linux: 7 digits.
const PID: Constraint = Constraint::Length(7);
/// `fmt_cores` up to 99.99 cores.
const CORES_WIDTH: u16 = 5;
const CORES: Constraint = Constraint::Length(CORES_WIDTH);
/// Agents and orphans show cores as the 4th of 5 columns. Panes too narrow
/// for their name columns drop it and its gap.
const CORES_COL: usize = 3;
const CORES_SPAN: u16 = CORES_WIDTH + 1;
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
/// the `process` header. Narrower orphan sections drop cores.
const SIDE_MIN: u16 = 44;
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

/// What the user toggles while hum runs.
pub struct View {
    pub help: bool,
    /// Per-core bars under the gauge line.
    pub cores: bool,
    /// Every listening port, not only those of agents and orphans.
    pub all_ports: bool,
    pub theme: Theme,
    pub focus: Focus,
    pub trees: Trees,
}

/// What a tree key does to the tree under the agents cursor.
pub enum Fold {
    Toggle,
    Expand,
    /// On a child row, moves the cursor to its root and leaves the tree open.
    Collapse,
}

impl View {
    /// Expands or collapses the tree under the agents cursor.
    pub fn fold(&mut self, agents: &[Agent], fold: Fold) {
        if !self.focus.shows(focus::AGENTS) {
            return;
        }
        let Some(at) = self.focus.selected(focus::AGENTS) else {
            return;
        };
        let Some(agent) = trees::tree_of(agents, at) else {
            return;
        };
        let root = trees::root(agent);
        match fold {
            Fold::Collapse if at != root => self.focus.select(focus::AGENTS, root),
            // kept closed, so children spawned later start collapsed
            _ if agent.procs.len() == 1 => {}
            Fold::Toggle => self.trees.toggle(root),
            Fold::Expand => self.trees.expand(root),
            Fold::Collapse => self.trees.collapse(root),
        }
    }

    /// Forgets exited sessions and moves a cursor on a hidden child to its
    /// root, whether a key collapsed the tree or its root changed.
    fn sync(&mut self, agents: &[Agent]) {
        self.trees.retain(agents);
        if let Some(agent) = self
            .focus
            .selected(focus::AGENTS)
            .and_then(|at| trees::tree_of(agents, at))
        {
            let root = trees::root(agent);
            if !self.trees.is_open(root) {
                self.focus.select(focus::AGENTS, root);
            }
        }
    }
}

/// Everything one frame shows, read out of [`App`] so drawing needs no
/// `System`.
struct Screen<'a> {
    /// 0.0 to 1.0.
    cpu: f64,
    /// Each core's use, 0.0 to 1.0.
    cores: Vec<f64>,
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
    /// Bound beyond loopback; marked `*`.
    public: bool,
    name: String,
    /// The session or orphan holding the port.
    owner: Option<String>,
}

/// A titled list of rows under a dim header naming the columns.
struct Section {
    name: &'static str,
    /// The section's cursor in [`Focus`].
    pane: usize,
    count: usize,
    /// Rows a filter left out, as `ports: 2/7`; `None` when none were.
    total: Option<usize>,
    /// Draws the count in the title; `None` keeps it dim like the rest.
    count_style: Option<Style>,
    header: Row<'static>,
    rows: Vec<Row<'static>>,
    /// One per row.
    keys: Vec<Key>,
    widths: Vec<Constraint>,
    /// The rows as short entries for a title line; only ports fold into one.
    inline: Vec<Line<'static>>,
}

pub fn draw(frame: &mut Frame, app: &App, view: &mut View) {
    render(frame, &screen(app, view.cores), view);
}

/// Reads each core's use only when `cores`, so the grid costs nothing while
/// it is off.
fn screen(app: &App, cores: bool) -> Screen<'_> {
    let sys = &app.sys;
    let ports = app
        .ports
        .iter()
        .map(|l| PortRow {
            port: l.port,
            pid: l.pid,
            public: l.public,
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
        cores: if cores {
            sys.cpus()
                .iter()
                .map(|c| f64::from(c.usage()) / 100.0)
                .collect()
        } else {
            Vec::new()
        },
        mem: (sys.used_memory(), sys.total_memory()),
        swap: (sys.used_swap(), sys.total_swap()),
        agents: &app.agents,
        orphans: &app.orphans,
        ports,
    }
}

fn render(frame: &mut Frame, screen: &Screen, view: &mut View) {
    view.sync(screen.agents);
    let (theme, trees) = (&view.theme, &view.trees);
    let focus = &mut view.focus;
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let [gauges, hint] = Layout::vertical([Constraint::Length(1); 2]).areas(area);
        draw_gauges(frame, screen, gauges, theme);
        frame.render_widget(Line::styled("too small", theme.dim), hint);
        return;
    }
    let grid = view
        .cores
        .then(|| CoreGrid::new(screen.cores.len(), area.width));
    // the rows compact sections need for their titles: three stacked, two
    // side by side
    let titles = if area.width >= WIDE { 2 } else { 3 };
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
    draw_gauges(frame, screen, gauges, theme);
    if let Some(grid) = grid {
        draw_cores(frame, &screen.cores, grid, cores, theme);
    }

    let ports = ports_section(&screen.ports, view.all_ports, theme);
    if body.width >= WIDE {
        let side = (body.width * 2 / 5).max(SIDE_MIN);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(side)]).areas(body);
        let agents = agents_section(screen.agents, trees, left.width, theme);
        let orphans = orphans_section(screen.orphans, right.width, theme);
        stack(frame, [agents], left, compact, theme, focus);
        stack(frame, [orphans, ports], right, compact, theme, focus);
    } else {
        let agents = agents_section(screen.agents, trees, body.width, theme);
        let orphans = orphans_section(screen.orphans, body.width, theme);
        stack(frame, [agents, orphans, ports], body, compact, theme, focus);
    }

    if view.help {
        draw_help(frame);
    }
    focus.settle();
}

/// `cpu ⎣⣿⣿⡆⠀  34%⎦   mem ⎣⣿⣿⣿⠀ 12.1/31G⎦   swp ⎣⡀⠀⠀⠀ 0.0/8G⎦` on one
/// line, bracketed as in htop. The bars share one length so they compare at
/// a glance, and stretch to fill the line. The unused part stays blank, no
/// track. Each bar takes one color from its load.
fn draw_gauges(frame: &mut Frame, screen: &Screen, area: Rect, theme: &Theme) {
    let gauges = [
        (
            "cpu",
            screen.cpu,
            // as wide as `100` so the bars do not jitter
            format!("{:>3.0}", screen.cpu * 100.0),
            "%",
        ),
        ("mem", ratio(screen.mem), usage_label(screen.mem), "G"),
        ("swp", ratio(screen.swap), usage_label(screen.swap), "G"),
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
        // index, ` ⎣`, ` 77%⎦`
        let fixed = index + 2 + 6;
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

    /// One core's cell: index, space, bar and percent in brackets.
    fn cell(&self) -> u16 {
        self.index + self.bar + 8
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
fn agents_section(agents: &[Agent], trees: &Trees, width: u16, theme: &Theme) -> Section {
    let cores = width >= AGENTS_MIN;
    let names = (width + if cores { 0 } else { CORES_SPAN }).saturating_sub(AGENTS_FIXED);
    let project =
        names.saturating_sub(PROCESS).min(PROJECT) + names.saturating_sub(PROCESS + PROJECT) / 4;
    let process = names - project;
    let (process_max, project_max) = (usize::from(process), usize::from(project));
    let mut rows = Vec::new();
    let mut keys = Vec::new();
    for agent in agents {
        let root = &agent.procs[0];
        let kids = &agent.procs[1..];
        let root_key = trees::root(agent);
        let open = trees.is_open(root_key);
        // `▸ ● claude (14)` collapsed, `▾ ● claude` open; a long kind is cut
        // before the count
        let (marker, count) = match kids.len() {
            0 => (' ', String::new()),
            _ if open => ('▾', String::new()),
            n => ('▸', format!(" ({n})")),
        };
        let (status, status_style) = match agent.busy {
            true => ('●', theme.accent),
            false => ('○', theme.dim),
        };
        let kind = cut(agent.kind, process_max.saturating_sub(4 + count.len()));
        let name = cut(&format!("{marker} {status} {kind}{count}"), process_max);
        let mut chars = name.chars();
        let fold: String = chars.by_ref().take(2).collect();
        let status: String = chars.by_ref().take(1).collect();
        let name = Line::from(vec![
            Span::raw(fold),
            Span::styled(status, status_style),
            Span::raw(chars.collect::<String>()),
        ]);
        rows.push(
            Row::new(columns(
                [
                    Cell::from(name),
                    Cell::from(cut(&agent.project, project_max)),
                    num(root.pid.to_string()),
                    num(fmt_cores(agent.cpu())),
                    num(fmt_bytes(agent.mem())),
                ],
                cores,
            ))
            .style(theme.accent),
        );
        keys.push(root_key);
        if !open {
            continue;
        }
        for p in kids {
            rows.push(Row::new(columns(
                [
                    Cell::from(cut(
                        &format!("{}└ {}", "  ".repeat(p.depth - 1), p.name),
                        process_max,
                    )),
                    // Cell::default() spans zero columns and would shift the rest.
                    Cell::from(""),
                    num(p.pid.to_string()),
                    num(fmt_cores(p.cpu)),
                    num(fmt_bytes(p.mem)),
                ],
                cores,
            )));
            keys.push(key(p.pid, p.start));
        }
    }
    Section {
        name: "agents",
        pane: focus::AGENTS,
        count: agents.len(),
        total: None,
        count_style: None,
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
        keys,
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
        inline: Vec::new(),
    }
}

/// Orphan rows warn, except `from`: agent tags keep the default color.
/// `width` is the whole box.
fn orphans_section(orphans: &[Orphan], width: u16, theme: &Theme) -> Section {
    let cores = width >= SIDE_MIN;
    let rows = orphans
        .iter()
        .map(|o| {
            Row::new(columns(
                [
                    num(o.pid.to_string()),
                    Cell::from(cut(&o.name, ORPHAN_NAME)),
                    Cell::from(o.from.clone()).style(Style::reset()),
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
        pane: focus::ORPHANS,
        count: orphans.len(),
        total: None,
        count_style: (!orphans.is_empty()).then_some(theme.warn),
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
        keys: orphans.iter().map(|o| key(o.pid, o.start)).collect(),
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
        inline: Vec::new(),
    }
}

/// The columns of an agents or orphans row, without cores unless `cores`.
fn columns<T>(columns: [T; 5], cores: bool) -> Vec<T> {
    let mut columns = Vec::from(columns);
    if !cores {
        columns.remove(CORES_COL);
    }
    columns
}

/// Only ports of agents and orphans unless `all`. A port bound beyond
/// loopback gets a `*`, which warns when an agent or orphan owns it.
fn ports_section(ports: &[PortRow], all: bool, theme: &Theme) -> Section {
    let shown: Vec<&PortRow> = ports.iter().filter(|p| all || p.owner.is_some()).collect();
    // `:3000*`
    let port = |p: &PortRow| {
        let star = match (p.public, p.owner.is_some()) {
            (false, _) => None,
            (true, true) => Some(Span::styled("*", theme.warn)),
            (true, false) => Some(Span::raw("*")),
        };
        let mut spans = vec![Span::raw(format!(":{}", p.port))];
        spans.extend(star);
        spans
    };
    let rows = shown
        .iter()
        .map(|p| {
            Row::new([
                Cell::from(Line::from(port(p))),
                num(p.pid.map_or_else(|| "-".into(), |p| p.to_string())),
                Cell::from(p.name.clone()),
                Cell::from(p.owner.clone().unwrap_or_default()),
            ])
        })
        .collect();
    Section {
        name: "ports",
        pane: focus::PORTS,
        count: shown.len(),
        total: (shown.len() < ports.len()).then_some(ports.len()),
        count_style: None,
        header: Row::new([
            Cell::from("port"),
            num("pid".into()),
            Cell::from("process"),
            Cell::from("agent"),
        ]),
        rows,
        keys: shown
            .iter()
            .map(|p| (u64::from(p.port), p.pid.map_or(0, |p| p.as_u32().into())))
            .collect(),
        widths: vec![
            // `:65535*`
            Constraint::Length(7),
            PID,
            Constraint::Fill(1),
            Constraint::Fill(2),
        ],
        // `:3000* node claude/hum`, or `:5432 postgres` without an owner
        inline: shown
            .iter()
            .map(|p| {
                let mut spans = port(p);
                spans.extend(
                    std::iter::once(&p.name)
                        .chain(&p.owner)
                        .filter(|s| !s.is_empty())
                        .map(|s| Span::raw(format!(" {s}"))),
                );
                Line::from(spans)
            })
            .collect(),
    }
}

/// Stacks sections top to bottom. Every section keeps its title. The
/// focused section first gets a line for its cursor, then data rows go to
/// the earlier sections first. Rows nobody needs are shared out so the
/// boxes fill the area.
///
/// Each section is a box, and a header shows over at least one row or in
/// an empty section. `compact`, the sections share one box split by `├ name ─┤`
/// lines, drop their headers, and ports fold into the bottom border.
fn stack<const N: usize>(
    frame: &mut Frame,
    sections: [Section; N],
    area: Rect,
    compact: bool,
    theme: &Theme,
    focus: &mut Focus,
) {
    let footer = compact && sections[N - 1].pane == focus::PORTS;
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
    let rows: [usize; N] = std::array::from_fn(|i| {
        if footer && i == N - 1 {
            0
        } else {
            sections[i].rows.len()
        }
    });
    // header and data lines; a section given one line shows a row without
    // its header, see `draw_section`
    let mut lines = [0; N];
    for ((l, section), &n) in lines.iter_mut().zip(&sections).zip(&rows) {
        if focus.shows(section.pane) && n > 0 {
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
    for (i, (section, area)) in sections.into_iter().zip(areas).enumerate() {
        let block = if !compact {
            Block::bordered().border_type(BorderType::Rounded)
        } else if footer && i == N - 1 {
            draw_footer(frame, section, area, theme, focus);
            continue;
        } else {
            let bottom = if i == N - 1 {
                Borders::BOTTOM
            } else {
                Borders::NONE
            };
            Block::new()
                .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT | bottom)
                .border_set(if i == 0 { border::ROUNDED } else { DIVIDER })
        };
        draw_section(frame, section, area, block, !compact, theme, focus);
    }
}

/// `block` with the title and count in its top border and a dim frame. The
/// rows scroll to keep the cursor in view, and a last `+N more` line counts
/// the rows below. The focused section's name takes the accent.
fn draw_section(
    frame: &mut Frame,
    section: Section,
    area: Rect,
    block: Block,
    header: bool,
    theme: &Theme,
    focus: &mut Focus,
) {
    let shows = focus.shows(section.pane);
    let mut title = title(&section, shows, theme);
    title.push(Span::styled(" ", theme.dim));
    let block = block
        .border_style(theme.dim)
        .padding(Padding::horizontal(1))
        .title(Line::from(title));
    let inner = block.inner(area);
    // a single line goes to a row, not to a header with nothing under it
    let header = header && (inner.height >= 2 || section.keys.is_empty());
    let lines = inner.height.saturating_sub(u16::from(header));
    let window = focus
        .pane(section.pane)
        .frame(section.keys, usize::from(lines));
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
/// `╰ ports: 2 · :3000 node claude/hum  :5432 postgres ──╯`. Entries that do
/// not fit are counted as `+N`. `Tab` passes over the section.
fn draw_footer(frame: &mut Frame, section: Section, area: Rect, theme: &Theme, focus: &mut Focus) {
    focus.pane(section.pane).hide();
    let mut spans = title(&section, false, theme);
    // corners, the closing space and at least one rule
    let mut room = usize::from(area.width).saturating_sub(Line::from(spans.clone()).width() + 4);
    let entries = &section.inline;
    let mut shown = 0;
    for (i, entry) in entries.iter().enumerate() {
        let sep = if i == 0 { " · " } else { "  " };
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
        let sep = if shown == 0 { " · " } else { "  " };
        spans.push(Span::styled(sep, theme.dim));
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
            section.count_style.unwrap_or(theme.dim),
        ),
    ]
}

fn draw_help(frame: &mut Frame) {
    let lines = [
        "j k ↑ ↓  move the cursor",
        "g G      first, last row",
        "PgUp/Dn  move a page",
        "Tab      next section",
        "S-Tab    previous section",
        "Enter ␣  expand or collapse agent tree",
        "l →      expand agent tree",
        "h ←      collapse, or go to the root",
        "e c      expand, collapse all trees",
        "1        per-core cpu",
        "a        all ports, or agent ports only",
        "Esc      close help, hide cursor, else quit",
        "q        quit",
        "?        toggle this help",
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
    use ratatui::style::{Color, Modifier};

    use crate::agents::{self, BusyTracker, ProcInfo};
    use crate::focus::Focus;

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
        let buf = draw((width, height), procs, Theme::ayu_mirage());
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// Draws with every tree expanded.
    fn draw((width, height): (u16, u16), procs: Vec<(Pid, ProcInfo)>, theme: Theme) -> Buffer {
        let mut view = View { theme, ..view() };
        view.trees.expand_all(&scan(&procs));
        draw_view((width, height), procs, &mut view)
    }

    fn scan(procs: &[(Pid, ProcInfo)]) -> Vec<Agent> {
        agents::scan(&procs.iter().cloned().collect(), None)
    }

    /// Draws with `view`, so its cursor carries over to the next frame.
    fn draw_view(size: (u16, u16), procs: Vec<(Pid, ProcInfo)>, view: &mut View) -> Buffer {
        draw_cores_view(size, procs, eight_cores(), view)
    }

    /// One core pegged, one at the mid load, the rest low.
    fn eight_cores() -> Vec<f64> {
        vec![0.21, 0.06, 1.0, 0.17, 0.0, 0.7, 0.3, 0.12]
    }

    fn draw_cores_view(
        (width, height): (u16, u16),
        procs: Vec<(Pid, ProcInfo)>,
        cores: Vec<f64>,
        view: &mut View,
    ) -> Buffer {
        let procs: HashMap<Pid, ProcInfo> = procs.into_iter().collect();
        draw_agents((width, height), &agents::scan(&procs, None), cores, view)
    }

    fn draw_agents(
        (width, height): (u16, u16),
        agents: &[Agent],
        cores: Vec<f64>,
        view: &mut View,
    ) -> Buffer {
        let orphans = [Orphan {
            pid: Pid::from_u32(9120),
            name: "node".into(),
            from: "claude/hum".into(),
            start: 0,
            cpu: 0.0,
            mem: 48 << 20,
        }];
        let screen = Screen {
            cpu: 0.34,
            cores,
            mem: (GIB * 121 / 10, 31 * GIB),
            swap: (GIB / 1000, 8 * GIB),
            agents,
            orphans: &orphans,
            ports: vec![
                PortRow {
                    port: 3000,
                    pid: Some(Pid::from_u32(9120)),
                    public: true,
                    name: "node".into(),
                    owner: Some("claude/hum".into()),
                },
                PortRow {
                    port: 5432,
                    pid: None,
                    public: false,
                    name: "postgres".into(),
                    owner: None,
                },
                PortRow {
                    port: 8000,
                    pid: Some(Pid::from_u32(5871)),
                    public: false,
                    name: "python".into(),
                    owner: Some("claude/infra".into()),
                },
            ],
        };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, &screen, view)).unwrap();
        terminal.backend().buffer().clone()
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
        style.fg.unwrap_or_default()
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
        let mut agents = agents::scan(&procs, None);
        let mut busy = BusyTracker::default();
        for _ in 0..10 {
            busy.update(&mut agents);
        }
        let mut view = View { theme, ..view() };
        let buf = draw_agents((74, 16), &agents, eight_cores(), &mut view);
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
    fn inline_ports_take_no_cursor() {
        let mut view = view();
        draw_view((74, 9), mockup(), &mut view);
        view.focus.cycle(1);
        view.focus.cycle(1);
        // orphans, then back to agents past the folded ports
        view.focus.cycle(1);
        let buf = draw_view((74, 9), mockup(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains("4123"));
    }

    #[test]
    fn folded_ports_keep_their_cursor() {
        let mut view = view();
        draw_view((74, 16), mockup(), &mut view);
        view.focus.cycle(1);
        view.focus.cycle(-1);
        view.focus.step(1);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains(":8000"));
        // folded, focus moves on to agents
        draw_view((74, 9), mockup(), &mut view);
        draw_view((74, 16), mockup(), &mut view);
        view.focus.cycle(-1);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains(":8000"));
    }

    #[test]
    fn a_shows_ports_without_an_agent() {
        let mut view = View {
            all_ports: true,
            ..view()
        };
        let buf = draw_view((74, 20), mockup(), &mut view);
        let lines = text(&buf);
        assert!(
            lines.iter().any(|l| l.starts_with("╭ ports: 3 ")),
            "{lines:#?}"
        );
        let postgres = lines.iter().find(|l| l.contains("postgres")).unwrap();
        assert!(postgres.starts_with("│ :5432 "), "{lines:#?}");
        // a public port without an agent gets a plain `*`
        let ports = [PortRow {
            port: 22,
            pid: None,
            public: true,
            name: "sshd".into(),
            owner: None,
        }];
        let theme = Theme::ayu_mirage();
        let section = ports_section(&ports, true, &theme);
        assert_eq!(section.inline[0].spans[1].content, "*");
        assert_eq!(section.inline[0].spans[1].style, Style::default());
    }

    #[test]
    fn filter_hiding_nothing_counts_plainly() {
        let ports = [PortRow {
            port: 3000,
            pid: None,
            public: true,
            name: "node".into(),
            owner: Some("claude/hum".into()),
        }];
        let section = ports_section(&ports, false, &Theme::ayu_mirage());
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
        view.trees.expand_all(&scan(&big_tree(29)));
        draw_view((74, 9), big_tree(29), &mut view);
        view.focus.step(1);
        for _ in 0..29 {
            draw_view((74, 9), big_tree(29), &mut view);
            view.focus.step(1);
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
        view.trees.expand_all(&scan(&big_tree(29)));
        draw_view((74, 9), big_tree(29), &mut view);
        view.focus.cycle(1);
        view.focus.cycle(1);
        let buf = draw_view((74, 9), big_tree(29), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("9120"));
    }

    #[test]
    fn short_panes_show_a_row_without_its_header() {
        let theme = Theme::ayu_mirage();
        for height in [8, 10] {
            let mut view = View { theme, ..view() };
            view.trees.expand_all(&scan(&big_tree(29)));
            draw_view((74, height), big_tree(29), &mut view);
            view.focus.cycle(1);
            view.focus.cycle(1);
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
    fn cursor_stays_on_its_process_when_rows_reorder() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        draw_view((74, 16), mockup(), &mut view);
        view.focus.step(1);
        view.focus.step(1);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("5871"));
        // a new session sorts first and pushes 5871 down a row
        let mut procs = mockup();
        procs.push(root_proc(1000, "codex", "/x/new", 1 << 20));
        let buf = draw_view((74, 16), procs, &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("5871"));
    }

    #[test]
    fn tab_moves_the_cursor_to_orphans_and_leaves_agents() {
        let theme = Theme::ayu_mirage();
        let mut view = View { theme, ..view() };
        draw_view((74, 16), mockup(), &mut view);
        // the first Tab shows the cursor in agents
        view.focus.cycle(1);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("4123"));
        view.focus.cycle(1);
        view.focus.step(1);
        let buf = draw_view((74, 16), mockup(), &mut view);
        assert!(selected_line(&buf, &theme).unwrap().contains("9120"));
        assert_eq!(fg_at(&buf, "orphans: 1", 1), fg(theme.accent));
        assert_eq!(fg_at(&buf, "agents: 3", 0), fg(theme.dim));
        view.focus.cycle(-1);
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

    fn view() -> View {
        View {
            help: false,
            cores: false,
            all_ports: false,
            theme: Theme::ayu_mirage(),
            focus: Focus::default(),
            trees: Trees::default(),
        }
    }

    fn shows(buf: &Buffer, pid: &str) -> bool {
        text(buf).iter().any(|l| l.contains(pid))
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
    fn expanded_trees_stay_expanded_across_ticks() {
        let mut view = view();
        let agents = scan(&sessions());
        draw_view((74, 16), sessions(), &mut view);
        view.focus.step(1);
        view.fold(&agents, Fold::Toggle);
        for _ in 0..3 {
            draw_view((74, 16), sessions(), &mut view);
        }
        view.focus.last();
        draw_view((74, 16), sessions(), &mut view);
        view.focus.step(-1);
        draw_view((74, 16), sessions(), &mut view);
        view.fold(&agents, Fold::Expand);
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(
            text(&buf)[4].starts_with("│ ▾ ○ claude "),
            "{:#?}",
            text(&buf)
        );
        assert!(shows(&buf, "4125") && shows(&buf, "5872"));
    }

    #[test]
    fn collapse_then_expand_all_restores_every_tree() {
        let mut view = view();
        let agents = scan(&sessions());
        view.trees.expand_all(&agents);
        view.trees.collapse_all();
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(!shows(&buf, "4124") && !shows(&buf, "5872"));
        view.trees.expand_all(&agents);
        let buf = draw_view((74, 16), sessions(), &mut view);
        for pid in ["4124", "4125", "5872"] {
            assert!(shows(&buf, pid), "{pid}");
        }
    }

    #[test]
    fn cursor_on_a_collapsed_child_moves_to_its_root() {
        let mut view = view();
        let agents = scan(&sessions());
        view.trees.expand_all(&agents);
        draw_view((74, 16), sessions(), &mut view);
        view.focus.step(1);
        view.focus.step(2);
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains("4125"));
        // ← on a child goes to the root and leaves the tree open
        view.fold(&agents, Fold::Collapse);
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains("4123"));
        assert!(shows(&buf, "4125"));
        // Enter on a child collapses its tree
        view.focus.step(2);
        draw_view((74, 16), sessions(), &mut view);
        view.fold(&agents, Fold::Toggle);
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains("4123"));
        assert!(!shows(&buf, "4125"));
        // so does collapsing every tree
        view.trees.expand_all(&agents);
        view.focus.last();
        draw_view((74, 16), sessions(), &mut view);
        view.focus.step(-1);
        draw_view((74, 16), sessions(), &mut view);
        view.trees.collapse_all();
        let buf = draw_view((74, 16), sessions(), &mut view);
        assert!(selected_line(&buf, &view.theme).unwrap().contains("5871"));
    }

    #[test]
    fn no_color_reverses_the_selected_row() {
        let mut view = View {
            theme: Theme::no_color(),
            ..view()
        };
        draw_view((74, 16), mockup(), &mut view);
        view.focus.step(1);
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
        View {
            cores: true,
            ..view()
        }
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
