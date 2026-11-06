//! Cell-based selection of wrapped transcript rows. Scroll does not move anchors.
//!
//! Two front ends share one range: the mouse (begin/drag/release, with
//! auto-scroll when the pointer leaves the viewport) and the keyboard cursor
//! (vi-like movement, `v` for a visual range, `y` to yank).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::transcript::render::TranscriptRow;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Cell {
    pub(super) row: usize,
    pub(super) column: usize,
}

/// Which way the transcript should scroll while a drag sits on an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AutoScroll {
    Older,
    Newer,
}

/// One keyboard-selection movement, already translated from a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CursorMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    Top,
    Bottom,
    PageOlder,
    PageNewer,
}

#[derive(Default)]
pub(super) struct Selection {
    anchor: Option<Cell>,
    focus: Option<Cell>,
    dragging: bool,
    moved: bool,
    /// Last drag position in terminal coordinates, so auto-scroll can extend
    /// the selection into rows the pointer has not moved over.
    pointer: Option<(u16, u16)>,
    /// Keyboard selection cursor; `Some` while selection mode is active.
    cursor: Option<Cell>,
    /// Column the cursor wants on vertical moves, as in vi.
    goal_column: usize,
    visual: bool,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.anchor.is_none() && self.cursor.is_none()
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
        self.pointer = Some((column, row));
        let focus = Cell {
            row: (start + row.saturating_sub(area.y).min(area.height - 1) as usize).min(count - 1),
            column: column.saturating_sub(area.x).min(area.width - 1) as usize,
        };
        self.moved |= self.anchor != Some(focus);
        self.focus = Some(focus);
    }

    /// The drag is being held at or beyond the top or bottom edge of `area`.
    /// The top edge counts at the first row because terminals report no
    /// pointer position above the window.
    pub(super) fn edge(&self, area: Rect) -> Option<AutoScroll> {
        let (_, row) = self.pointer.filter(|_| self.dragging)?;
        if row <= area.y {
            Some(AutoScroll::Older)
        } else if row >= area.y.saturating_add(area.height) {
            Some(AutoScroll::Newer)
        } else {
            None
        }
    }

    /// Re-applies the last drag position after the viewport scrolled, so the
    /// focus follows the pointer into newly revealed rows. The anchor is in
    /// transcript coordinates and does not move.
    pub(super) fn follow(&mut self, area: Rect, start: usize, count: usize) {
        if let Some((column, row)) = self.pointer {
            self.drag(column, row, area, start, count);
        }
    }

    pub(super) fn release(&mut self) -> bool {
        let copy = self.dragging && self.moved;
        self.dragging = false;
        self.pointer = None;
        copy
    }

    /// Starts keyboard selection with the cursor on `row`, column 0.
    pub(super) fn enter_keyboard(&mut self, row: usize) {
        self.clear();
        self.cursor = Some(Cell { row, column: 0 });
    }

    pub(super) fn keyboard_active(&self) -> bool {
        self.cursor.is_some()
    }

    pub(super) fn visual_active(&self) -> bool {
        self.cursor.is_some() && self.visual
    }

    pub(super) fn cursor(&self) -> Option<Cell> {
        self.cursor
    }

    /// Keeps the cursor on an existing row after the transcript changed;
    /// leaves selection mode if nothing is left to select.
    pub(super) fn clamp_cursor(&mut self, rows: &[TranscriptRow]) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if rows.is_empty() {
            self.clear();
            return;
        }
        let row = cursor.row.min(rows.len() - 1);
        let column = snap(&rows[row].text, cursor.column);
        self.place_cursor(Cell { row, column });
    }

    pub(super) fn move_cursor(
        &mut self,
        movement: CursorMove,
        rows: &[TranscriptRow],
        page: usize,
    ) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let last_row = rows.len() - 1;
        let starts = grapheme_starts(&rows[cursor.row].text);
        let vertical = |row: usize| Cell {
            row,
            column: snap(&rows[row].text, self.goal_column),
        };
        let next = match movement {
            CursorMove::Left => Cell {
                row: cursor.row,
                column: starts
                    .iter()
                    .rev()
                    .find(|&&start| start < cursor.column)
                    .copied()
                    .unwrap_or(0),
            },
            CursorMove::Right => Cell {
                row: cursor.row,
                column: starts
                    .iter()
                    .find(|&&start| start > cursor.column)
                    .copied()
                    .unwrap_or(cursor.column),
            },
            CursorMove::LineStart => Cell {
                row: cursor.row,
                column: 0,
            },
            CursorMove::LineEnd => Cell {
                row: cursor.row,
                column: starts.last().copied().unwrap_or(0),
            },
            CursorMove::Up => vertical(cursor.row.saturating_sub(1)),
            CursorMove::Down => vertical((cursor.row + 1).min(last_row)),
            CursorMove::PageOlder => vertical(cursor.row.saturating_sub(page.max(1))),
            CursorMove::PageNewer => vertical((cursor.row + page.max(1)).min(last_row)),
            CursorMove::Top => vertical(0),
            CursorMove::Bottom => vertical(last_row),
        };
        if matches!(
            movement,
            CursorMove::Left | CursorMove::Right | CursorMove::LineStart | CursorMove::LineEnd
        ) {
            self.goal_column = next.column;
        }
        self.place_cursor(next);
    }

    fn place_cursor(&mut self, cell: Cell) {
        self.cursor = Some(cell);
        if self.visual {
            self.focus = Some(cell);
            self.moved = true;
        }
    }

    /// `v`: starts a visual range at the cursor, or drops the range again.
    pub(super) fn toggle_visual(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if self.visual {
            self.visual = false;
            self.anchor = None;
            self.focus = None;
            self.moved = false;
        } else {
            self.visual = true;
            self.anchor = Some(cursor);
            self.focus = Some(cursor);
            self.moved = true;
        }
    }

    /// Leaves keyboard selection but keeps the highlighted range, so a
    /// yank that fails can be retried with Ctrl+Y and Esc still clears it.
    pub(super) fn leave_keyboard(&mut self) {
        self.cursor = None;
        self.visual = false;
        self.goal_column = 0;
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
        let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
        for (index, line) in lines.iter_mut().enumerate() {
            let selected = self.columns(start + index);
            let cursor = self.cursor.filter(|cursor| cursor.row == start + index);
            if selected.is_none() && cursor.is_none() {
                continue;
            }
            let mut column = 0;
            let mut spans = Vec::new();
            for span in &line.spans {
                for grapheme in span.content.graphemes(true) {
                    let next = column + grapheme.width();
                    let mut style = span.style;
                    if let Some((left, right)) = selected {
                        if next > left && column < right {
                            style = style.patch(selected_style);
                        }
                    }
                    if cursor.is_some_and(|cursor| column <= cursor.column && cursor.column < next)
                    {
                        style = style.patch(cursor_style);
                    }
                    spans.push(Span::styled(grapheme.to_string(), style));
                    column = next;
                }
            }
            line.spans = spans;
        }
    }
}

/// Cell offsets at which each grapheme of `text` starts.
fn grapheme_starts(text: &str) -> Vec<usize> {
    let mut column = 0;
    text.graphemes(true)
        .map(|grapheme| {
            let start = column;
            column += grapheme.width();
            start
        })
        .collect()
}

/// The start of the grapheme covering `column`, or the last grapheme start
/// when `column` is past the end of the row; 0 for an empty row.
fn snap(text: &str, column: usize) -> usize {
    grapheme_starts(text)
        .into_iter()
        .rev()
        .find(|&start| start <= column)
        .unwrap_or(0)
}
