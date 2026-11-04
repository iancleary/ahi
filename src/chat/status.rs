//! Styled Powerlevel10k-like location row and session row text.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::backend::Selection;
use super::location::GitStatus;
use super::thinking::ThinkingLevel;
use super::usage::SessionMeters;

pub(crate) fn session_model_segment(selection: &Selection, thinking: ThinkingLevel) -> String {
    format!(
        "({}) {} • {}",
        selection.gateway,
        selection.model,
        thinking.as_str()
    )
}

/// Embed known context usage near the right end of the input's upper rule.
/// Keep two rule cells on either side of the padded label; compact or hide
/// the label instead of wrapping it. Unknown/zero-capacity context stays blank.
pub(crate) fn render_context_rule(width: usize, meters: &SessionMeters) -> Line<'static> {
    let context = meters
        .context_tokens
        .zip(meters.context_window)
        .and_then(|(used, window)| format_percent(used, window).map(|percent| (percent, window)));
    if let Some((percent, window)) = context {
        for label in [
            format!("{percent}% / {}", format_count(window)),
            format!("{percent}%"),
        ] {
            // Label text is ASCII. Four spaces/rule cells form the right tail
            // and separators; two more preserve a visible leading rule.
            if width >= label.len() + 6 {
                return Line::from(vec![
                    Span::raw(format!("{} ", "─".repeat(width - label.len() - 4))),
                    Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Indexed(103))
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::raw(" ──"),
                ]);
            }
        }
    }
    Line::raw("─".repeat(width))
}

fn format_count(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    if count < 1_000_000 {
        return format_scaled(count, 1_000, "k");
    }
    format_scaled(count, 1_000_000, "M")
}

fn format_scaled(count: u64, unit: u64, suffix: &str) -> String {
    let tenths = (count as u128 * 10 + unit as u128 / 2) / unit as u128;
    let whole = tenths / 10;
    let fraction = tenths % 10;
    if fraction == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{fraction}{suffix}")
    }
}

fn format_percent(part: u64, whole: u64) -> Option<String> {
    if whole == 0 {
        return None;
    }
    let tenths = (part as u128 * 1000 + whole as u128 / 2) / whole as u128;
    let whole_percent = tenths / 10;
    let fraction = tenths % 10;
    if fraction == 0 {
        Some(whole_percent.to_string())
    } else {
        Some(format!("{whole_percent}.{fraction}"))
    }
}

/// Powerlevel10k-style platform, directory, and optional git label.
/// Shorten from the left when narrow so the branch remains visible.
pub(crate) fn render_location_row(
    width: usize,
    os_icon: &str,
    directory: &str,
    git: Option<&GitStatus>,
) -> Line<'static> {
    // Match the operator's P10k palette: path 31, anchor 39, Git 76.
    let path_style = Style::default().fg(Color::Indexed(31));
    let anchor_style = Style::default()
        .fg(Color::Indexed(39))
        .add_modifier(Modifier::BOLD);
    let git_style = Style::default().fg(Color::Indexed(76));
    let (parent, anchor) = directory
        .rfind(std::path::MAIN_SEPARATOR)
        .map(|separator| directory.split_at(separator + 1))
        .unwrap_or(("", directory));
    let mut spans = vec![
        Span::raw(format!("{os_icon} ")),
        Span::styled(format!(" {parent}"), path_style),
        Span::styled(anchor.to_string(), anchor_style),
    ];
    if let Some(git) = git {
        spans.push(Span::raw(" on "));
        spans.push(Span::styled(format!("  {}", git.label), git_style));
        if let Some(changes) = &git.changes {
            for (symbol, count, color) in [
                ("~", changes.conflicted, 196),
                ("+", changes.staged, 178),
                ("!", changes.unstaged, 178),
                ("?", changes.untracked, 39),
            ] {
                if count > 0 {
                    spans.push(Span::styled(
                        format!(" {symbol}{count}"),
                        Style::default().fg(Color::Indexed(color)),
                    ));
                }
            }
        } else {
            spans.push(Span::styled(
                " status unavailable",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }
    if width == 0 {
        return Line::default();
    }
    let length: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if length > width {
        // Trim the source spans, not a flattened string, so visible text keeps
        // its color even when the OS/path prefix has been clipped away.
        let mut skip = length - (width - 1);
        for span in &mut spans {
            let count = span.content.chars().count();
            let removed = skip.min(count);
            span.content = span
                .content
                .chars()
                .skip(removed)
                .collect::<String>()
                .into();
            skip -= removed;
        }
        spans.insert(0, Span::raw("…"));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_location_keeps_the_visible_branch_green() {
        let git = GitStatus {
            label: "main".to_string(),
            changes: Some(super::super::location::GitChanges::default()),
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(render_location_row(
                        12,
                        "",
                        "~/Work/gh/ahi",
                        Some(&git),
                    )),
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = (0..12).map(|x| buffer[(x, 0)].symbol()).collect();
        assert_eq!(text, "…on   main");
        for x in 4..12 {
            assert_eq!(buffer[(x, 0)].fg, Color::Indexed(76));
            assert!(!buffer[(x, 0)].modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn git_markers_use_p10k_order_colors_and_explicit_unavailable_status() {
        use super::super::location::GitChanges;
        let mut git = GitStatus {
            label: "main".to_string(),
            changes: Some(GitChanges {
                conflicted: 1,
                staged: 2,
                unstaged: 6,
                untracked: 3,
            }),
        };
        let row = render_location_row(100, "", "/tmp/ahi", Some(&git));
        assert_eq!(row.to_string(), "  /tmp/ahi on   main ~1 +2 !6 ?3");
        for (marker, color) in [(" ~1", 196), (" +2", 178), (" !6", 178), (" ?3", 39)] {
            let span = row
                .spans
                .iter()
                .find(|span| span.content == marker)
                .unwrap();
            assert_eq!(span.style.fg, Some(Color::Indexed(color)));
        }
        git.changes = Some(GitChanges::default());
        assert_eq!(
            render_location_row(100, "", "/tmp/ahi", Some(&git)).to_string(),
            "  /tmp/ahi on   main"
        );
        git.changes = None;
        assert_eq!(
            render_location_row(100, "", "/tmp/ahi", Some(&git)).to_string(),
            "  /tmp/ahi on   main status unavailable"
        );
    }

    fn reference_meters() -> SessionMeters {
        SessionMeters {
            input_tokens: Some(139_000),
            output_tokens: Some(11_000),
            cached_input_tokens: Some(1_200_000),
            cost_usd: Some(0.502),
            subscription_sign_in: true,
            context_tokens: Some(77_248),
            context_window: Some(272_000),
            automatic_compaction: true,
        }
    }

    #[test]
    fn input_rule_embeds_context_and_compacts_without_wrapping() {
        let meters = reference_meters();
        assert_eq!(
            render_context_rule(24, &meters).to_string(),
            "──────── 28.4% / 272k ──"
        );
        assert_eq!(
            render_context_rule(17, &meters).to_string(),
            "──────── 28.4% ──"
        );
        assert_eq!(render_context_rule(10, &meters).to_string(), "──────────");
        assert_eq!(render_context_rule(0, &meters).to_string(), "");
    }

    #[test]
    fn input_rule_omits_unknown_context_but_shows_known_zero() {
        let mut meters = SessionMeters::default();
        assert_eq!(
            render_context_rule(15, &meters).to_string(),
            "───────────────"
        );
        meters.context_window = Some(128_000);
        assert_eq!(
            render_context_rule(15, &meters).to_string(),
            "───────────────"
        );
        meters.context_tokens = Some(0);
        assert_eq!(
            render_context_rule(15, &meters).to_string(),
            "── 0% / 128k ──"
        );
        meters.context_window = None;
        assert_eq!(
            render_context_rule(15, &meters).to_string(),
            "───────────────"
        );
        meters.context_window = Some(0);
        assert_eq!(
            render_context_rule(15, &meters).to_string(),
            "───────────────"
        );
    }
}
