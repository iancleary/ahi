//! The startup header: a text wordmark, the version, the model, and the
//! first keys to know. It is a notice, so it never reaches a model and
//! `/clear` removes it like any other row.

/// Two rows of block glyphs spelling "ahi". The formatter recognises a
/// notice that begins with the first row and paints both rows in the accent.
pub(crate) const WORDMARK: [&str; 2] = ["▄▀█ █░█ █", "█▀█ █▀█ █"];

/// Build the header text for the given facts. Below 60 columns the wordmark
/// is dropped so the first message stays on a small screen.
pub(crate) fn text(
    width: usize,
    model: &str,
    gateway: &str,
    thinking: &str,
    directory: &str,
    tools_configured: bool,
) -> String {
    let version = crate::VERSION;
    let model_line = format!("{model} via {gateway} · thinking {thinking}");
    let place_line = format!(
        "{directory} · {}",
        if tools_configured {
            "tools approved by ahi.tools.yaml"
        } else {
            "no ahi.tools.yaml: tools off · `ahi tools init` writes one"
        }
    );
    let keys_line = "/ for commands · Ctrl+S selects · /mouse toggles native selection";
    if width < 60 {
        return format!("ahi {version} · {model_line}\n{place_line}\n{keys_line}");
    }
    let indent = " ".repeat(WORDMARK[0].chars().count() + 3);
    format!(
        "{}   ahi {version}\n{}   {model_line}\n{indent}{place_line}\n{indent}{keys_line}",
        WORDMARK[0], WORDMARK[1]
    )
}

pub(crate) fn is_header(text: &str) -> bool {
    text.starts_with(WORDMARK[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fits_wide_and_narrow_screens() {
        let wide = text(100, "gpt-5.1", "openai", "medium", "~/w/ahi on main", true);
        assert!(is_header(&wide));
        assert_eq!(wide.lines().count(), 4);
        assert!(wide.contains(&format!("ahi {}", crate::VERSION)));
        assert!(wide.contains("gpt-5.1 via openai · thinking medium"));
        let narrow = text(50, "gpt-5.1", "openai", "medium", "~/w/ahi", false);
        assert!(!is_header(&narrow));
        assert_eq!(narrow.lines().count(), 3);
        assert!(narrow.contains("tools off"));
    }
}
