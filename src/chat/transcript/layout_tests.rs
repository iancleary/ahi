//! End-to-end layout tests: a message goes through `format::paragraphs` and
//! the wrapping layout, and the rows a terminal would draw are checked.

use super::format::Options;
use super::render::{visible_lines, Dirty, TranscriptLayout, TranscriptRow};
use super::{ChatMessage, ToolPayload, TranscriptRole};
use crate::chat::backend::tools::ToolDetail;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn options() -> Options<'static> {
    Options {
        markdown: true,
        exists: &|path| path == "src/main.rs",
    }
}

fn layout(messages: &[ChatMessage], width: usize) -> TranscriptLayout {
    let mut layout = TranscriptLayout::new();
    layout.sync(messages, width, &mut Dirty::From(0), &options());
    layout
}

/// The rows as drawn: every row padded to the full width.
fn screen(layout: &TranscriptLayout, width: usize) -> Vec<String> {
    let (lines, _) = visible_lines(layout.rows(), width, layout.len(), 0);
    lines.iter().map(|line| line.to_string()).collect()
}

/// Drawn rows with the padding trimmed, for readable expectations.
fn trimmed(messages: &[ChatMessage], width: usize) -> Vec<String> {
    screen(&layout(messages, width), width)
        .iter()
        .map(|row| row.trim_end().to_string())
        .collect()
}

/// Drawn rows of a tool message without its header, which wraps at narrow
/// widths and is not what these tests are about.
fn body(message: &ChatMessage, width: usize) -> Vec<String> {
    let laid = layout(std::slice::from_ref(message), width);
    let header = laid
        .rows()
        .iter()
        .position(|row| row.break_after)
        .map_or(0, |index| index + 1);
    screen(&laid, width)[header..]
        .iter()
        .map(|row| row.trim_end().to_string())
        .collect()
}

fn assistant(text: &str) -> ChatMessage {
    ChatMessage::new(TranscriptRole::Assistant, text)
}

fn tool(name: &str, output: &str, detail: ToolDetail) -> ChatMessage {
    let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, output);
    message.tool = Some(ToolPayload::Result {
        call_id: "c".into(),
        name: name.into(),
        content: String::new(),
    });
    message.detail = Some(detail);
    message
}

fn read(offset: Option<usize>, body: &[&str]) -> ChatMessage {
    let mut output = String::from("read_file src/main.rs");
    for line in body {
        output.push('\n');
        output.push_str(line);
    }
    tool(
        "read_file",
        &output,
        ToolDetail::Read {
            offset,
            limit: None,
            lines: body.len(),
        },
    )
}

fn write(before: Option<&str>, after: &str) -> ChatMessage {
    tool(
        "edit_file",
        "edit_file src/main.rs",
        ToolDetail::Write {
            before: before.map(str::to_string),
            after: after.to_string(),
        },
    )
}

/// What every laid-out row must satisfy, whatever the content or width.
fn check_rows(layout: &TranscriptLayout, width: usize) {
    let width = width.max(1);
    let in_bounds = |row: &TranscriptRow, start: usize, end: usize| {
        start <= end
            && end <= row.text.len()
            && row.text.is_char_boundary(start)
            && row.text.is_char_boundary(end)
    };
    for (index, row) in layout.rows().iter().enumerate() {
        let context = format!("row {index} {:?} at width {width}", row.text);
        // Only a single grapheme wider than the screen may overflow it.
        assert!(
            row.text.trim_end().width() <= width
                || row.text.trim_end().graphemes(true).count() == 1,
            "{context}"
        );
        assert!(row.hang <= row.text.len(), "{context}");
        for span in &row.spans {
            assert!(in_bounds(row, span.start, span.end), "{context}: {span:?}");
        }
        for target in &row.targets {
            assert!(
                target.start < target.end && in_bounds(row, target.start, target.end),
                "{context}: {target:?}"
            );
        }
        for column in 0..width + 2 {
            let _ = row.target_at(column);
        }
    }
    // Drawing every row must not panic, and rows are padded to the width.
    for (line, row) in screen(layout, width).iter().zip(layout.rows()) {
        assert_eq!(line.width(), width.max(row.text.width()), "{line:?}");
    }
}

/// The text of a block with soft wraps rejoined and wrap padding removed,
/// the way selection copies it.
fn copied(rows: &[TranscriptRow]) -> String {
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        let skip = match index.checked_sub(1) {
            Some(previous) if !rows[previous].break_after => row.hang,
            _ => 0,
        };
        out.push_str(&row.text[skip..]);
        if row.break_after && index + 1 < rows.len() {
            out.push('\n');
        }
    }
    out
}

const ASSORTMENT: &str = "\
# Heading one
## Heading two
### Heading three with `code` and **bold**
#### Four
##### Five
###### Six

A paragraph with **bold**, *italic*, ~~strike~~, `inline code`, ***both***, a
[link](https://example.com/a/very/long/path?query=1&other=2) and a bare
https://example.org/another/long/address/that/keeps/going link, plus src/main.rs.

- bullet one is long enough that it has to wrap onto another row at narrow widths
  - nested bullet that is also long enough to wrap around the edge of the screen
    - third level
* star bullet
+ plus bullet
1. first ordered item that wraps because it is quite a bit longer than the width
2. second
10. tenth with a wider marker that wraps because it is also rather long indeed
- [ ] open task that is long enough to wrap onto a second row of the transcript
- [x] done task

> a quoted line that is long enough to wrap onto the next row of the screen
> second quoted line

---

```rust
fn main() {
\tprintln!(\"a very long line of code that goes past the width of a narrow terminal and wraps\");
}
```

```
no language here
```

| Name | Count | Note |
|:-----|------:|:----:|
| a    |     1 | x    |
| long name here | 22 | centered |

| a | b |
|---|---|
| a table that is much too wide for a narrow screen to align | second wide cell here |

日本語のテキストは幅が二倍です。 emoji 👨‍👩‍👧 and e\u{301} combining.

```rust
fn unterminated() {";

#[test]
fn the_markdown_assortment_lays_out_cleanly_at_every_width() {
    let messages = [
        ChatMessage::new(TranscriptRole::Operator, "question"),
        assistant(ASSORTMENT),
    ];
    for width in 1..=140 {
        check_rows(&layout(&messages, width), width);
    }
    // Streaming the same text one char at a time never breaks a layout.
    let mut partial = String::new();
    for (index, ch) in ASSORTMENT.chars().enumerate() {
        partial.push(ch);
        if index % 9 == 0 {
            check_rows(&layout(&[assistant(&partial)], 37), 37);
        }
    }
}

#[test]
fn markdown_blocks_render_as_expected() {
    let text = "\
# Title
### Third

Some **bold** and `code`.

- one
  - two
1. first
10. tenth
- [ ] todo
- [x] done

> quoted

---

| A | Longer |
|:--|-------:|
| x | 1 |

```rust
let x = 1;
```";
    assert_eq!(
        trimmed(&[assistant(text)], 30),
        vec![
            "Title",
            "### Third",
            "",
            "Some bold and code.",
            "",
            "• one",
            "  • two",
            "1. first",
            "10. tenth",
            "[ ] todo",
            "[x] done",
            "",
            "▎ quoted",
            "",
            "──────────────────────────────",
            "",
            "A │ Longer",
            "──┼───────",
            "x │      1",
            "",
            "rust  copy",
            "let x = 1;",
        ]
    );
}

#[test]
fn wrapped_markdown_breaks_at_spaces_and_lines_up_under_its_text() {
    let list =
        "- alpha beta gamma delta\n  - nested iota kappa lambda\n10. ten omicron pi rho sigma";
    assert_eq!(
        trimmed(&[assistant(list)], 14),
        vec![
            "• alpha beta",
            "  gamma delta",
            "  • nested",
            "    iota kappa",
            "    lambda",
            "10. ten",
            "    omicron pi",
            "    rho sigma",
        ]
    );
    let quote = "> one two three four five six seven";
    assert_eq!(
        trimmed(&[assistant(quote)], 14),
        vec!["▎ one two", "  three four", "  five six", "  seven"]
    );
    // A word longer than the row still breaks at the cell edge, after the
    // words before it have moved up.
    assert_eq!(
        trimmed(&[assistant("see https://example.com/a/long/path ok")], 12),
        vec!["see", "https://exam", "ple.com/a/lo", "ng/path ok"]
    );
}

#[test]
fn word_wrapped_rows_copy_as_the_original_text() {
    let text = "alpha beta gamma  delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
    for width in 1..40 {
        let messages = [ChatMessage::new(TranscriptRole::Notice, text)];
        let laid = layout(&messages, width);
        check_rows(&laid, width);
        assert_eq!(copied(laid.rows()), text, "width {width}");
    }
}

#[test]
fn read_output_numbers_each_file_line_once_and_hangs_the_wrap() {
    let message = read(
        None,
        &["short", "a line long enough to wrap across rows", "end"],
    );
    assert_eq!(
        body(&message, 16),
        vec![
            "1 short",
            "2 a line long en",
            "  ough to wrap a",
            "  cross rows",
            "3 end",
        ]
    );
}

#[test]
fn read_output_gutter_follows_the_offset_and_leaves_notes_unnumbered() {
    let mut lines = vec!["[lines 8-11 of 40]"];
    lines.extend(["eight", "nine", "ten wraps around here", "eleven"]);
    lines.push("[29 more lines; continue with offset=12]");
    let message = read(Some(8), &lines);
    assert_eq!(
        body(&message, 20),
        vec![
            "[lines 8-11 of 40]",
            " 8 eight",
            " 9 nine",
            "10 ten wraps around",
            "   here",
            "11 eleven",
            "[29 more lines; cont",
            "inue with offset=12]",
        ]
    );
}

#[test]
fn wrapped_read_rows_copy_as_the_original_line() {
    let line = "let value = compute(first_argument, second_argument, third_argument);";
    let message = read(None, &[line]);
    for width in [10, 16, 23, 40, 200] {
        let laid = layout(std::slice::from_ref(&message), width);
        check_rows(&laid, width);
        let first = laid.rows().iter().position(|row| row.break_after).unwrap() + 1;
        let file_rows = &laid.rows()[first..];
        assert_eq!(copied(file_rows), format!("1 {line}"), "width {width}");
        for row in &file_rows[1..] {
            assert_eq!(&row.text[..row.hang], "  ", "width {width}: {:?}", row.text);
        }
    }
}

#[test]
fn wide_and_combining_text_wraps_under_the_hang() {
    let message = read(
        None,
        &[
            "日本語のテキストが折り返される",
            "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}",
        ],
    );
    for width in 1..30 {
        check_rows(&layout(std::slice::from_ref(&message), width), width);
    }
    assert_eq!(
        body(&message, 11),
        vec![
            "1 日本語の",
            "  テキスト",
            "  が折り返",
            "  される",
            "2 e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}",
        ]
    );
    // A combining mark stays with its base when the row wraps between them.
    let narrow = body(&message, 5);
    assert_eq!(
        narrow[narrow.len() - 2..],
        ["2 e\u{301}e\u{301}e\u{301}", "  e\u{301}e\u{301}"]
    );
}

#[test]
fn a_hang_wider_than_the_screen_still_makes_progress() {
    let message = read(Some(1000), &["abcdefghijklmnop"]);
    for width in 1..8 {
        let laid = layout(std::slice::from_ref(&message), width);
        check_rows(&laid, width);
        assert!(laid.len() < 80, "width {width}: {} rows", laid.len());
    }
}

#[test]
fn diff_rows_wrap_under_the_code_not_the_gutter() {
    let message = write(
        Some("keep\nold line that is long enough to wrap\nend"),
        "keep\nnew line that is long enough to wrap\nend",
    );
    assert_eq!(
        body(&message, 24),
        vec![
            "@@ -1,3 +1,3 @@",
            "1 1  keep",
            "2   -old line that is lo",
            "     ng enough to wrap",
            "  2 +new line that is lo",
            "     ng enough to wrap",
            "3 3  end",
        ]
    );
}

/// The text of the row's spans that carry the given emphasis style.
fn emphasised(row: &TranscriptRow) -> Vec<&str> {
    let strong = [
        super::theme::diff_added_emphasis(),
        super::theme::diff_removed_emphasis(),
    ];
    row.spans
        .iter()
        .filter(|span| strong.contains(&span.style))
        .map(|span| &row.text[span.start..span.end])
        .collect()
}

fn added_row<'a>(laid: &'a TranscriptLayout, needle: &str) -> &'a TranscriptRow {
    laid.rows()
        .iter()
        .find(|row| row.text.contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle:?}"))
}

#[test]
fn diff_emphasis_lands_on_the_changed_word() {
    let laid = layout(&[write(Some("let a = 1;"), "let a = 2;")], 40);
    assert_eq!(emphasised(added_row(&laid, "+let a = 2;")), ["2"]);
    // Tabs expand to a different byte length than the line the emphasis was
    // computed on; the emphasis follows its word.
    let laid = layout(&[write(Some("\t\told word here"), "\t\tnew word here")], 40);
    let row = added_row(&laid, "new word");
    assert!(
        row.text.contains("+        new word here"),
        "{:?}",
        row.text
    );
    assert_eq!(emphasised(row), ["new"]);
    // A change after non-tab text on a tab-indented line.
    let laid = layout(&[write(Some("a\tb old"), "a\tb new")], 40);
    assert_eq!(emphasised(added_row(&laid, "+a")), ["new"]);
}

#[test]
fn diff_emphasis_stops_at_the_clip_without_splitting_the_ellipsis() {
    // A changed word that straddles, or sits beyond, the 400-character clip.
    for pad in 380..420 {
        let before = format!("{} old tail", "a".repeat(pad));
        let after = format!("{} new tail", "a".repeat(pad));
        let message = write(Some(&before), &after);
        for width in [40, 100] {
            check_rows(&layout(std::slice::from_ref(&message), width), width);
        }
    }
    // 401 characters: the clip keeps 400 and the changed word loses its tail.
    let before = format!("{} old", "a".repeat(397));
    let after = format!("{} new", "a".repeat(397));
    let laid = layout(&[write(Some(&before), &after)], 500);
    let row = added_row(&laid, "+aaa");
    assert!(row.text.ends_with("ne…"), "{:?}", row.text.len());
    assert_eq!(emphasised(row), ["ne"], "only the part before the clip");
}

#[test]
fn created_files_with_long_lines_draw() {
    // A write whose lines exceed the clip, as a policy-approved TODO.md does.
    let long = format!("- [ ] {}", "word ".repeat(120));
    let after = format!("# Title\n\n{long}\n{long}\n");
    let message = write(None, &after);
    for width in [20, 80, 200] {
        check_rows(&layout(std::slice::from_ref(&message), width), width);
    }
}
