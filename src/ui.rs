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
    let width = usize::from(block.inner(area).width);
    let freed: u64 = outcomes
        .iter()
        .filter(|o| o.error.is_none())
        .map(|o| o.size)
        .sum();
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    let mut summary = format!("Freed about {}", size::format(freed));
    if failed > 0 {
        summary.push_str(&format!(" · {failed} failed"));
    }
    let mut lines = vec![
        Line::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
        Line::default(),
    ];
    let dim = Style::new().add_modifier(Modifier::DIM);
    for outcome in outcomes {
        match &outcome.error {
            None => lines.push(right_aligned(
                vec![Span::raw(format!("  ✓ {}", outcome.label))],
                size::format(outcome.size),
                width,
            )),
            Some(err) => {
                lines.push(Line::from(format!("  ✗ {}", outcome.label)));
                lines.push(Line::styled(format!("      {err}"), dim));
            }
        }
        if let Some(err) = &outcome.log_error {
            lines.push(Line::styled(format!("      ! {err}"), dim));
        }
    }
    draw_scrolled(frame, area, block, lines, scroll);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_line_up_with_wide_characters() {
        let line = right_aligned(vec![Span::raw("~/.cache/日本")], "1 B".into(), 20);
        assert_eq!(line.width(), 20);
    }
}
