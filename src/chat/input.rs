//! Line editor for the chat input.
//!
//! The screen translates terminal keys into `EditorKey`. This module does
//! not read the terminal.

const ROW_LIMIT: usize = 8;

/// A key the line editor understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
    Character(char),
    Newline,
}

pub(crate) struct InputEditor {
    lines: Vec<String>,
    line: usize,
    column: usize,
}

impl InputEditor {
    pub(crate) fn new() -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            column: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let line = lines.len() - 1;
        let column = lines[line].chars().count();
        Self {
            lines,
            line,
            column,
        }
    }

    pub(crate) fn apply(&mut self, key: EditorKey) {
        match key {
            EditorKey::Left => self.move_left(),
            EditorKey::Right => self.move_right(),
            EditorKey::Up => self.move_up(),
            EditorKey::Down => self.move_down(),
            EditorKey::Home => self.move_home(),
            EditorKey::End => self.move_end(),
            EditorKey::Backspace => self.backspace(),
            EditorKey::Delete => self.delete(),
            EditorKey::Character(character) => self.insert_char(character),
            EditorKey::Newline => self.insert_newline(),
        }
    }

    pub(crate) fn contents(&self) -> String {
        self.lines.join("\n")
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.contents().trim().is_empty()
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn visible_row_count(&self) -> usize {
        self.lines.len().clamp(1, ROW_LIMIT)
    }

    pub(crate) fn cursor_line(&self) -> usize {
        self.line
    }

    pub(crate) fn cursor_column(&self) -> usize {
        self.column
    }

    /// Scroll offset and the lines that fit in the visible window.
    pub(crate) fn visible_lines(&self) -> (usize, Vec<String>) {
        let start = if self.lines.len() <= ROW_LIMIT {
            0
        } else {
            self.line
                .saturating_sub(ROW_LIMIT - 1)
                .min(self.lines.len() - ROW_LIMIT)
        };
        let end = start + self.visible_row_count();
        (start, self.lines[start..end].to_vec())
    }

    fn insert_char(&mut self, character: char) {
        let column = self.column.min(self.lines[self.line].chars().count());
        let byte = byte_index(&self.lines[self.line], column);
        self.lines[self.line].insert(byte, character);
        self.column = column + 1;
    }

    fn insert_newline(&mut self) {
        let column = self.column.min(self.lines[self.line].chars().count());
        let byte = byte_index(&self.lines[self.line], column);
        let right = self.lines[self.line].split_off(byte);
        self.lines.insert(self.line + 1, right);
        self.line += 1;
        self.column = 0;
    }

    fn backspace(&mut self) {
        if self.column > 0 {
            let end = byte_index(&self.lines[self.line], self.column);
            let start = byte_index(&self.lines[self.line], self.column - 1);
            self.lines[self.line].replace_range(start..end, "");
            self.column -= 1;
            return;
        }
        if self.line == 0 {
            return;
        }
        let current = self.lines.remove(self.line);
        self.line -= 1;
        self.column = self.lines[self.line].chars().count();
        self.lines[self.line].push_str(&current);
    }

    fn delete(&mut self) {
        let length = self.lines[self.line].chars().count();
        if self.column < length {
            let start = byte_index(&self.lines[self.line], self.column);
            let end = byte_index(&self.lines[self.line], self.column + 1);
            self.lines[self.line].replace_range(start..end, "");
            return;
        }
        if self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.column > 0 {
            self.column -= 1;
        } else if self.line > 0 {
            self.line -= 1;
            self.column = self.lines[self.line].chars().count();
        }
    }

    fn move_right(&mut self) {
        let length = self.lines[self.line].chars().count();
        if self.column < length {
            self.column += 1;
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.column = 0;
        }
    }

    fn move_up(&mut self) {
        if self.line > 0 {
            self.line -= 1;
            let length = self.lines[self.line].chars().count();
            if self.column > length {
                self.column = length;
            }
        }
    }

    fn move_down(&mut self) {
        if self.line + 1 < self.lines.len() {
            self.line += 1;
            let length = self.lines[self.line].chars().count();
            if self.column > length {
                self.column = length;
            }
        }
    }

    fn move_home(&mut self) {
        self.column = 0;
    }

    fn move_end(&mut self) {
        self.column = self.lines[self.line].chars().count();
    }
}

fn byte_index(text: &str, column: usize) -> usize {
    text.chars().take(column).map(char::len_utf8).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_grows_to_eight_rows_and_no_further() {
        let mut editor = InputEditor::from_text("a");
        assert_eq!(editor.visible_row_count(), 1);
        for _ in 0..7 {
            editor.apply(EditorKey::Newline);
        }
        assert_eq!(editor.visible_row_count(), 8);
        editor.apply(EditorKey::Newline);
        assert_eq!(editor.lines.len(), 9);
        assert_eq!(editor.visible_row_count(), 8);
    }
}
