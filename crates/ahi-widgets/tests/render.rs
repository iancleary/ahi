//! Each forked widget rendered through `ratatui-core`'s test backend and
//! compared cell by cell. These are the fork's own tests; the upstream unit
//! tests were not copied (see FORK.md).

use ahi_widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_core::backend::TestBackend;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::terminal::Terminal;
use ratatui_core::text::Line;
use ratatui_core::widgets::{StatefulWidget, Widget};

fn render(width: u16, height: u16, draw: impl FnOnce(&mut Buffer, Rect)) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw(frame.buffer_mut(), area);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn block_draws_borders_and_a_title() {
    let buffer = render(12, 3, |buf, area| {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("ahi")
            .render(area, buf);
    });
    assert_eq!(
        buffer,
        Buffer::with_lines(["╭ahi───────╮", "│          │", "╰──────────╯"])
    );
}

#[test]
fn paragraph_wraps_words_and_trims_leading_space() {
    let buffer = render(10, 3, |buf, area| {
        Paragraph::new("hello wide world")
            .wrap(Wrap { trim: true })
            .render(area, buf);
    });
    assert_eq!(
        buffer,
        Buffer::with_lines(["hello wide", "world     ", "          "])
    );
}

#[test]
fn paragraph_inside_a_block_uses_the_inner_area() {
    let buffer = render(9, 3, |buf, area| {
        Paragraph::new("inner")
            .block(Block::default().borders(Borders::ALL))
            .render(area, buf);
    });
    assert_eq!(
        buffer,
        Buffer::with_lines(["┌───────┐", "│inner  │", "└───────┘"])
    );
}

#[test]
fn list_highlights_the_selected_item_with_a_symbol() {
    let mut state = ListState::default();
    state.select(Some(1));
    let buffer = render(8, 3, |buf, area| {
        let list = List::new([
            ListItem::new("one"),
            ListItem::new("two"),
            ListItem::new("three"),
        ])
        .highlight_symbol(">> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
        StatefulWidget::render(list, area, buf, &mut state);
    });
    let mut expected = Buffer::with_lines(["   one  ", ">> two  ", "   three"]);
    expected.set_style(
        Rect::new(0, 1, 8, 1),
        Style::default().add_modifier(Modifier::BOLD),
    );
    assert_eq!(buffer, expected);
    assert_eq!(state.selected(), Some(1));
}

#[test]
fn clear_resets_cells_to_blank() {
    let buffer = render(5, 1, |buf, area| {
        Paragraph::new(Line::from("abcde")).render(area, buf);
        Clear.render(Rect::new(1, 0, 3, 1), buf);
    });
    assert_eq!(buffer, Buffer::with_lines(["a   e"]));
}
