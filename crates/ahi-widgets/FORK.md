# ahi-widgets: a fork of six ratatui widgets

This crate is a copy of part of [`ratatui-widgets`](https://crates.io/crates/ratatui-widgets)
**0.3.0** (git tag `ratatui-widgets-v0.3.0` in [ratatui/ratatui](https://github.com/ratatui/ratatui)),
made so that `ahi` depends on `ratatui-core` and `ratatui-crossterm` alone and
not on the `ratatui` facade or the full widget set. ratatui is MIT licensed;
`LICENSE` in this directory is the upstream license with its copyright lines
(Florian Dehau; The Ratatui Developers), and every copied file carries a
header naming its upstream path.

## What was copied

| Upstream file | Here | Lines |
| --- | --- | --- |
| `src/block.rs`, `src/block/padding.rs` | `src/block.rs`, `src/block/padding.rs` | `Block`, `Padding`, `BlockExt` |
| `src/borders.rs` | `src/borders.rs` | `Borders`, `BorderType` |
| `src/paragraph.rs`, `src/reflow.rs` | `src/paragraph.rs`, `src/reflow.rs` | `Paragraph`, `Wrap`, word wrapping and truncation |
| `src/list.rs`, `src/list/{item,state,rendering}.rs` | `src/list.rs`, `src/list/{item,state,rendering}.rs` | `List`, `ListItem`, `ListState`, `ListDirection` |
| `src/table/highlight_spacing.rs` | `src/list/highlight_spacing.rs` | `HighlightSpacing` (the table widget is not forked, so the enum moved next to `List`) |
| `src/clear.rs` | `src/clear.rs` | `Clear` |

Not copied: `barchart`, `calendar`, `canvas`, `chart`, `gauge`, `logo`,
`mascot`, `scrollbar`, `sparkline`, `table`, `tabs`, and the `no_std`
polyfills.

## What was changed

Rendering code is unchanged. The edits are mechanical:

- `alloc::` and `core::` paths became `std::`; the crate is std-only.
- `strum` derives (`Display`, `EnumString`) were removed from `BorderType`,
  `Position`, `ListDirection`, and `HighlightSpacing`. ahi never formats or
  parses those enums by name.
- `#[instability::unstable]` attributes were removed from `Paragraph::line_count`
  and `Paragraph::line_width`; the methods are kept as ordinary methods.
- `itertools::Itertools::collect_vec` became `collect::<Vec<_>>`.
- `serde` `cfg_attr` derives were removed; the crate has no `serde` feature.
- The upstream unit tests were removed. They depend on `rstest`,
  `pretty_assertions`, `indoc`, and `itertools`, which would add crates to
  ahi's lockfile for tests alone. `tests/render.rs` in this crate renders
  each widget through `ratatui-core`'s `TestBackend` and compares cells, and
  ahi's own screen tests exercise every widget the way ahi uses it.

## Dependencies

`ratatui-core`, `bitflags` (for `Borders`), `unicode-width`, and
`unicode-segmentation`, all already in ahi's dependency graph. Nothing else.

## Updating

`scripts/sync-ahi-widgets.py <unpacked ratatui-widgets source> <version>`
re-copies the files and reapplies every edit above, so an update is: run the
script against the new upstream version, review the diff, bump the version
named in this file, and run `cargo test --workspace`. Upstream doc examples
import the `ratatui` facade, so the crate sets `doctest = false`; the render
tests and ahi's screen tests are the executable coverage.
