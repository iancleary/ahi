use super::*;
use crate::chat::transcript::{render, ChatMessage, TranscriptRole};

fn messages(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: TranscriptRole::Assistant,
        text: text.into(),
        tool: None,
        detail: None,
        elapsed: None,
    }]
}

#[test]
fn unicode_selection_rejoins_soft_wraps_in_both_directions() {
    let messages = messages("ab界e\u{301}🙂Z\nlast  \nnext");
    let rows = render::rows(&messages, 5);
    assert_eq!(
        rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
        ["ab界e\u{301}", "🙂Z", "last ", " ", "next"]
    );
    let area = Rect::new(3, 4, 5, 5);
    for reverse in [false, true] {
        let mut selection = Selection::default();
        let (a, b) = if reverse {
            ((6, 6), (5, 4))
        } else {
            ((5, 4), (6, 6))
        };
        selection.begin(a.0, a.1, area, 0, rows.len());
        selection.drag(b.0, b.1, area, 0, rows.len());
        assert!(selection.release());
        assert_eq!(
            selection.text(&rows).as_deref(),
            Some("界e\u{301}🙂Z\nlast")
        );
        let mut lines = render::wrapped_lines(&messages, 5);
        selection.highlight(&mut lines, 0);
        let highlighted = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(Color::Rgb(55, 85, 120)))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(highlighted, "界e\u{301}");
    }
}

#[test]
fn copying_preserves_original_spaces_and_newlines_but_excludes_screen_padding() {
    let rows = render::rows(&messages("hello  \nworld"), 4);
    let area = Rect::new(0, 0, 4, 4);
    let mut selection = Selection::default();
    selection.begin(0, 0, area, 0, rows.len());
    assert!(!selection.release()); // A click alone is not a text selection.
    assert_eq!(selection.text(&rows), None);
    selection.begin(0, 0, area, 0, rows.len());
    selection.drag(3, 3, area, 0, rows.len());
    assert!(selection.release());
    assert_eq!(selection.text(&rows).as_deref(), Some("hello  \nworld"));
    selection.clear();
    assert_eq!(selection.text(&rows), None);
}

#[test]
fn wheel_scrolling_during_a_drag_keeps_the_original_transcript_anchor() {
    let rows = render::rows(&messages("0\n1\n2\n3\n4"), 8);
    let area = Rect::new(0, 0, 8, 2);
    let mut selection = Selection::default();
    selection.begin(0, 0, area, 2, rows.len());
    selection.drag(0, 1, area, 3, rows.len());
    assert_eq!(selection.text(&rows).as_deref(), Some("2\n3\n4"));
    assert!(selection.release());
}

#[test]
fn edge_detection_follows_the_pointer_and_ends_on_release() {
    let rows = render::rows(&messages("0\n1\n2\n3\n4\n5\n6\n7"), 8);
    let area = Rect::new(0, 2, 8, 3);
    let mut selection = Selection::default();
    assert_eq!(selection.edge(area), None);
    selection.begin(0, 3, area, 2, rows.len()); // row 3 of the transcript
    selection.drag(0, 3, area, 2, rows.len());
    assert_eq!(selection.edge(area), None);
    selection.drag(0, 7, area, 2, rows.len()); // below the area: newer
    assert_eq!(selection.edge(area), Some(AutoScroll::Newer));
    assert_eq!(selection.text(&rows).as_deref(), Some("3\n4"));
    selection.follow(area, 3, rows.len()); // viewport scrolled one row
    assert_eq!(selection.text(&rows).as_deref(), Some("3\n4\n5"));
    selection.drag(0, 2, area, 3, rows.len()); // first row: older
    assert_eq!(selection.edge(area), Some(AutoScroll::Older));
    selection.follow(area, 1, rows.len());
    assert_eq!(selection.text(&rows).as_deref(), Some("1\n2\n3"));
    assert!(selection.release());
    assert_eq!(selection.edge(area), None);
}

#[test]
fn keyboard_cursor_snaps_to_graphemes_and_visual_range_yanks_exact_text() {
    let messages = messages("ab界e\u{301}🙂Z\nlast  \nnext");
    let rows = render::rows(&messages, 5);
    let mut selection = Selection::default();
    selection.enter_keyboard(0);
    assert!(selection.keyboard_active());
    assert!(!selection.visual_active());
    assert_eq!(selection.text(&rows), None);
    let columns = |selection: &Selection| selection.cursor().unwrap().column;
    selection.move_cursor(CursorMove::Right, &rows, 2); // a -> b
    selection.move_cursor(CursorMove::Right, &rows, 2); // b -> 界
    assert_eq!(columns(&selection), 2);
    selection.move_cursor(CursorMove::Right, &rows, 2); // 界 -> é (two cells wide skipped)
    assert_eq!(columns(&selection), 4);
    selection.move_cursor(CursorMove::Right, &rows, 2); // end of row: stays
    assert_eq!(columns(&selection), 4);
    selection.move_cursor(CursorMove::Down, &rows, 2); // "🙂Z": goal 4 snaps to Z
    assert_eq!(selection.cursor().unwrap(), Cell { row: 1, column: 2 });
    selection.move_cursor(CursorMove::Up, &rows, 2);
    assert_eq!(selection.cursor().unwrap(), Cell { row: 0, column: 4 });
    selection.move_cursor(CursorMove::Left, &rows, 2);
    assert_eq!(columns(&selection), 2);
    selection.toggle_visual(VisualMode::Char, &rows);
    assert!(selection.visual_active());
    assert_eq!(selection.text(&rows).as_deref(), Some("界"));
    selection.move_cursor(CursorMove::Down, &rows, 2);
    selection.move_cursor(CursorMove::Down, &rows, 2);
    selection.move_cursor(CursorMove::LineEnd, &rows, 2);
    assert_eq!(
        selection.text(&rows).as_deref(),
        Some("界e\u{301}🙂Z\nlast ")
    );
    selection.move_cursor(CursorMove::Bottom, &rows, 2);
    assert_eq!(selection.cursor().unwrap().row, 4);
    selection.move_cursor(CursorMove::PageOlder, &rows, 2);
    assert_eq!(selection.cursor().unwrap().row, 2);
    selection.move_cursor(CursorMove::Top, &rows, 2);
    selection.move_cursor(CursorMove::LineStart, &rows, 2);
    // Range is ordered regardless of direction.
    assert_eq!(selection.text(&rows).as_deref(), Some("ab界"));
    let mut lines = render::wrapped_lines(&messages, 5);
    selection.highlight(&mut lines, 0);
    assert!(lines[0]
        .spans
        .iter()
        .any(|span| span.style.add_modifier.contains(Modifier::REVERSED)));
    selection.toggle_visual(VisualMode::Char, &rows);
    assert!(!selection.visual_active());
    assert_eq!(selection.text(&rows), None);
    selection.toggle_visual(VisualMode::Char, &rows);
    selection.leave_keyboard();
    assert!(!selection.keyboard_active());
    assert_eq!(selection.text(&rows).as_deref(), Some("a"));
    // Transcript shrinks under an active cursor.
    selection.enter_keyboard(4);
    selection.clamp_cursor(&rows[..2]);
    assert_eq!(selection.cursor().unwrap().row, 1);
    selection.clamp_cursor(&[]);
    assert!(selection.is_empty());
}

#[test]
fn word_motions_follow_vi_character_classes_and_cross_rows() {
    // "foo.bar  界界x" then an empty row, then "  z".
    // Vim classes: Latin and CJK are different words; emoji are their own.
    let messages = messages("foo.bar  界界x🙂\n\n  z");
    let rows = render::rows(&messages, 40);
    let mut selection = Selection::default();
    selection.enter_keyboard(0);
    let at = |selection: &Selection| {
        let cursor = selection.cursor().unwrap();
        (cursor.row, cursor.column)
    };
    // w: foo -> . -> bar -> 界界 (one run) -> x -> 🙂 -> empty row -> z -> stays
    let forward = [
        (0, 3),
        (0, 4),
        (0, 9),
        (0, 13),
        (0, 14),
        (1, 0),
        (2, 2),
        (2, 2),
    ];
    for expected in forward {
        selection.move_cursor(CursorMove::WordForward, &rows, 1);
        assert_eq!(at(&selection), expected);
    }
    // b retraces the same stops back to the first column.
    let backward = [
        (1, 0),
        (0, 14),
        (0, 13),
        (0, 9),
        (0, 4),
        (0, 3),
        (0, 0),
        (0, 0),
    ];
    for expected in backward {
        selection.move_cursor(CursorMove::WordBackward, &rows, 1);
        assert_eq!(at(&selection), expected);
    }
    // b from inside a word goes to its start, w from inside skips to the next.
    selection.move_cursor(CursorMove::Right, &rows, 1);
    selection.move_cursor(CursorMove::WordBackward, &rows, 1);
    assert_eq!(at(&selection), (0, 0));
    selection.move_cursor(CursorMove::Right, &rows, 1);
    selection.move_cursor(CursorMove::WordForward, &rows, 1);
    assert_eq!(at(&selection), (0, 3));
}

#[test]
fn linewise_visual_selects_whole_logical_lines_with_trailing_newline() {
    // Width 5 soft-wraps "abcdefgh" into "abcde" + "fgh".
    let messages = messages("abcdefgh\nij\nkl");
    let rows = render::rows(&messages, 5);
    assert_eq!(rows.len(), 4);
    let mut selection = Selection::default();
    selection.enter_keyboard(1); // on the soft-wrapped tail "fgh"
    selection.move_cursor(CursorMove::Right, &rows, 1);
    selection.toggle_visual(VisualMode::Line, &rows);
    assert_eq!(selection.visual_mode(), Some(VisualMode::Line));
    // The whole logical line, soft wrap rejoined, newline included.
    assert_eq!(selection.text(&rows).as_deref(), Some("abcdefgh\n"));
    selection.move_cursor(CursorMove::Down, &rows, 1);
    assert_eq!(selection.text(&rows).as_deref(), Some("abcdefgh\nij\n"));
    let mut lines = render::wrapped_lines(&messages, 5);
    selection.highlight(&mut lines, 0);
    let highlighted_rows = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(Color::Rgb(55, 85, 120)))
        })
        .collect::<Vec<_>>();
    assert_eq!(highlighted_rows, [true, true, true, false]);
    // v switches to character-wise from the same anchor; V again leaves visual.
    selection.toggle_visual(VisualMode::Char, &rows);
    assert_eq!(selection.visual_mode(), Some(VisualMode::Char));
    assert_eq!(selection.text(&rows).as_deref(), Some("gh\nij"));
    selection.toggle_visual(VisualMode::Line, &rows);
    assert_eq!(selection.text(&rows).as_deref(), Some("abcdefgh\nij\n"));
    selection.toggle_visual(VisualMode::Line, &rows);
    assert_eq!(selection.visual_mode(), None);
    assert_eq!(selection.text(&rows), None);
    // The last row has no following newline in the transcript.
    selection.move_cursor(CursorMove::Bottom, &rows, 1);
    selection.toggle_visual(VisualMode::Line, &rows);
    assert_eq!(selection.text(&rows).as_deref(), Some("kl\n"));
}

#[test]
fn vim_character_classes_match_utf_class() {
    assert_eq!(vim_class(' '), 0);
    assert_eq!(vim_class('\u{3000}'), 0); // ideographic space
    assert_eq!(vim_class('a'), 2);
    assert_eq!(vim_class('_'), 2);
    assert_eq!(vim_class('é'), 2); // 192-255 are keyword by default
    assert_eq!(vim_class('.'), 1);
    assert_eq!(vim_class('«'), 1); // 0xab, below 192
    assert_eq!(vim_class('—'), 1); // general punctuation
    assert_eq!(vim_class('、'), 1); // ideographic comma
    assert_eq!(vim_class('界'), 0x4e00);
    assert_eq!(vim_class('あ'), 0x3040);
    assert_eq!(vim_class('ア'), 0x30a0);
    assert_eq!(vim_class('한'), 0xac00);
    assert_eq!(vim_class('🙂'), 3);
    assert_eq!(vim_class('Ж'), 2); // Cyrillic: plain word character
}
