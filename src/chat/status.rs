//! Location row and session row text.
//!
//! The screen applies dim and normal styles when it draws these strings.

use super::backend::Selection;
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

/// The session row, with the model segment right-aligned and never truncated.
pub(crate) fn render_session_row(
    width: usize,
    meters: &SessionMeters,
    selection: &Selection,
    thinking: ThinkingLevel,
) -> String {
    let right = session_model_segment(selection, thinking);
    let fields = session_fields(meters);
    if width == 0 {
        return String::new();
    }
    let right_len = right.chars().count();
    if right_len >= width {
        return right;
    }
    let budget = width - right_len - 1;
    let left = fit_prefix(&fields, budget);
    if left.is_empty() {
        let pad = width - right_len;
        format!("{}{right}", " ".repeat(pad))
    } else {
        let pad = width - left.chars().count() - right_len;
        format!("{left}{}{right}", " ".repeat(pad))
    }
}

fn session_fields(meters: &SessionMeters) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(count) = meters.input_tokens {
        fields.push(format!("↑{}", format_count(count)));
    }
    if let Some(count) = meters.output_tokens {
        fields.push(format!("↓{}", format_count(count)));
    }
    if let Some(count) = meters.cached_input_tokens {
        fields.push(format!("R{}", format_count(count)));
    }
    if let Some(cost) = meters.cost_usd {
        let mut field = format!("${cost:.3}");
        if meters.subscription_sign_in {
            field.push_str(" (sub)");
        }
        fields.push(field);
    }
    if let (Some(used), Some(window)) = (meters.context_tokens, meters.context_window) {
        if let Some(percent) = format_percent(used, window) {
            fields.push(format!("{percent}%/{}", format_count(window)));
        }
    }
    if meters.automatic_compaction {
        fields.push("(auto)".to_string());
    }
    fields
}

fn fit_prefix(fields: &[String], budget: usize) -> String {
    let mut end = fields.len();
    while end > 0 {
        let text = fields[..end].join(" ");
        if text.chars().count() <= budget {
            return text;
        }
        end -= 1;
    }
    String::new()
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

/// Directory, with an optional git label, shortened from the left when narrow.
pub(crate) fn render_location_row(
    width: usize,
    directory: &str,
    git_label: Option<&str>,
) -> String {
    let full = match git_label {
        Some(label) if !label.is_empty() => format!("{directory} ({label})"),
        _ => directory.to_string(),
    };
    shorten_from_the_left(&full, width)
}

fn shorten_from_the_left(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut shortened = String::from("…");
    let keep = width - 1;
    shortened.extend(chars[chars.len() - keep..].iter());
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn selection() -> Selection {
        Selection {
            gateway: "openai".to_string(),
            model: "gpt-6.1-sol".to_string(),
        }
    }

    #[test]
    fn session_row_matches_the_reference_order_and_drops_fields() {
        let left = "↑139k ↓11k R1.2M $0.502 (sub) 28.4%/272k (auto)";
        let right = "(openai) gpt-6.1-sol • medium";
        let wide = render_session_row(
            120,
            &reference_meters(),
            &selection(),
            ThinkingLevel::Medium,
        );
        assert_eq!(wide.chars().count(), 120);
        assert!(wide.starts_with(left), "{wide}");
        assert!(wide.ends_with(right), "{wide}");
        assert!(!wide.contains("CH"), "{wide}");
        let mut previous = 0;
        for marker in ["↑", "↓", "R", "$", "(sub)", "28.4%/", "(auto)"] {
            let position = wide
                .find(marker)
                .unwrap_or_else(|| panic!("missing {marker} in {wide}"));
            assert!(position >= previous, "{marker} out of order in {wide}");
            previous = position;
        }

        let narrow =
            render_session_row(60, &reference_meters(), &selection(), ThinkingLevel::Medium);
        assert_eq!(narrow.chars().count(), 60, "{narrow}");
        assert!(narrow.ends_with(right), "{narrow}");
        assert!(narrow.contains("↑139k ↓11k R1.2M $0.502 (sub)"), "{narrow}");
        assert!(!narrow.contains("(auto)"), "{narrow}");
        assert!(!narrow.contains("28.4%"), "{narrow}");
        assert!(!narrow.contains("CH"), "{narrow}");
    }

    #[test]
    fn unknown_usage_fields_are_omitted_and_zero_is_shown() {
        let meters = SessionMeters {
            input_tokens: Some(8),
            output_tokens: Some(2),
            context_tokens: Some(8),
            context_window: Some(128_000),
            ..SessionMeters::default()
        };
        let row = render_session_row(80, &meters, &selection(), ThinkingLevel::Medium);
        assert!(row.contains("↑8 ↓2 0%/128k"), "{row}");
        assert!(!row.contains('R'), "{row}");
        assert!(!row.contains("CH"), "{row}");
        assert!(!row.contains('$'), "{row}");
        assert!(!row.contains("(sub)"), "{row}");
        assert!(!row.contains("(auto)"), "{row}");

        let mut known_zero = meters;
        known_zero.cached_input_tokens = Some(0);
        let row = render_session_row(80, &known_zero, &selection(), ThinkingLevel::Medium);
        assert!(row.contains("R0"), "{row}");
        assert!(!row.contains("CH"), "{row}");
    }
}
