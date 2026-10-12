//! The `status` screen: the dashboard, read again every refresh while it is on screen.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, RenderDirection, Sparkline};

use super::cpu::Times;
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
    /// The memory used, as of each reading so far.
    memory_graph: History,
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
            memory_graph: History::default(),
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
        if let Some(memory) = &reading.memory {
            self.memory_graph
                .push(ui::share(memory.used(), memory.total, 1000));
        }
        self.reading = reading;
    }

    /// The CPU panel: the use of all CPUs over time, then of each now.
    fn draw_cpu(&self, frame: &mut Frame, area: Rect) {
        let cpu = self.reading.cpu.as_ref();
        let total = cpu.map_or(String::new(), |cpu| percent(cpu.all));
        let block = panel("CPU", total);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let cores = cpu.map_or(&[][..], |cpu| &cpu.cores);
        let lines = core_lines(cores, inner.width);
        let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let [graph, grid] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(rows)]).areas(inner);
        self.cpu_graph.draw(frame, graph, 1000);
        frame.render_widget(Paragraph::new(lines), grid);
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

    fn on_key(&mut self, _key: KeyEvent) -> bool {
        false
    }

    fn hint(&self) -> &'static str {
        ""
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
        self.draw_cpu(frame, left);
        let disks = self.disk_lines(right.width);
        let [memory, disks_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(height(&disks) + 1)])
                .areas(right);
        self.draw_memory(frame, memory);
        draw_lines(frame, disks_area, panel("Disks", String::new()), disks);
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
    use crate::status::cpu::Cpu;
    use crate::status::disks::{DiskIo, Filesystem};
    use crate::status::memory::Memory;

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
