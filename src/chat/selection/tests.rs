use super::*;
use crate::chat::transcript::{render, ChatMessage, TranscriptRole};

fn messages(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: TranscriptRole::Assistant,
        text: text.into(),
        tool: None,
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
