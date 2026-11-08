#!/usr/bin/env python3
"""Refresh crates/ahi-widgets from an unpacked ratatui-widgets source tree.

Usage: scripts/sync-ahi-widgets.py <path to ratatui-widgets-X.Y.Z> <X.Y.Z>

Copies the six widgets ahi uses and applies the mechanical edits listed in
crates/ahi-widgets/FORK.md: std paths, no strum or instability or itertools,
no serde cfg_attrs, upstream unit tests removed, doc links to widgets that are
not in the fork turned into plain code. Rendering code is not touched. Run
`cargo fmt --all`, the workspace tests, and clippy afterwards, then review the
diff against the previous copy before committing.
"""
import pathlib
import re
import sys

FILES = {
    "block.rs": "block.rs",
    "block/padding.rs": "block/padding.rs",
    "borders.rs": "borders.rs",
    "paragraph.rs": "paragraph.rs",
    "reflow.rs": "reflow.rs",
    "list.rs": "list.rs",
    "list/item.rs": "list/item.rs",
    "list/state.rs": "list/state.rs",
    "list/rendering.rs": "list/rendering.rs",
    "list/highlight_spacing.rs": "table/highlight_spacing.rs",
    "clear.rs": "clear.rs",
}


def header(upstream: str, version: str) -> str:
    return (
        f"// Copied from ratatui-widgets {version} (`ratatui-widgets/src/{upstream}`), MIT License,\n"
        "// Copyright (c) 2016-2022 Florian Dehau, Copyright (c) 2023-2025 The Ratatui\n"
        "// Developers. See LICENSE and FORK.md in this crate for the changes made.\n\n"
    )


def strip_derives(text: str) -> str:
    def keep(match: re.Match) -> str:
        names = [t.strip() for t in match.group(1).split(",")]
        kept = [n for n in names if n not in ("Display", "EnumString")]
        return "#[derive(" + ", ".join(kept) + ")]"

    return re.sub(r"#\[derive\(([^)]*)\)\]", keep, text)


def strip_tests(text: str, name: str) -> str:
    start = text.find("#[cfg(test)]\nmod tests {")
    if start < 0:
        return text
    brace = text.index("{", start)
    depth = 0
    index = brace
    while index < len(text):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                break
        index += 1
    if text[index + 1 :].strip():
        raise SystemExit(f"{name}: the tests module is not at the end of the file")
    return text[:start].rstrip() + "\n"


def transform(text: str, name: str) -> str:
    text = re.sub(r"\balloc::", "std::", text)
    text = re.sub(r"\bcore::", "std::", text)
    text = re.sub(r"^\s*use std::vec;\n", "", text, flags=re.M)
    text = re.sub(r"^\s*use strum::\{[^}]*\};\n", "", text, flags=re.M)
    text = re.sub(r"^\s*use strum::ParseError;\n", "", text, flags=re.M)
    text = strip_derives(text)
    text = re.sub(r"^\s*#\[strum\([^\n]*\n", "", text, flags=re.M)
    text = re.sub(r"^\s*#\[cfg_attr\(feature = \"serde\"[^\n]*\n", "", text, flags=re.M)
    text = re.sub(r"[ \t]*#\[instability::unstable\((?:[^()]*|\([^()]*\))*\)\]\n", "", text)
    text = text.replace("use itertools::Itertools;\n", "")
    text = text.replace(".collect_vec()", ".collect::<Vec<_>>()")
    text = text.replace("use crate::table::HighlightSpacing;", "use crate::list::HighlightSpacing;")
    # Clippy on current stable; upstream targets an older MSRV.
    text = text.replace(".truncate(0);", ".clear();")
    # Doc links to items that are not in the fork.
    text = re.sub(r"^\s*/// \[`Table`\]: crate::table::Table\n", "", text, flags=re.M)
    text = text.replace("[`Table`]", "`Table`")
    text = text.replace("[`crate::table::Table`]", "`Table` (not in this fork)")
    for variant in ("Replace", "Fuzzy", "Exact"):
        text = text.replace(f"[`{variant}`]", f"`MergeStrategy::{variant}`")
    text = strip_tests(text, name)
    return text


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    source = pathlib.Path(sys.argv[1]) / "src"
    version = sys.argv[2]
    root = pathlib.Path(__file__).resolve().parent.parent / "crates" / "ahi-widgets" / "src"
    for target, upstream in FILES.items():
        text = (source / upstream).read_text()
        text = transform(text, upstream)
        if target == "list/highlight_spacing.rs":
            pass
        if target == "list.rs":
            text = text.replace("use crate::list::HighlightSpacing;\n", "")
            text = text.replace(
                "pub use self::item::ListItem;",
                "pub use self::highlight_spacing::HighlightSpacing;\npub use self::item::ListItem;",
                1,
            )
            text = text.replace(
                "mod item;\n",
                "// `HighlightSpacing` lived beside the table widget upstream; the table is not\n"
                "// in this fork, so it lives here.\nmod highlight_spacing;\nmod item;\n",
                1,
            )
        out = root / target
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(header(upstream, version) + text)
        print(f"{upstream} -> {target}")


if __name__ == "__main__":
    main()
