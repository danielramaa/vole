//! The `status` screen: the dashboard, read again every refresh while it is on screen.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, RenderDirection, Sparkline};

use super::battery::State;
use super::cpu::Times;
use super::processes::Process;
use super::reading::{Reader, Reading};
use crate::app::ModuleScreen;
use crate::config::Config;
use crate::term::RunRoot;
use crate::{size, ui};

/// How soon after a reading with no rates (the first, or the first after a pause) the next one
/// is taken, so the rates show up quickly.
const SOON: Duration = Duration::from_millis(250);

/// How many readings the graphs keep: more than a terminal is wide.
const HISTORY: usize = 1024;

/// The columns between the cores' columns, and between panels side by side.
const GAP: u16 = 2;

pub struct Screen {
    /// How long between readings.
    interval: Duration,
    /// The thread taking readings, from the first tick on.
    reader: Option<Reader>,
    /// Whether a reading was asked for and hasn't come yet.
    waiting: bool,
    /// When to ask for the next reading.
    due: Instant,
    /// The latest reading.
    reading: Reading,
    /// The CPU use of the readings so far.
    cpu_graph: History,
    /// Whether the busiest processes are those holding the most memory, not CPU.
    by_memory: bool,
    /// The memory used, as of each reading so far.
    memory_graph: History,
    /// What the network received and sent each second, as of each reading so far.
    received_graph: History,
    sent_graph: History,
    /// Why there are no readings any more, once the thread taking them stopped.
    failed: Option<String>,
}

impl Screen {
    pub fn new(config: &Config) -> Self {
        Self {
            interval: Duration::from_millis(u64::from(config.status_refresh_ms)),
            reader: None,
            waiting: false,
            due: Instant::now(),
            reading: Reading::default(),
            cpu_graph: History::default(),
            by_memory: false,
            memory_graph: History::default(),
            received_graph: History::default(),
            sent_graph: History::default(),
            failed: None,
        }
    }

    /// Shows `reading`, which came `now`.
    fn show(&mut self, reading: Reading, now: Instant) {
        match &reading.cpu {
            Some(cpu) => self
                .cpu_graph
                .push(ui::share(cpu.all.busy, cpu.all.total, 1000)),
            None => self.due = self.due.min(now + SOON),
        }
        if let Some(network) = &reading.network {
            self.received_graph
                .push(network.iter().map(|interface| interface.received).sum());
            self.sent_graph
                .push(network.iter().map(|interface| interface.sent).sum());
        }
        if let Some(memory) = &reading.memory {
            self.memory_graph
                .push(ui::share(memory.used(), memory.total, 1000));
        }
        self.reading = reading;
    }

    /// The CPU panel: the use of all CPUs over time, then of each now, in `cores`.
    fn draw_cpu(&self, frame: &mut Frame, area: Rect, cores: Vec<Line>) {
        let total = self.reading.cpu.as_ref();
        let block = panel("CPU", total.map_or(String::new(), |cpu| percent(cpu.all)));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [graph, grid] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(height(&cores))])
                .areas(inner);
        self.cpu_graph.draw(frame, graph, 1000);
        frame.render_widget(Paragraph::new(cores), grid);
    }

    /// The processes panel: the busiest processes, by CPU or by memory, as many as fit.
    fn draw_processes(&self, frame: &mut Frame, area: Rect) {
        let by = if self.by_memory {
            "by memory"
        } else {
            "by CPU"
        };
        let block = panel("Processes", by.into());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(processes) = &self.reading.processes else {
            return;
        };
        let mut busiest: Vec<&Process> = processes.iter().collect();
        let by_cpu = |a: &&Process, b: &&Process| b.cpu.total_cmp(&a.cpu);
        let by_memory = |a: &&Process, b: &&Process| b.memory.cmp(&a.memory);
        if self.by_memory {
            busiest.sort_by(|a, b| by_memory(a, b).then(by_cpu(a, b)));
        } else {
            busiest.sort_by(|a, b| by_cpu(a, b).then(by_memory(a, b)));
        }
        // The pid, the name, its share of a CPU and the memory it holds, e.g.
        // `   4321  firefox   12.3%    1.2 GiB`.
        let name = usize::from(inner.width).saturating_sub(7 + 2 + 2 + 6 + 2 + 10);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let mut lines = vec![Line::styled(
            format!(
                "{:>7}  {:<name$}  {:>6}  {:>10}",
                "PID", "Name", "CPU", "Memory"
            ),
            dim,
        )];
        lines.extend(
            busiest
                .iter()
                .take(usize::from(inner.height))
                .map(|process| {
                    let shown = ui::fit(&ui::printable(&process.name), name);
                    Line::from(format!(
                        "{:>7}  {shown:<name$}  {:>5.1}%  {:>10}",
                        process.pid,
                        process.cpu,
                        size::format(process.memory)
                    ))
                }),
        );
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// The memory panel: how much memory and swap are used, then the memory used over time.
    fn draw_memory(&self, frame: &mut Frame, area: Rect) {
        let block = panel("Memory", String::new());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut rows = Vec::new();
        if let Some(memory) = &self.reading.memory {
            let used = memory.used();
            let detail = part_of(used, memory.total);
            rows.push(("Used".into(), used, memory.total, detail));
            if memory.swap_total > 0 {
                let swap = memory.swap_used();
                let detail = part_of(swap, memory.swap_total);
                rows.push(("Swap".into(), swap, memory.swap_total, detail));
            }
        }
        let lines = meters(&rows, inner.width);
        let [meters, graph] =
            Layout::vertical([Constraint::Length(height(&lines)), Constraint::Fill(1)])
                .areas(inner);
        frame.render_widget(Paragraph::new(lines), meters);
        self.memory_graph.draw(frame, graph, 1000);
    }

    /// The network panel: what the network receives and sends now, then over time, each graph
    /// up to the most it reached on screen.
    fn draw_network(&self, frame: &mut Frame, area: Rect) {
        let network = self.reading.network.as_deref().unwrap_or_default();
        let names: Vec<&str> = network
            .iter()
            .map(|interface| interface.name.as_str())
            .collect();
        let block = panel("Network", names.join(", "));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [now, graphs] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        if self.reading.network.is_some() {
            let received = network.iter().map(|interface| interface.received).sum();
            let sent = network.iter().map(|interface| interface.sent).sum();
            let text = format!("↓ {}  ↑ {}", rate(received), rate(sent));
            frame.render_widget(Line::from(text), now);
        }
        // What is sent only shows with room for both graphs.
        let [received, sent] = match graphs.height {
            0 | 1 => [graphs, Rect::default()],
            _ => Layout::vertical([Constraint::Fill(1); 2]).areas(graphs),
        };
        for (graph, area, arrow) in [
            (&self.received_graph, received, "↓"),
            (&self.sent_graph, sent, "↑"),
        ] {
            let [label, area] =
                Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
            frame.render_widget(Line::from(arrow), label);
            graph.draw(frame, area, graph.peak(area.width));
        }
    }

    /// The temperatures panel's lines: what each sensor chip measures first, in bold when it is
    /// too hot.
    fn temperature_lines(&self, width: u16) -> Vec<Line<'static>> {
        let temperatures = &self.reading.temperatures;
        let chip = temperatures
            .iter()
            .map(|t| Span::raw(t.chip.as_str()).width());
        let chip = chip.max().unwrap_or(0);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let lines = temperatures.iter().map(|temperature| {
            let label = temperature.label.as_deref().unwrap_or_default();
            let name = ui::printable(&format!("{:<chip$}  {label}", temperature.chip));
            let degrees = format!("{:.0}°C", temperature.celsius);
            let line = ui::right_aligned(vec![Span::raw(name)], degrees, usize::from(width));
            if temperature.is_high() {
                line.style(bold)
            } else {
                line
            }
        });
        lines.collect()
    }

    /// The battery panel's lines: how full each battery is, and how long until it is empty or
    /// full.
    fn battery_lines(&self, width: u16) -> Vec<Line<'static>> {
        let rows: Vec<Meter> = (self.reading.batteries.iter())
            .map(|battery| {
                let left = battery.left.map(hours_and_minutes);
                let state = match (battery.state, left) {
                    (State::Discharging, Some(left)) => format!("{left} left"),
                    (State::Discharging, None) => "on battery".into(),
                    (State::Charging, Some(left)) => format!("{left} to full"),
                    (State::Charging, None) => "charging".into(),
                    (State::Full, _) => "full".into(),
                    (State::NotCharging, _) => "not charging".into(),
                    (State::Unknown, _) => String::new(),
                };
                (battery.name.clone(), battery.percent, 100, state)
            })
            .collect();
        meters(&rows, width)
    }

    /// The disks panel's lines: how full each filesystem on a disk is, then what the disks read
    /// and write.
    fn disk_lines(&self, width: u16) -> Vec<Line<'static>> {
        let reading = &self.reading;
        let rows: Vec<Meter> = reading
            .filesystems
            .iter()
            .map(|filesystem| {
                let path = ui::printable(&filesystem.path.to_string_lossy());
                let free = format!("{} free", size::format(filesystem.free));
                let whole = filesystem.used + filesystem.free;
                (path, filesystem.used, whole, free)
            })
            .collect();
        let mut lines = meters(&rows, width);
        // All disks together: a disk's own would take more than 80 columns.
        if let Some(io) = &reading.disk_io {
            let read = io.iter().map(|disk| disk.read).sum();
            let written = io.iter().map(|disk| disk.written).sum();
            lines.push(Line::from(format!(
                "read {}  write {}",
                rate(read),
                rate(written)
            )));
        }
        lines
    }
}

impl ModuleScreen for Screen {
    /// Picks up the reading asked for, and asks for the next one when it is due.
    fn tick(&mut self) {
        if self.failed.is_some() {
            return;
        }
        let now = Instant::now();
        match self.reader.get_or_insert_with(Reader::start).take() {
            Ok(Some(reading)) => {
                self.waiting = false;
                self.show(reading, now);
            }
            Ok(None) => {}
            Err(_) => {
                self.failed = Some("Reading the system stopped unexpectedly.".into());
                return;
            }
        }
        if !self.waiting
            && now >= self.due
            && let Some(reader) = &self.reader
        {
            reader.ask();
            self.waiting = true;
            // Readings stay `interval` apart on average, though ticks come only now and then.
            self.due += self.interval;
            if self.due < now {
                self.due = now + self.interval;
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('s') => self.by_memory = !self.by_memory,
            _ => return false,
        }
        true
    }

    fn hint(&self) -> &'static str {
        if self.by_memory {
            "s sort by CPU"
        } else {
            "s sort by memory"
        }
    }

    fn can_leave(&self) -> bool {
        true
    }

    fn needs_terminal(&self) -> bool {
        false
    }

    fn run_root(&mut self, _: &mut RunRoot) -> io::Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let every = format!(" every {} ", interval_text(self.interval));
        let block = Block::bordered()
            .title(" status ")
            .title(Line::from(every).right_aligned());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(failed) = &self.failed {
            frame.render_widget(Paragraph::new(failed.as_str()), inner);
            return;
        }
        let [left, right] = Layout::horizontal([Constraint::Fill(1); 2])
            .spacing(GAP)
            .areas(inner);
        let cores = self.reading.cpu.as_ref().map(|cpu| cpu.cores.as_slice());
        let cores = core_lines(cores.unwrap_or_default(), left.width);
        // The graph grows with the screen, slower than the list of processes.
        let graph = (left.height.saturating_sub(21) / 4 + 2).min(10);
        let [cpu, processes] = Layout::vertical([
            Constraint::Length(1 + graph + height(&cores)),
            Constraint::Fill(1),
        ])
        .areas(left);
        self.draw_cpu(frame, cpu, cores);
        self.draw_processes(frame, processes);
        let disks = self.disk_lines(right.width);
        let temperatures = self.temperature_lines(right.width);
        let batteries = self.battery_lines(right.width);
        let [memory, network, disks_area, temperatures_area, battery_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(panel_height(&disks)),
            Constraint::Length(panel_height(&temperatures)),
            Constraint::Length(panel_height(&batteries)),
        ])
        .areas(right);
        self.draw_memory(frame, memory);
        self.draw_network(frame, network);
        draw_lines(frame, disks_area, panel("Disks", String::new()), disks);
        let block = panel("Temperatures", String::new());
        draw_lines(frame, temperatures_area, block, temperatures);
        draw_lines(
            frame,
            battery_area,
            panel("Battery", String::new()),
            batteries,
        );
    }
}

/// A graph's values, oldest first, the last [`HISTORY`] of them.
#[derive(Default)]
struct History(VecDeque<u64>);

impl History {
    fn push(&mut self, value: u64) {
        if self.0.len() == HISTORY {
            self.0.pop_front();
        }
        self.0.push_back(value);
    }

    /// The most the graph shows in `width` columns, at least 1.
    fn peak(&self, width: u16) -> u64 {
        let shown = self.0.iter().rev().take(usize::from(width));
        shown.copied().max().unwrap_or(0).max(1)
    }

    /// Draws the values as a graph from 0 to `max`, the newest at the right edge.
    fn draw(&self, frame: &mut Frame, area: Rect, max: u64) {
        let newest_first: Vec<u64> = self
            .0
            .iter()
            .rev()
            .take(usize::from(area.width))
            .copied()
            .collect();
        let graph = Sparkline::default()
            .data(newest_first)
            .max(max)
            .direction(RenderDirection::RightToLeft);
        frame.render_widget(graph, area);
    }
}

/// A panel's frame: a rule across its top, with `title` at the left and `value` at the right.
fn panel(title: &str, value: String) -> Block<'_> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    // The rule is dim; its titles aren't.
    let bright = Style::new().remove_modifier(Modifier::DIM);
    let mut block = Block::new()
        .borders(Borders::TOP)
        .border_style(dim)
        .title(Line::styled(
            format!("{title} "),
            bright.add_modifier(Modifier::BOLD),
        ));
    if !value.is_empty() {
        block = block.title(Line::styled(format!(" {value}"), bright).right_aligned());
    }
    block
}

/// Draws a panel holding `lines`.
fn draw_lines(frame: &mut Frame, area: Rect, block: Block, lines: Vec<Line>) {
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// How many rows `lines` take.
fn height(lines: &[Line]) -> u16 {
    u16::try_from(lines.len()).unwrap_or(u16::MAX)
}

/// How many rows a panel holding `lines` takes: none when there is nothing to show.
fn panel_height(lines: &[Line]) -> u16 {
    match height(lines) {
        0 => 0,
        rows => rows.saturating_add(1),
    }
}

/// A rate in bytes per second, e.g. `1.2 MiB/s`.
fn rate(bytes: u64) -> String {
    format!("{}/s", size::format(bytes))
}

/// A part of a whole to show as a bar: its label, the part, the whole, and a detail.
type Meter = (String, u64, u64, String);

/// Lines showing parts of wholes as bars, lined up: each row's label, its bar, its percentage
/// and its detail at the right edge of `width`, e.g. `Swap  ██▌        12%  1.0 of 8.0 GiB`.
fn meters(rows: &[Meter], width: u16) -> Vec<Line<'static>> {
    let label = rows.iter().map(|row| Span::raw(row.0.as_str()).width());
    let label = label.max().unwrap_or(0);
    let detail = rows.iter().map(|row| Span::raw(row.3.as_str()).width());
    let detail = detail.max().unwrap_or(0);
    let bar = usize::from(width).saturating_sub(label + 1 + 5 + 2 + detail);
    rows.iter()
        .map(|(name, part, whole, text)| {
            let share = format!("{}%", ui::share(*part, *whole, 100));
            let bar = ui::bar(*part, *whole, bar);
            Line::from(format!("{name:<label$} {bar} {share:>4}  {text:>detail$}"))
        })
        .collect()
}

/// `part` of `whole`, with the unit once when they share it, e.g. `5.1 of 15.2 GiB`.
fn part_of(part: u64, whole: u64) -> String {
    let (part, whole) = (size::format(part), size::format(whole));
    match (part.split_once(' '), whole.split_once(' ')) {
        (Some((number, unit)), Some((_, whole_unit))) if unit == whole_unit => {
            format!("{number} of {whole}")
        }
        _ => format!("{part} of {whole}"),
    }
}

/// The share of `times` that was busy, in percent.
fn percent(times: Times) -> String {
    format!("{}%", ui::share(times.busy, times.total, 100))
}

/// Each core's use as a bar, in as many columns as fit in `width`, numbered down each column.
fn core_lines(cores: &[(u32, Times)], width: u16) -> Vec<Line<'static>> {
    let Some(last) = cores.iter().map(|&(n, _)| n).max() else {
        return Vec::new();
    };
    let label = last.to_string().len();
    // The number, a bar of at least 3 cells and the percentage, e.g. `12 ██▌  45%`.
    let narrowest = label + 1 + 3 + 5;
    let gap = usize::from(GAP);
    let width = usize::from(width);
    let columns = ((width + gap) / (narrowest + gap)).clamp(1, cores.len());
    let rows = cores.len().div_ceil(columns);
    let cell = (width + gap) / columns - gap;
    let bar = cell.saturating_sub(label + 1 + 5).max(1);
    (0..rows)
        .map(|row| {
            let cells: Vec<String> = cores
                .iter()
                .skip(row)
                .step_by(rows)
                .map(|&(n, times)| {
                    let share = ui::bar(times.busy, times.total, bar);
                    format!("{n:>label$} {share} {:>4}", percent(times))
                })
                .collect();
            Line::from(cells.join(&" ".repeat(gap)))
        })
        .collect()
}

/// `duration` in hours and minutes, e.g. `3 h 40 min`.
fn hours_and_minutes(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// `interval` as the title says it, e.g. `250 ms` or `1.5 s`.
fn interval_text(interval: Duration) -> String {
    let ms = interval.as_millis();
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let seconds = format!("{}.{:03}", ms / 1000, ms % 1000);
    format!("{} s", seconds.trim_end_matches('0').trim_end_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::battery::Battery;
    use crate::status::cpu::Cpu;
    use crate::status::disks::{DiskIo, Filesystem};
    use crate::status::memory::Memory;
    use crate::status::network::Traffic;
    use crate::status::sensors::Temperature;

    const GIB: u64 = 1024 * size::MIB;

    fn render(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn times(busy: u64, total: u64) -> Times {
        Times { busy, total }
    }

    /// A screen showing a reading of 4 CPUs.
    fn screen() -> Screen {
        let mut screen = Screen::new(&Config::default());
        let cpu = Cpu {
            all: times(250, 400),
            cores: vec![
                (0, times(100, 100)),
                (1, times(50, 100)),
                (2, times(100, 100)),
                (3, times(0, 100)),
            ],
        };
        let reading = Reading {
            cpu: Some(cpu),
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        screen
    }

    #[test]
    fn shows_the_use_of_each_cpu() {
        let rows = render(&mut screen(), 80, 12).join("\n");
        assert!(rows.contains("every 1 s"), "{rows}");
        assert!(rows.contains("CPU") && rows.contains("62%"), "{rows}");
        assert!(rows.contains("0 █"), "{rows}");
        assert!(rows.contains(" 50%") && rows.contains("100%"), "{rows}");
    }

    #[test]
    fn shows_swap_only_when_there_is_some() {
        let mut screen = screen();
        let memory = Memory {
            total: 16 * GIB,
            available: 4 * GIB,
            swap_total: 0,
            swap_free: 0,
        };
        let reading = Reading {
            memory: Some(memory),
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        let rows = render(&mut screen, 80, 12).join("\n");
        assert!(
            rows.contains("Used") && rows.contains("75%  12.0 of 16.0 GiB"),
            "{rows}"
        );
        assert!(!rows.contains("Swap"), "{rows}");
    }

    #[test]
    fn shows_how_full_the_disks_are_and_what_they_read_and_write() {
        let mut screen = screen();
        let disk = |name: &str, read| DiskIo {
            name: name.into(),
            read,
            written: 0,
        };
        let reading = Reading {
            filesystems: vec![Filesystem {
                path: "/home".into(),
                used: 3 * GIB,
                free: GIB,
            }],
            disk_io: Some(vec![disk("nvme0n1", 1024), disk("sda", 1024)]),
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        let rows = render(&mut screen, 80, 12).join("\n");
        assert!(rows.contains("Disks"), "{rows}");
        assert!(
            rows.contains("/home") && rows.contains("75%  1.0 GiB free"),
            "{rows}"
        );
        assert!(rows.contains("read 2.0 KiB/s  write 0 B/s"), "{rows}");
    }

    #[test]
    fn shows_what_the_network_receives_and_sends() {
        let mut screen = screen();
        let interface = |name: &str| Traffic {
            name: name.into(),
            received: 1024,
            sent: 512,
        };
        let reading = Reading {
            network: Some(vec![interface("enp3s0"), interface("wlan0")]),
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        assert_eq!(screen.received_graph.0.back(), Some(&2048));
        let rows = render(&mut screen, 80, 16).join("\n");
        assert!(
            rows.contains("Network") && rows.contains("enp3s0, wlan0"),
            "{rows}"
        );
        assert!(rows.contains("↓ 2.0 KiB/s  ↑ 1.0 KiB/s"), "{rows}");
    }

    #[test]
    fn shows_temperatures_in_bold_when_too_hot() {
        let mut screen = screen();
        let temperature = |chip: &str, label: Option<&str>, celsius| Temperature {
            chip: chip.into(),
            label: label.map(String::from),
            celsius,
            high: Some(100.0),
        };
        let reading = Reading {
            temperatures: vec![
                temperature("coretemp", Some("Package id 0"), 100.4),
                temperature("nvme", None, 36.85),
            ],
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        let lines = screen.temperature_lines(30);
        assert_eq!(lines[0].to_string(), "coretemp  Package id 0   100°C");
        assert_eq!(lines[1].to_string(), format!("nvme{}37°C", " ".repeat(22)));
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!lines[1].style.add_modifier.contains(Modifier::BOLD));
        // No sensors, no panel.
        screen.show(Reading::default(), Instant::now());
        assert_eq!(panel_height(&screen.temperature_lines(30)), 0);
    }

    #[test]
    fn says_how_long_the_batteries_last() {
        let mut screen = screen();
        let battery = |name: &str, state, left| Battery {
            name: name.into(),
            percent: 56,
            state,
            left,
        };
        let reading = Reading {
            batteries: vec![
                battery(
                    "BAT0",
                    State::Discharging,
                    Some(Duration::from_secs(13_203)),
                ),
                battery("BAT1", State::Charging, None),
                battery("BAT2", State::NotCharging, None),
            ],
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        let lines: Vec<String> = (screen.battery_lines(40).iter())
            .map(|line| line.to_string())
            .collect();
        assert!(lines[0].ends_with("56%  3 h 40 min left"), "{lines:#?}");
        // Details line up at the right.
        assert!(lines[1].ends_with("56%         charging"), "{lines:#?}");
        assert!(lines[2].ends_with("not charging"), "{lines:#?}");
        assert_eq!(hours_and_minutes(Duration::from_secs(59 * 60)), "59 min");
    }

    #[test]
    fn lists_the_busiest_processes_by_cpu_or_by_memory() {
        let mut screen = screen();
        let reading = Reading {
            processes: Some(vec![
                Process::new(1, "systemd", 0.0, 10 * size::MIB),
                Process::new(4321, "firefox", 0.5, GIB),
                Process::new(66, "cc1plus", 98.7, 400 * size::MIB),
            ]),
            ..Reading::default()
        };
        screen.show(reading, Instant::now());
        let names = |screen: &mut Screen| -> Vec<String> {
            let rows = render(screen, 80, 16);
            let at = |name: &str| rows.iter().position(|row| row.contains(name)).unwrap();
            let mut names = ["systemd", "firefox", "cc1plus"].map(String::from).to_vec();
            names.sort_by_key(|name| at(name));
            names
        };
        assert_eq!(names(&mut screen), ["cc1plus", "firefox", "systemd"]);
        let rows = render(&mut screen, 80, 16).join("\n");
        assert!(
            rows.contains("by CPU") && rows.contains("  66  cc1plus"),
            "{rows}"
        );
        assert!(rows.contains("98.7%   400.0 MiB"), "{rows}");
        assert_eq!(screen.hint(), "s sort by memory");
        assert!(screen.on_key(KeyEvent::from(KeyCode::Char('s'))));
        assert_eq!(names(&mut screen), ["firefox", "cc1plus", "systemd"]);
        assert!(render(&mut screen, 80, 16).join("\n").contains("by memory"));
        assert!(!screen.on_key(KeyEvent::from(KeyCode::Char('q'))));
    }

    #[test]
    fn says_how_much_of_how_much() {
        assert_eq!(part_of(1536, 2048), "1.5 of 2.0 KiB");
        assert_eq!(part_of(512, 2048), "512 B of 2.0 KiB");
    }

    #[test]
    fn cores_go_down_each_column() {
        let cores: Vec<(u32, Times)> = (0..16).map(|n| (n, times(n.into(), 100))).collect();
        let lines: Vec<String> = core_lines(&cores, 39)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert_eq!(lines.len(), 6, "{lines:#?}");
        assert!(
            lines[0].starts_with(" 0 ") && lines[0].contains(" 6 "),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| Line::from(line.as_str()).width() <= 39)
        );
        assert!(core_lines(&[], 39).is_empty());
    }

    #[test]
    fn rates_come_soon_after_a_reading_without_them() {
        let mut screen = Screen::new(&Config::default());
        let now = Instant::now();
        screen.due = now + Duration::from_secs(1);
        screen.show(Reading::default(), now);
        assert_eq!(screen.due, now + SOON);
    }

    #[test]
    fn says_how_often_it_reads() {
        assert_eq!(interval_text(Duration::from_millis(250)), "250 ms");
        assert_eq!(interval_text(Duration::from_millis(1000)), "1 s");
        assert_eq!(interval_text(Duration::from_millis(1500)), "1.5 s");
        assert_eq!(interval_text(Duration::from_millis(1250)), "1.25 s");
    }

    #[test]
    fn graphs_keep_the_last_readings() {
        let mut graph = History::default();
        for value in 0..HISTORY as u64 + 10 {
            graph.push(value);
        }
        assert_eq!(graph.0.len(), HISTORY);
        assert_eq!(graph.0.front(), Some(&10));
    }
}
