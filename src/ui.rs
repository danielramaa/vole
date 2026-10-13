//! Drawing helpers shared by the module screens.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::log::{MovedTo, Outcome};
use crate::size;

/// `text` with control characters, which would break a line, shown as `?`: file names may hold
/// them.
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// `spans` on the left, `right` at the right edge of `width`.
pub fn right_aligned(mut spans: Vec<Span<'static>>, right: String, width: usize) -> Line<'static> {
    // Terminal columns, not chars: wide characters take two.
    let right = Span::raw(right);
    let used: usize = spans.iter().map(Span::width).sum();
    let gap = width.saturating_sub(used + right.width()).max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(right);
    Line::from(spans)
}

/// `part`'s share of `whole`, out of `scale`.
pub fn share(part: u64, whole: u64, scale: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let share = u128::from(part) * u128::from(scale) / u128::from(whole);
    u64::try_from(share).unwrap_or(u64::MAX).min(scale)
}

/// `part`'s share of `whole` as a bar `cells` wide, drawn in eighths of a cell.
pub fn bar(part: u64, whole: u64, cells: usize) -> String {
    const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = share(part, whole, cells as u64 * 8) as usize;
    let mut bar = "█".repeat(eighths / 8);
    bar.push_str(EIGHTHS[eighths % 8]);
    bar.push_str(&" ".repeat(cells - eighths.div_ceil(8)));
    bar
}

/// `text` cut to `width` columns, ending with … where it was cut.
pub fn fit(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_string();
    }
    let mut fitted = String::new();
    // The … takes one column.
    let mut used = 1;
    for c in text.chars() {
        let columns = Span::raw(c.to_string()).width();
        if used + columns > width {
            break;
        }
        used += columns;
        fitted.push(c);
    }
    fitted.push('…');
    fitted
}

/// `text` cut at the start to `width` columns, so its end stays.
pub fn fit_start(text: &str, width: usize) -> String {
    let reversed: String = text.chars().rev().collect();
    fit(&reversed, width).chars().rev().collect()
}

/// The scroll offset after `key`: j/k and the arrows move one line.
pub fn scrolled(scroll: u16, key: KeyEvent) -> u16 {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
        _ => scroll,
    }
}

/// Draws `lines` scrolled by `scroll`, which is first kept within the lines that don't fit.
pub fn draw_scrolled(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    lines: Vec<Line>,
    scroll: &mut u16,
) {
    let height = usize::from(block.inner(area).height);
    let most = u16::try_from(lines.len().saturating_sub(height)).unwrap_or(u16::MAX);
    *scroll = (*scroll).min(most);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((*scroll, 0)),
        area,
    );
}

/// The result screen: what was freed, and what failed and why.
pub fn draw_outcomes(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    outcomes: &[Outcome],
    scroll: &mut u16,
) {
    let lines = outcome_lines(outcomes, usize::from(block.inner(area).width));
    draw_scrolled(frame, area, block, lines, scroll);
}

/// The lines of the result screen, `width` wide.
pub fn outcome_lines(outcomes: &[Outcome], width: usize) -> Vec<Line<'static>> {
    let done = |moved: Option<MovedTo>| -> u64 {
        outcomes
            .iter()
            .filter(|o| o.error.is_none() && o.moved == moved)
            .map(|o| o.size)
            .sum()
    };
    let freed = done(None);
    let (backup, trash) = (done(Some(MovedTo::Backup)), done(Some(MovedTo::Trash)));
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    let mut parts = Vec::new();
    if freed > 0 || backup == 0 && trash == 0 {
        parts.push(format!("freed about {}", size::format(freed)));
    }
    if backup > 0 {
        parts.push(format!("moved {} into a Backup", size::format(backup)));
    }
    if trash > 0 {
        parts.push(format!("moved {} to the trash", size::format(trash)));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let mut summary = parts.join(" · ");
    summary[..1].make_ascii_uppercase();
    let mut lines = vec![
        Line::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
        Line::default(),
    ];
    let dim = Style::new().add_modifier(Modifier::DIM);
    for outcome in outcomes {
        match &outcome.error {
            None => {
                let to = match outcome.moved {
                    Some(MovedTo::Backup) => " → Backup",
                    Some(MovedTo::Trash) => " → trash",
                    None => "",
                };
                let label = printable(&outcome.label);
                lines.push(right_aligned(
                    vec![Span::raw(format!("  ✓ {label}{to}"))],
                    size::format(outcome.size),
                    width,
                ))
            }
            Some(err) => {
                lines.push(Line::from(format!("  ✗ {}", printable(&outcome.label))));
                lines.push(Line::styled(format!("      {}", printable(err)), dim));
            }
        }
        if let Some(err) = &outcome.log_error {
            lines.push(Line::styled(format!("      ! {err}"), dim));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(label: &str, size: u64, error: Option<&str>, moved: Option<MovedTo>) -> Outcome {
        Outcome {
            label: label.into(),
            size,
            error: error.map(String::from),
            log_error: None,
            moved,
        }
    }

    /// The result screen as text.
    fn render(outcomes: &[Outcome]) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_outcomes(frame, frame.area(), Block::bordered(), outcomes, &mut 0);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..8)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn results_tell_freed_from_moved() {
        let backup = Some(MovedTo::Backup);
        let text = render(&[
            outcome("pkg", 2048, None, None),
            outcome("~/.config/gone", 1024, None, backup),
            outcome("~/.config/stuck", 1, Some("refused"), backup),
        ]);
        assert!(
            text.contains("Freed about 2.0 KiB · moved 1.0 KiB into a Backup · 1 failed"),
            "{text}"
        );
        assert!(text.contains("✓ ~/.config/gone → Backup"));
        let text = render(&[outcome("~/.config/gone", 1024, None, backup)]);
        assert!(text.contains("Moved 1.0 KiB into a Backup"), "{text}");
        assert!(render(&[]).contains("Freed about 0 B"));
        let text = render(&[outcome("~/a.iso", 2048, None, Some(MovedTo::Trash))]);
        assert!(text.contains("Moved 2.0 KiB to the trash"), "{text}");
        assert!(text.contains("✓ ~/a.iso → trash"));
    }

    #[test]
    fn odd_names_stay_on_one_line() {
        let text = render(&[outcome("~/a\nb", 1, Some("~/a\nb: gone"), None)]);
        assert!(text.contains("✗ ~/a?b"), "{text}");
        assert!(text.contains("~/a?b: gone"), "{text}");
    }

    #[test]
    fn cuts_text_by_columns() {
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本語", 4), "日…");
        assert_eq!(fit_start("/home/me/Videos", 8), "…/Videos");
    }

    #[test]
    fn bars_show_a_share_in_eighths_of_a_cell() {
        assert_eq!(bar(1, 1, 10), "█".repeat(10));
        assert_eq!(bar(0, 0, 10), " ".repeat(10));
        assert_eq!(bar(13, 20, 10), "██████▌   ");
        assert_eq!(share(1, 3, 100), 33);
    }

    #[test]
    fn sizes_line_up_with_wide_characters() {
        let line = right_aligned(vec![Span::raw("~/.cache/日本")], "1 B".into(), 20);
        assert_eq!(line.width(), 20);
    }
}
