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
    /// `w`: start of the next word, crossing row ends like vi.
    WordForward,
    /// `b`: start of the current or previous word.
    WordBackward,
}

/// `v` selects cells; `V` selects whole logical lines, soft wraps included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VisualMode {
    Char,
    Line,
}

/// Vim's character class for word motions (`utf_class` in `mbyte.c`, with the
/// default `iskeyword=@,48-57,_,192-255`): 0 blank, 1 punctuation, 2 keyword,
/// 3 emoji, and one class per script block for CJK, Hiragana, Katakana and
/// Hangul so each script is its own word. A word is a run of one class.
type CharClass = u32;
const BLANK: CharClass = 0;

fn class_of(grapheme: &str) -> CharClass {
    let Some(first) = grapheme.chars().next() else {
        return BLANK;
    };
    vim_class(first)
}

fn vim_class(ch: char) -> CharClass {
    let code = ch as u32;
    if code < 0x100 {
        return if ch == ' ' || ch == '\t' || ch == '\0' || ch == '\u{a0}' {
            BLANK
        } else if ch.is_ascii_alphanumeric() || ch == '_' || (0xc0..=0xff).contains(&code) {
            2
        } else {
            1
        };
    }
    if is_emoji(code) {
        return 3;
    }
    // Vim's class table: (first, last, class). Sorted by `first`.
    const CLASSES: &[(u32, u32, CharClass)] = &[
        (0x037e, 0x037e, 1),
        (0x0387, 0x0387, 1),
        (0x055a, 0x055f, 1),
        (0x0589, 0x0589, 1),
        (0x05be, 0x05be, 1),
        (0x05c0, 0x05c0, 1),
        (0x05c3, 0x05c3, 1),
        (0x05f3, 0x05f4, 1),
        (0x060c, 0x060c, 1),
        (0x061b, 0x061b, 1),
        (0x061f, 0x061f, 1),
        (0x066a, 0x066d, 1),
        (0x06d4, 0x06d4, 1),
        (0x0700, 0x070d, 1),
        (0x0964, 0x0965, 1),
        (0x0970, 0x0970, 1),
        (0x0df4, 0x0df4, 1),
        (0x0e4f, 0x0e4f, 1),
        (0x0e5a, 0x0e5b, 1),
        (0x0f04, 0x0f12, 1),
        (0x0f3a, 0x0f3d, 1),
        (0x0f85, 0x0f85, 1),
        (0x104a, 0x104f, 1),
        (0x10fb, 0x10fb, 1),
        (0x1361, 0x1368, 1),
        (0x166d, 0x166e, 1),
        (0x1680, 0x1680, 0),
        (0x169b, 0x169c, 1),
        (0x16eb, 0x16ed, 1),
        (0x1735, 0x1736, 1),
        (0x17d4, 0x17dc, 1),
        (0x1800, 0x180a, 1),
        (0x2000, 0x200b, 0),
        (0x200c, 0x2027, 1),
        (0x2028, 0x2029, 0),
        (0x202a, 0x202e, 1),
        (0x202f, 0x202f, 0),
        (0x2030, 0x205e, 1),
        (0x205f, 0x205f, 0),
        (0x2060, 0x206f, 1),
        (0x2070, 0x207f, 0x2070),
        (0x2080, 0x2094, 0x2080),
        (0x20a0, 0x27ff, 1),
        (0x2800, 0x28ff, 0x2800),
        (0x2900, 0x2998, 1),
        (0x29d8, 0x29db, 1),
        (0x29fc, 0x29fd, 1),
        (0x2e00, 0x2e7f, 1),
        (0x3000, 0x3000, 0),
        (0x3001, 0x3020, 1),
        (0x3030, 0x3030, 1),
        (0x303d, 0x303d, 1),
        (0x3040, 0x309f, 0x3040),
        (0x30a0, 0x30ff, 0x30a0),
        (0x3300, 0x9fff, 0x4e00),
        (0xac00, 0xd7a3, 0xac00),
        (0xf900, 0xfaff, 0x4e00),
        (0xfd3e, 0xfd3f, 1),
        (0xfe30, 0xfe6b, 1),
        (0xff00, 0xff0f, 1),
        (0xff1a, 0xff20, 1),
        (0xff3b, 0xff40, 1),
        (0xff5b, 0xff65, 1),
        (0x1d000, 0x1d24f, 1),
        (0x1d400, 0x1d7ff, 1),
        (0x1f000, 0x1f2ff, 1),
        (0x1f300, 0x1f9ff, 1),
        (0x20000, 0x2a6df, 0x4e00),
        (0x2a700, 0x2b73f, 0x4e00),
        (0x2b740, 0x2b81f, 0x4e00),
        (0x2f800, 0x2fa1f, 0x4e00),
    ];
    let index = CLASSES.partition_point(|&(first, _, _)| first <= code);
    match index.checked_sub(1).map(|i| CLASSES[i]) {
        Some((_, last, class)) if code <= last => class,
        // Everything else is a word character, as in Vim.
        _ => 2,
    }
}

/// Vim's `emoji_all` table, reduced to the blocks that are entirely emoji.
fn is_emoji(code: u32) -> bool {
    matches!(
        code,
        0x1f1e6..=0x1f1ff
            | 0x1f300..=0x1f64f
            | 0x1f680..=0x1f6ff
            | 0x1f900..=0x1f9ff
            | 0x1fa70..=0x1faff
            | 0x2600..=0x27bf
    )
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
    visual: Option<VisualMode>,
    /// Row span of the logical lines a line-wise range covers, kept current
    /// on every cursor move so highlighting needs no row data.
    line_span: Option<(usize, usize)>,
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
        self.cursor.is_some() && self.visual.is_some()
    }

    pub(super) fn visual_mode(&self) -> Option<VisualMode> {
        self.cursor.and(self.visual)
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
        self.place_cursor(Cell { row, column }, rows);
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
            CursorMove::WordForward => word_forward(cursor, rows),
            CursorMove::WordBackward => word_backward(cursor, rows),
        };
        if !matches!(
            movement,
            CursorMove::Up
                | CursorMove::Down
                | CursorMove::PageOlder
                | CursorMove::PageNewer
                | CursorMove::Top
                | CursorMove::Bottom
        ) {
            self.goal_column = next.column;
        }
        self.place_cursor(next, rows);
    }

    fn place_cursor(&mut self, cell: Cell, rows: &[TranscriptRow]) {
        self.cursor = Some(cell);
        if self.visual.is_some() {
            self.focus = Some(cell);
            self.moved = true;
            self.update_line_span(rows);
        }
    }

    /// `v` or `V`: starts a visual range at the cursor, switches between the
    /// two kinds, or, for the kind already active, drops the range again.
    pub(super) fn toggle_visual(&mut self, mode: VisualMode, rows: &[TranscriptRow]) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if self.visual == Some(mode) {
            self.visual = None;
            self.anchor = None;
            self.focus = None;
            self.moved = false;
            self.line_span = None;
            return;
        }
        if self.visual.is_none() {
            self.anchor = Some(cursor);
            self.focus = Some(cursor);
            self.moved = true;
        }
        self.visual = Some(mode);
        self.update_line_span(rows);
    }

    fn update_line_span(&mut self, rows: &[TranscriptRow]) {
        self.line_span = match (self.visual, self.range()) {
            (Some(VisualMode::Line), Some((start, end))) => {
                Some((line_start(rows, start.row), line_end(rows, end.row)))
            }
            _ => None,
        };
    }

    /// Leaves keyboard selection but keeps the highlighted range, so a
    /// yank that fails can be retried with Ctrl+Y and Esc still clears it.
    pub(super) fn leave_keyboard(&mut self) {
        self.cursor = None;
        self.visual = None;
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
        if let Some((first, last)) = self.line_span {
            return (first..=last).contains(&row).then_some((0, usize::MAX));
        }
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
        let (first, last) = self.line_span.unwrap_or((start.row, end.row));
        let mut result = String::new();
        for (index, row) in rows.iter().enumerate().take(last + 1).skip(first) {
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
            // A line-wise yank ends with its newline, as in vi.
            if (index < last || self.line_span.is_some()) && row.break_after {
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

/// First row of the logical line containing `row`: soft wraps are rejoined.
fn line_start(rows: &[TranscriptRow], row: usize) -> usize {
    (0..row)
        .rev()
        .find(|&previous| rows[previous].break_after)
        .map_or(0, |previous| previous + 1)
}

/// Last row of the logical line containing `row`.
fn line_end(rows: &[TranscriptRow], row: usize) -> usize {
    (row..rows.len())
        .find(|&next| rows[next].break_after)
        .unwrap_or(rows.len().saturating_sub(1))
}

/// Graphemes of a row with their start columns and classes.
fn classified(text: &str) -> Vec<(usize, CharClass)> {
    let mut column = 0;
    text.graphemes(true)
        .map(|grapheme| {
            let start = column;
            column += grapheme.width();
            (start, class_of(grapheme))
        })
        .collect()
}

/// vi `w`: skip the rest of the current word, then blanks, crossing rows.
/// An empty row counts as a word; the last row's end is the final stop.
fn word_forward(cursor: Cell, rows: &[TranscriptRow]) -> Cell {
    let cells = classified(&rows[cursor.row].text);
    let mut index = cells
        .iter()
        .position(|&(start, _)| start >= cursor.column)
        .unwrap_or(cells.len());
    if let Some(&(_, class)) = cells.get(index) {
        if class != BLANK {
            while cells.get(index).is_some_and(|&(_, next)| next == class) {
                index += 1;
            }
        }
    }
    while cells.get(index).is_some_and(|&(_, class)| class == BLANK) {
        index += 1;
    }
    if let Some(&(start, _)) = cells.get(index) {
        return Cell {
            row: cursor.row,
            column: start,
        };
    }
    // Continue on following rows: the first non-blank grapheme, or an empty row.
    for (row, next) in rows.iter().enumerate().skip(cursor.row + 1) {
        let cells = classified(&next.text);
        if cells.is_empty() {
            return Cell { row, column: 0 };
        }
        if let Some(&(start, _)) = cells.iter().find(|&&(_, class)| class != BLANK) {
            return Cell { row, column: start };
        }
    }
    Cell {
        row: cursor.row,
        column: cells.last().map_or(0, |&(start, _)| start),
    }
}

/// vi `b`: step back over blanks, then to the start of that word, crossing rows.
fn word_backward(cursor: Cell, rows: &[TranscriptRow]) -> Cell {
    let mut row = cursor.row;
    let mut cells = classified(&rows[row].text);
    // Index of the grapheme before the cursor, if any on this row.
    let mut index = cells.iter().rposition(|&(start, _)| start < cursor.column);
    loop {
        // Skip blanks backward, moving to earlier rows as needed.
        while index.is_some_and(|i| cells[i].1 == BLANK) {
            index = index.and_then(|i| i.checked_sub(1));
        }
        if let Some(i) = index {
            let class = cells[i].1;
            let mut first = i;
            while first > 0 && cells[first - 1].1 == class {
                first -= 1;
            }
            return Cell {
                row,
                column: cells[first].0,
            };
        }
        if row == 0 {
            return Cell { row: 0, column: 0 };
        }
        row -= 1;
        cells = classified(&rows[row].text);
        if cells.is_empty() {
            return Cell { row, column: 0 };
        }
        index = Some(cells.len() - 1);
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
