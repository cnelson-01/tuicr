//! Find-in-files results, rendered in place of the diff: one row per hit
//! (`path:line:col  text`), the selected row highlighted.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, FIND_MAX_HITS};
use crate::ui::styles;
use crate::ui::text_utils::truncate_str;

pub(super) fn render_find_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let find = &app.find;
    let count = find.hits.len();
    let cap = if find.truncated {
        format!(" (first {FIND_MAX_HITS})")
    } else {
        String::new()
    };
    let title = format!(
        " Find \u{00b7} \"{}\" \u{00b7} {count} hit{}{cap} \u{00b7} {} ",
        find.query,
        if count == 1 { "" } else { "s" },
        find.tool
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(styles::panel_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = usize::from(inner.height);
    let width = usize::from(inner.width);
    let selected = find.selected.min(count.saturating_sub(1));

    // Keep the selection inside the viewport.
    let mut offset = find.scroll_offset;
    if height > 0 {
        if selected < offset {
            offset = selected;
        } else if selected >= offset + height {
            offset = selected + 1 - height;
        }
    }
    let offset = offset.min(count.saturating_sub(1));

    let lines: Vec<Line> = find
        .hits
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, hit)| {
            let is_selected = i == selected;
            let location = format!("{}:{}:{}", hit.path.display(), hit.line, hit.column);
            let text_room = width.saturating_sub(location.chars().count() + 4);
            let text = truncate_str(hit.text.trim(), text_room);
            let row_style = if is_selected {
                styles::selected_style(theme).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg_primary)
            };
            let location_style = if is_selected {
                row_style
            } else {
                styles::hash_style(theme)
            };
            Line::from(vec![
                Span::styled(if is_selected { "\u{25b8} " } else { "  " }, row_style),
                Span::styled(location, location_style),
                Span::styled(format!("  {text}"), row_style),
            ])
        })
        .collect();

    app.find.scroll_offset = offset;
    app.find.viewport_height = height;
    frame.render_widget(
        Paragraph::new(lines).style(styles::panel_style(&app.theme)),
        inner,
    );
}
