//! Drawing helpers shared by the module screens.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::log::Outcome;
use crate::size;

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
    let done = |moved: bool| -> u64 {
        outcomes
            .iter()
            .filter(|o| o.error.is_none() && o.moved == moved)
            .map(|o| o.size)
            .sum()
    };
    let (freed, moved) = (done(false), done(true));
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    let mut parts = Vec::new();
    if freed > 0 || moved == 0 {
        parts.push(format!("freed about {}", size::format(freed)));
    }
    if moved > 0 {
        parts.push(format!("moved {} into a Backup", size::format(moved)));
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
                let to = if outcome.moved { " → Backup" } else { "" };
                lines.push(right_aligned(
                    vec![Span::raw(format!("  ✓ {}{to}", outcome.label))],
                    size::format(outcome.size),
                    width,
                ))
            }
            Some(err) => {
                lines.push(Line::from(format!("  ✗ {}", outcome.label)));
                lines.push(Line::styled(format!("      {err}"), dim));
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

    fn outcome(label: &str, size: u64, error: Option<&str>, moved: bool) -> Outcome {
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
        let text = render(&[
            outcome("pkg", 2048, None, false),
            outcome("~/.config/gone", 1024, None, true),
            outcome("~/.config/stuck", 1, Some("refused"), true),
        ]);
        assert!(
            text.contains("Freed about 2.0 KiB · moved 1.0 KiB into a Backup · 1 failed"),
            "{text}"
        );
        assert!(text.contains("✓ ~/.config/gone → Backup"));
        let text = render(&[outcome("~/.config/gone", 1024, None, true)]);
        assert!(text.contains("Moved 1.0 KiB into a Backup"), "{text}");
        assert!(render(&[]).contains("Freed about 0 B"));
    }

    #[test]
    fn sizes_line_up_with_wide_characters() {
        let line = right_aligned(vec![Span::raw("~/.cache/日本")], "1 B".into(), 20);
        assert_eq!(line.width(), 20);
    }
}
