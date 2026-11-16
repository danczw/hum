use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use sysinfo::System;

const TICK: Duration = Duration::from_secs(1);

fn main() -> io::Result<()> {
    let terminal = ratatui::init();
    let result = run(terminal);
    ratatui::restore();
    result
}

fn run(mut terminal: DefaultTerminal) -> io::Result<()> {
    let mut sys = System::new().map_err(io::Error::other)?;
    let mut last_tick = Instant::now() - TICK;

    loop {
        if last_tick.elapsed() >= TICK {
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            last_tick = Instant::now();
        }

        terminal.draw(|frame| draw(frame, &sys))?;

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

fn draw(frame: &mut Frame, sys: &System) {
    let [cpu, mem, swap, _, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let cpu_pct = f64::from(sys.global_cpu_usage()) / 100.0;
    frame.render_widget(
        gauge("cpu", cpu_pct, format!("{:.1}%", cpu_pct * 100.0), Color::Cyan),
        cpu,
    );

    frame.render_widget(
        gauge(
            "mem",
            ratio(sys.used_memory(), sys.total_memory()),
            usage_label(sys.used_memory(), sys.total_memory()),
            Color::Magenta,
        ),
        mem,
    );

    frame.render_widget(
        gauge(
            "swap",
            ratio(sys.used_swap(), sys.total_swap()),
            usage_label(sys.used_swap(), sys.total_swap()),
            Color::Yellow,
        ),
        swap,
    );

    frame.render_widget(
        Paragraph::new(" q quit").style(Style::new().fg(Color::DarkGray)),
        footer,
    );
}

fn gauge(title: &str, ratio: f64, label: String, color: Color) -> Gauge<'static> {
    Gauge::default()
        .block(Block::bordered().title(format!(" {title} ")))
        .gauge_style(Style::new().fg(color))
        .ratio(ratio.clamp(0.0, 1.0))
        .label(label)
}

fn ratio(used: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { used as f64 / total as f64 }
}

fn usage_label(used: u64, total: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    format!("{:.1} / {:.1} GiB", used as f64 / GIB, total as f64 / GIB)
}
