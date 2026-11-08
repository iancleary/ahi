//! The widgets ahi draws with (`Block`, `Paragraph`, `List`, `Clear`),
//! forked from `ratatui-widgets` 0.3.0 under the MIT License so that ahi
//! depends on `ratatui-core` and `ratatui-crossterm` alone. `FORK.md` lists
//! what was copied, what was removed, and why; `LICENSE` carries the
//! upstream copyright.
//!
//! Rendering matches upstream at that version: ahi's screen tests compare
//! terminal cells and pass unchanged against this crate.

pub mod block;
pub mod borders;
pub mod clear;
pub mod list;
pub mod paragraph;
mod reflow;

pub use block::{Block, BlockExt, Padding};
pub use borders::{BorderType, Borders};
pub use clear::Clear;
pub use list::{HighlightSpacing, List, ListDirection, ListItem, ListState};
pub use paragraph::{Paragraph, Wrap};
