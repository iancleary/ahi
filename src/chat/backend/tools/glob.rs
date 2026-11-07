//! Deterministic glob matching for file names and workspace-relative paths.
//!
//! Syntax: `*` matches within one path component, `**` matches across
//! components, `?` matches one character, `[abc]` and `[a-z]` match a class,
//! `[!abc]` negates it, and `\` escapes the next character. A pattern that
//! contains `/` is anchored to the path it is matched against; a pattern
//! without `/` matches the final component at any depth, as `fd` and
//! `.gitignore` do.

use regex::Regex;

#[derive(Debug, Clone)]
pub(super) struct Glob {
    regex: Regex,
    anchored: bool,
}

impl Glob {
    /// `None` when the pattern is empty, longer than 256 bytes, or malformed.
    pub(super) fn new(pattern: &str) -> Option<Self> {
        if pattern.is_empty() || pattern.len() > 256 {
            return None;
        }
        let anchored = pattern.contains('/');
        let source = pattern.trim_start_matches('/');
        let regex = Regex::new(&format!("^{}$", translate(source)?)).ok()?;
        Some(Self { regex, anchored })
    }

    /// Match a `/`-separated relative path. Unanchored patterns test the
    /// final component only.
    pub(super) fn matches(&self, relative: &str) -> bool {
        if self.anchored {
            self.regex.is_match(relative)
        } else {
            let name = relative.rsplit('/').next().unwrap_or(relative);
            self.regex.is_match(name)
        }
    }
}

fn translate(pattern: &str) -> Option<String> {
    let mut out = String::with_capacity(pattern.len() * 2);
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    // `**/` matches zero or more whole components; a bare
                    // `**` matches anything including `/`.
                    if chars.get(i + 2) == Some(&'/') {
                        out.push_str("(?:[^/]*/)*");
                        i += 3;
                    } else {
                        out.push_str(".*");
                        i += 2;
                    }
                } else {
                    out.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            '[' => {
                let close = chars[i + 1..].iter().position(|c| *c == ']')? + i + 1;
                let mut class = String::from("[");
                let mut j = i + 1;
                if chars.get(j) == Some(&'!') {
                    class.push('^');
                    j += 1;
                }
                for c in &chars[j..close] {
                    match c {
                        '\\' | '^' | '[' | ']' => {
                            class.push('\\');
                            class.push(*c);
                        }
                        other => class.push(*other),
                    }
                }
                class.push(']');
                out.push_str(&class);
                i = close + 1;
            }
            '\\' => {
                let next = *chars.get(i + 1)?;
                out.push_str(&regex::escape(&next.to_string()));
                i += 2;
            }
            other => {
                out.push_str(&regex::escape(&other.to_string()));
                i += 1;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_at_any_depth_and_paths_are_anchored() {
        let rs = Glob::new("*.rs").unwrap();
        assert!(rs.matches("main.rs"));
        assert!(rs.matches("src/deep/main.rs"));
        assert!(!rs.matches("src/main.rs.bak"));
        let under_src = Glob::new("src/**/*.rs").unwrap();
        assert!(under_src.matches("src/main.rs"));
        assert!(under_src.matches("src/a/b/c.rs"));
        assert!(!under_src.matches("tests/main.rs"));
        let one_level = Glob::new("src/*.rs").unwrap();
        assert!(one_level.matches("src/main.rs"));
        assert!(!one_level.matches("src/a/main.rs"));
        let question = Glob::new("f?.txt").unwrap();
        assert!(question.matches("f1.txt"));
        assert!(!question.matches("f10.txt"));
        let class = Glob::new("[a-c]*.md").unwrap();
        assert!(class.matches("b.md"));
        assert!(!class.matches("d.md"));
        let negated = Glob::new("[!.]*").unwrap();
        assert!(negated.matches("visible"));
        assert!(!negated.matches(".hidden"));
        let escaped = Glob::new("a\\*b").unwrap();
        assert!(escaped.matches("a*b"));
        assert!(!escaped.matches("axb"));
        let dot = Glob::new("a.b").unwrap();
        assert!(!dot.matches("axb"));
    }

    #[test]
    fn malformed_and_oversized_patterns_are_refused() {
        assert!(Glob::new("").is_none());
        assert!(Glob::new("[abc").is_none());
        assert!(Glob::new("trailing\\").is_none());
        assert!(Glob::new(&"a".repeat(257)).is_none());
    }
}
