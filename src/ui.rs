use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Table};
use sysinfo::Pid;

use crate::App;

pub fn draw(frame: &mut Frame, app: &App) {
    let [gauges, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [agents, side] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
    let [ports, orphans] = Layout::vertical([Constraint::Percentage(50); 2]).areas(side);

    let sys = &app.sys;
    let cpu = f64::from(sys.global_cpu_usage()) / 100.0;
    let (mem, swap) = (
        (sys.used_memory(), sys.total_memory()),
        (sys.used_swap(), sys.total_swap()),
    );
    let gauge_data = [
        ("cpu", cpu, format!("{:.1}%", cpu * 100.0), Color::Cyan),
        ("mem", ratio(mem), usage_label(mem), Color::Magenta),
        ("swap", ratio(swap), usage_label(swap), Color::Yellow),
    ];
    let areas: [Rect; 3] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(gauges);
    for ((title, ratio, label, color), area) in gauge_data.into_iter().zip(areas) {
        let gauge = Gauge::default()
            .block(Block::bordered().title(format!(" {title} ")))
            .gauge_style(Style::new().fg(color))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(label);
        frame.render_widget(gauge, area);
    }

    draw_agents(frame, app, agents);
    draw_ports(frame, app, ports);
    draw_orphans(frame, app, orphans);

    frame.render_widget(
        Paragraph::new(" q quit").style(Style::new().fg(Color::DarkGray)),
        footer,
    );
}

fn draw_agents(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .agents
        .iter()
        .flat_map(|agent| {
            let root = &agent.procs[0];
            let head = Row::new([
                agent.kind.to_string(),
                agent.project.clone(),
                root.pid.to_string(),
                fmt_cores(agent.cpu()),
                fmt_bytes(agent.mem()),
            ])
            .style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            let kids = agent.procs[1..].iter().map(|p| {
                Row::new([
                    format!("{}└ {}", "  ".repeat(p.depth - 1), p.name),
                    String::new(),
                    p.pid.to_string(),
                    fmt_cores(p.cpu),
                    fmt_bytes(p.mem),
                ])
            });
            std::iter::once(head).chain(kids)
        })
        .collect();
    let title = format!(" agents ({}) ", app.agents.len());
    let widths = [
        Constraint::Min(16),
        Constraint::Min(8),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
    ];
    render_table(
        frame,
        area,
        &title,
        ["process", "project", "pid", "cores", "mem"],
        rows,
        &widths,
        "no agents running",
    );
}

fn draw_ports(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .ports
        .iter()
        .map(|l| {
            let name = l
                .pid
                .and_then(|pid| app.sys.process(pid))
                .map(crate::agents::proc_name)
                .unwrap_or_default();
            let pid = l.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
            let owner = l.pid.and_then(|p| owner(app, p));
            let style = if owner.is_some() {
                Style::new().fg(Color::Cyan)
            } else {
                Style::new()
            };
            Row::new([format!(":{}", l.port), pid, name, owner.unwrap_or_default()]).style(style)
        })
        .collect();
    let title = format!(" ports ({}) ", app.ports.len());
    let widths = [
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Min(8),
        Constraint::Length(14),
    ];
    render_table(
        frame,
        area,
        &title,
        ["port", "pid", "process", "agent"],
        rows,
        &widths,
        "nothing listening",
    );
}

fn draw_orphans(frame: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<Row> = app
        .orphans
        .iter()
        .map(|o| {
            Row::new([
                o.pid.to_string(),
                o.name.clone(),
                o.agent.to_string(),
                fmt_cores(o.cpu),
                fmt_bytes(o.mem),
            ])
            .style(Style::new().fg(Color::Yellow))
        })
        .collect();
    let title = format!(" orphans ({}) ", app.orphans.len());
    let widths = [
        Constraint::Length(8),
        Constraint::Min(8),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(7),
    ];
    render_table(
        frame,
        area,
        &title,
        ["pid", "process", "from", "cores", "mem"],
        rows,
        &widths,
        "none",
    );
}

/// Which agent a pid belongs to, for tagging ports.
fn owner(app: &App, pid: Pid) -> Option<String> {
    if let Some(agent) = app
        .agents
        .iter()
        .find(|a| a.procs.iter().any(|p| p.pid == pid))
    {
        return Some(agent.kind.to_string());
    }
    let orphan = app.orphans.iter().find(|o| o.pid == pid)?;
    Some(format!("orphan:{}", orphan.agent))
}

fn render_table<const N: usize>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    header: [&str; N],
    rows: Vec<Row>,
    widths: &[Constraint],
    empty: &str,
) {
    let block = Block::bordered().title(title.to_string());
    if rows.is_empty() {
        let text = Line::from(format!(" {empty}")).style(Style::new().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    }
    let header = Row::new(header).style(Style::new().fg(Color::DarkGray));
    frame.render_widget(Table::new(rows, widths).header(header).block(block), area);
}

fn ratio((used, total): (u64, u64)) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64
    }
}

fn usage_label((used, total): (u64, u64)) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    format!("{:.1} / {:.1} GiB", used as f64 / GIB, total as f64 / GIB)
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
