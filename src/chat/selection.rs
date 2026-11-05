//! Cell-based selection of wrapped transcript rows. Scroll does not move anchors.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::transcript::render::TranscriptRow;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Cell {
    row: usize,
    column: usize,
}

#[derive(Default)]
pub(super) struct Selection {
    anchor: Option<Cell>,
    focus: Option<Cell>,
    dragging: bool,
    moved: bool,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.anchor.is_none()
    }

    pub(super) fn begin(&mut self, column: u16, row: u16, area: Rect, start: usize, count: usize) {
        self.clear();
        if !area.contains((column, row).into()) || start + (row - area.y) as usize >= count {
            return;
        }
        let cell = Cell {
            row: start + (row - area.y) as usize,
            column: (column - area.x) as usize,
        };
        self.anchor = Some(cell);
        self.focus = Some(cell);
        self.dragging = true;
    }

    pub(super) fn drag(&mut self, column: u16, row: u16, area: Rect, start: usize, count: usize) {
        if !self.dragging || count == 0 || area.is_empty() {
            return;
        }
        let focus = Cell {
            row: (start + row.saturating_sub(area.y).min(area.height - 1) as usize).min(count - 1),
            column: column.saturating_sub(area.x).min(area.width - 1) as usize,
        };
        self.moved |= self.anchor != Some(focus);
        self.focus = Some(focus);
    }

    pub(super) fn release(&mut self) -> bool {
        let copy = self.dragging && self.moved;
        self.dragging = false;
        copy
    }

    fn range(&self) -> Option<(Cell, Cell)> {
        if !self.moved {
            return None;
        }
        let a = self.anchor?;
        let b = self.focus?;
        Some((a.min(b), a.max(b)))
    }

    fn columns(&self, row: usize) -> Option<(usize, usize)> {
        let (start, end) = self.range()?;
        if !(start.row..=end.row).contains(&row) {
            return None;
        }
        Some((
            if row == start.row { start.column } else { 0 },
            if row == end.row {
                end.column.saturating_add(1)
            } else {
                usize::MAX
            },
        ))
    }

    pub(super) fn text(&self, rows: &[TranscriptRow]) -> Option<String> {
        let (start, end) = self.range()?;
        let mut result = String::new();
        for (index, row) in rows.iter().enumerate().take(end.row + 1).skip(start.row) {
            let (left, right) = self.columns(index)?;
            let mut column = 0;
            for grapheme in row.text.graphemes(true) {
                let next = column + grapheme.width();
                if next > left && column < right {
                    result.push_str(grapheme);
                }
                column = next;
            }
            // Rejoin soft wraps; only original newlines and message gaps survive.
            if index < end.row && row.break_after {
                result.push('\n');
            }
        }
        (!result.is_empty()).then_some(result)
    }

    pub(super) fn highlight(&self, lines: &mut [Line<'static>], start: usize) {
        let selected_style = Style::default()
            .bg(Color::Rgb(55, 85, 120))
            .fg(Color::White);
        for (index, line) in lines.iter_mut().enumerate() {
            let Some((left, right)) = self.columns(start + index) else {
                continue;
            };
            let mut column = 0;
            let mut spans = Vec::new();
            for span in &line.spans {
                for grapheme in span.content.graphemes(true) {
                    let next = column + grapheme.width();
                    let style = if next > left && column < right {
                        span.style.patch(selected_style)
                    } else {
                        span.style
                    };
                    spans.push(Span::styled(grapheme.to_string(), style));
                    column = next;
                }
            }
            line.spans = spans;
        }
    }
}
