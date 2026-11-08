//! Per-line syntax highlighting for fenced code blocks.
//!
//! One small tokenizer per language, stateless across lines: a block comment
//! or multi-line string opened on an earlier line is not known, so each line
//! is classified on its own. That keeps streamed redraws cheap and the rules
//! short enough to read. Every tokenizer covers its line contiguously, so a
//! renderer styles each range without a fallback pass.
//!
//! Most languages share one scanner driven by a `Spec` of keyword lists and
//! small flags; Markdown, Diff, Typst and YAML have shapes of their own.

use std::ops::Range;

/// The style class of one range of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token {
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Punct,
    Function,
    Variable,
    Attribute,
    Plain,
}

/// Languages with a tokenizer. Anything else is drawn plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    Rust,
    Toml,
    Yaml,
    Json,
    Shell,
    Python,
    TypeScript,
    Markdown,
    Diff,
    Typst,
    PowerShell,
}

/// The language named by a fence info string or a file extension. Only the
/// first word counts, so "rust,ignore" and "ts {1,3}" resolve; case does not.
pub(crate) fn language(tag: &str) -> Option<Language> {
    let first = tag
        .trim()
        .split([' ', '\t', ',', '{', ':'])
        .next()
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    Some(match first.as_str() {
        "rs" | "rust" => Language::Rust,
        "toml" => Language::Toml,
        "yaml" | "yml" => Language::Yaml,
        "json" | "jsonc" | "json5" => Language::Json,
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" => Language::Shell,
        "py" | "python" | "python3" => Language::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "typescript" | "javascript" => {
            Language::TypeScript
        }
        "md" | "markdown" => Language::Markdown,
        "diff" | "patch" => Language::Diff,
        "typ" | "typst" => Language::Typst,
        "ps1" | "psm1" | "psd1" | "pwsh" | "powershell" => Language::PowerShell,
        _ => return None,
    })
}

/// Tokens covering `line` entirely, in order, non-overlapping, as byte ranges
/// on char boundaries. Stateless per line: a block comment or multi-line
/// string that starts on an earlier line is not known; that is accepted.
pub(crate) fn tokens(language: Language, line: &str) -> Vec<(Range<usize>, Token)> {
    match language {
        Language::Rust => scan(&RUST, line),
        Language::Toml => scan(&TOML, line),
        Language::Yaml => yaml(line),
        Language::Json => scan(&JSON, line),
        Language::Shell => scan(&SHELL, line),
        Language::Python => scan(&PYTHON, line),
        Language::TypeScript => scan(&TYPESCRIPT, line),
        Language::Markdown => markdown(line),
        Language::Diff => diff(line),
        Language::Typst => typst(line),
        Language::PowerShell => scan(&POWERSHELL, line),
    }
}

/// A position in one line plus the tokens emitted so far.
struct Cursor<'a> {
    line: &'a str,
    pos: usize,
    out: Vec<(Range<usize>, Token)>,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a str) -> Self {
        Self {
            line,
            pos: 0,
            out: Vec::new(),
        }
    }

    fn rest(&self) -> &'a str {
        &self.line[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    /// Byte length of the longest prefix of the rest whose chars all satisfy `pred`.
    fn span(&self, pred: impl Fn(char) -> bool) -> usize {
        self.rest()
            .chars()
            .take_while(|&c| pred(c))
            .map(char::len_utf8)
            .sum()
    }

    /// Emit `len` bytes as `token`. Plain and Punct runs merge with a preceding
    /// run of the same kind, so a renderer sees one span per style change
    /// rather than one per character.
    fn emit(&mut self, len: usize, token: Token) {
        if len == 0 {
            return;
        }
        let end = self.pos + len;
        match self.out.last_mut() {
            Some((range, last))
                if *last == token
                    && matches!(token, Token::Plain | Token::Punct)
                    && range.end == self.pos =>
            {
                range.end = end;
            }
            _ => self.out.push((self.pos..end, token)),
        }
        self.pos = end;
    }

    fn rest_as(&mut self, token: Token) {
        let len = self.rest().len();
        self.emit(len, token);
    }

    /// Emit `token` from here through the first `close` found after `skip`
    /// bytes (an ASCII opener), or to the end of the line when it never closes.
    fn through(&mut self, skip: usize, close: &str, token: Token) {
        let len = self.line[self.pos + skip..]
            .find(close)
            .map_or(self.rest().len(), |i| skip + i + close.len());
        self.emit(len, token);
    }

    /// A quoted string from the opening `quote` here to its close or the end
    /// of the line. `escape` protects the next char; a tripled quote closes
    /// only on a tripled quote.
    fn quoted(&mut self, quote: char, escape: Option<char>) {
        let triple: std::string::String = std::iter::repeat_n(quote, 3).collect();
        if self.rest().starts_with(triple.as_str()) {
            return self.through(3, &triple, Token::String);
        }
        let mut chars = self.rest().char_indices().skip(1);
        let mut len = self.rest().len();
        while let Some((i, c)) = chars.next() {
            if Some(c) == escape {
                chars.next();
            } else if c == quote {
                len = i + c.len_utf8();
                break;
            }
        }
        self.emit(len, Token::String);
    }

    /// Digits, letters and underscores (so `0xff_u8` is one token), with a
    /// dot only when a digit follows it (so `1..2` keeps its range operator).
    fn number(&mut self) {
        let rest = self.rest();
        let mut len = 0;
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let dot_in_number =
                c == '.' && chars.peek().is_some_and(|(_, next)| next.is_ascii_digit());
            if !(is_ident(c) || dot_in_number) {
                break;
            }
            len = i + c.len_utf8();
        }
        self.emit(len, Token::Number);
    }

    /// Text and kind of the last token that is not whitespace.
    fn previous(&self) -> Option<(&'a str, Token)> {
        self.out
            .iter()
            .rev()
            .map(|(range, token)| (&self.line[range.clone()], *token))
            .find(|(text, _)| !text.trim().is_empty())
    }

    fn finish(self) -> Vec<(Range<usize>, Token)> {
        debug_assert_eq!(self.pos, self.line.len());
        self.out
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

/// Rules for the shared scanner. Each language overrides a few fields of `BASE`.
struct Spec {
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    /// `fn`-like words: the identifier after one is a Function.
    declares: &'static [&'static str],
    /// Line comment openers.
    comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    escape: char,
    /// Single quotes take no escapes (shells).
    literal_single: bool,
    /// Attribute openers: one ending in `[` runs to the closing `]`, any other
    /// takes the identifier after it (`@decorator`).
    attributes: &'static [&'static str],
    /// `$name`, `${...}` and `$?` are variables.
    sigil: bool,
    /// `-flag` is an Attribute.
    dash_flags: bool,
    /// A capitalized identifier is a Type.
    case_types: bool,
    /// `'a` is a lifetime, not an unterminated char literal.
    lifetimes: bool,
    /// An identifier (dots and dashes allowed) followed by this char is a key.
    key_terminator: Option<char>,
    /// A string followed by `:` is a key.
    string_keys: bool,
    /// `[name]` holding only identifier characters is this token.
    bracket: Option<Token>,
    /// `Verb-Noun` identifiers are functions, `$scope:name` is one variable,
    /// and keywords match case-insensitively.
    cmdlets: bool,
}

const BASE: Spec = Spec {
    keywords: &[],
    types: &[],
    declares: &[],
    comments: &[],
    block_comment: None,
    quotes: &['"'],
    escape: '\\',
    literal_single: false,
    attributes: &[],
    sigil: false,
    dash_flags: false,
    case_types: false,
    lifetimes: false,
    key_terminator: None,
    string_keys: false,
    bracket: None,
    cmdlets: false,
};

const RUST: Spec = Spec {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    types: &[
        "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
        "i64", "i128", "isize", "f32", "f64",
    ],
    declares: &["fn"],
    comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    attributes: &["#[", "#!["],
    case_types: true,
    lifetimes: true,
    ..BASE
};

const TOML: Spec = Spec {
    keywords: &["true", "false", "inf", "nan"],
    comments: &["#"],
    quotes: &['"', '\''],
    key_terminator: Some('='),
    bracket: Some(Token::Keyword),
    ..BASE
};

const YAML: Spec = Spec {
    keywords: &["true", "false", "null", "yes", "no", "on", "off"],
    comments: &["#"],
    quotes: &['"', '\''],
    ..BASE
};

const JSON: Spec = Spec {
    keywords: &["true", "false", "null"],
    comments: &["//"],
    string_keys: true,
    ..BASE
};

const SHELL: Spec = Spec {
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
        "in", "function", "select", "time", "return", "local", "export", "readonly", "declare",
        "set", "unset", "source", "alias", "exit",
    ],
    declares: &["function"],
    comments: &["#"],
    quotes: &['"', '\'', '`'],
    literal_single: true,
    sigil: true,
    dash_flags: true,
    ..BASE
};

const PYTHON: Spec = Spec {
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "self", "try", "while", "with", "yield",
    ],
    types: &[
        "int", "float", "str", "bool", "bytes", "list", "dict", "set", "tuple", "object",
    ],
    declares: &["def", "class"],
    comments: &["#"],
    quotes: &['"', '\''],
    attributes: &["@"],
    case_types: true,
    ..BASE
};

const TYPESCRIPT: Spec = Spec {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "any", "boolean", "number", "string", "unknown", "never", "object", "symbol", "bigint",
    ],
    declares: &["function"],
    comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    attributes: &["@"],
    case_types: true,
    ..BASE
};

const POWERSHELL: Spec = Spec {
    keywords: &[
        "begin", "break", "catch", "class", "continue", "do", "else", "elseif", "end", "enum",
        "exit", "filter", "finally", "for", "foreach", "from", "function", "hidden", "if", "in",
        "param", "process", "return", "static", "switch", "throw", "trap", "try", "until", "using",
        "while",
    ],
    declares: &["function", "filter"],
    comments: &["#"],
    block_comment: Some(("<#", "#>")),
    quotes: &['"', '\''],
    escape: '`',
    literal_single: true,
    sigil: true,
    dash_flags: true,
    bracket: Some(Token::Type),
    cmdlets: true,
    ..BASE
};

/// Run the shared scanner over a whole line.
fn scan(spec: &Spec, line: &str) -> Vec<(Range<usize>, Token)> {
    let mut c = Cursor::new(line);
    scan_rest(spec, &mut c);
    c.finish()
}

/// Tokenize from the cursor to the end of its line with `spec`.
fn scan_rest(spec: &Spec, c: &mut Cursor) {
    while let Some(ch) = c.peek() {
        let rest = c.rest();
        let after = |len: usize| rest[len..].trim_start();
        if ch.is_whitespace() {
            c.emit(c.span(char::is_whitespace), Token::Plain);
        } else if let Some(open) = spec.attributes.iter().find(|o| rest.starts_with(*o)) {
            if open.ends_with('[') {
                c.through(open.len(), "]", Token::Attribute);
            } else {
                let name = rest[open.len()..]
                    .chars()
                    .take_while(|&c| is_ident(c) || c == '.')
                    .map(char::len_utf8)
                    .sum::<usize>();
                c.emit(open.len() + name, Token::Attribute);
            }
        } else if spec.comments.iter().any(|p| rest.starts_with(p)) {
            c.rest_as(Token::Comment);
        } else if let Some((open, close)) = spec.block_comment.filter(|(o, _)| rest.starts_with(o))
        {
            c.through(open.len(), close, Token::Comment);
        } else if spec.lifetimes && ch == '\'' && !is_char_literal(rest) {
            let name = rest[1..]
                .chars()
                .take_while(|&c| is_ident(c))
                .map(char::len_utf8)
                .sum::<usize>();
            c.emit(1 + name, Token::Type);
        } else if spec.quotes.contains(&ch) {
            let escape = (ch != '\'' || !spec.literal_single).then_some(spec.escape);
            c.quoted(ch, escape);
            if spec.string_keys && c.rest().trim_start().starts_with(':') {
                if let Some((_, token)) = c.out.last_mut() {
                    *token = Token::Attribute;
                }
            }
        } else if ch.is_ascii_digit() {
            c.number();
        } else if spec.sigil && ch == '$' {
            variable(spec, c);
        } else if spec.dash_flags
            && ch == '-'
            && rest[1..].starts_with(|c: char| c.is_alphabetic() || c == '-')
        {
            c.emit(c.span(|c| is_ident(c) || c == '-'), Token::Attribute);
        } else if let Some(len) = spec.bracket.and(bracket_len(rest)) {
            c.emit(len, spec.bracket.unwrap_or(Token::Punct));
        } else if is_ident_start(ch) {
            let keyed = spec.key_terminator.is_some();
            let mut len = c.span(|c| is_ident(c) || (keyed && matches!(c, '.' | '-')));
            let word = &rest[..len];
            // `Verb-Noun` is one cmdlet name.
            if spec.cmdlets
                && rest[len..].starts_with('-')
                && rest[len + 1..].starts_with(char::is_alphabetic)
            {
                len += 1 + rest[len + 1..]
                    .chars()
                    .take_while(|&c| is_ident(c))
                    .map(char::len_utf8)
                    .sum::<usize>();
                c.emit(len, Token::Function);
                continue;
            }
            let lower = word.to_ascii_lowercase();
            let key = if spec.cmdlets { lower.as_str() } else { word };
            let declared = c.previous().is_some_and(|(text, token)| {
                token == Token::Keyword && spec.declares.contains(&text)
            });
            let token = if spec.keywords.contains(&key) {
                Token::Keyword
            } else if spec.types.contains(&word)
                || (spec.case_types && word.starts_with(char::is_uppercase))
            {
                Token::Type
            } else if declared
                || after(len).starts_with('(')
                || rest[len..].starts_with("!(")
                || rest[len..].starts_with("![")
            {
                Token::Function
            } else if spec
                .key_terminator
                .is_some_and(|k| after(len).starts_with(k))
            {
                Token::Attribute
            } else {
                Token::Plain
            };
            c.emit(len, token);
        } else if ch.is_ascii_punctuation() {
            c.emit(1, Token::Punct);
        } else {
            c.emit(ch.len_utf8(), Token::Plain);
        }
    }
}

/// `'x'` or `'\n'` opens a char literal; `'a` without a close is a lifetime.
fn is_char_literal(rest: &str) -> bool {
    let mut chars = rest.chars().skip(1);
    match chars.next() {
        Some('\\') => true,
        Some(_) => chars.next() == Some('\''),
        None => false,
    }
}

/// `$name`, `${...}`, `$scope:name` (cmdlets), or a one-char special like `$?`.
fn variable(spec: &Spec, c: &mut Cursor) {
    let rest = c.rest();
    if rest.starts_with("${") {
        return c.through(2, "}", Token::Variable);
    }
    let mut len = 1 + rest[1..]
        .chars()
        .take_while(|&c| is_ident(c))
        .map(char::len_utf8)
        .sum::<usize>();
    if len == 1 {
        len += rest[1..]
            .chars()
            .next()
            .filter(|c| c.is_ascii_punctuation())
            .map_or(0, char::len_utf8);
    } else if spec.cmdlets
        && rest[len..].starts_with(':')
        && rest[len + 1..].starts_with(is_ident_start)
    {
        len += 1 + rest[len + 1..]
            .chars()
            .take_while(|&c| is_ident(c))
            .map(char::len_utf8)
            .sum::<usize>();
    }
    c.emit(len, Token::Variable);
}

/// Length of `[name]` or `[[name]]` at the start of `rest` when the name is
/// identifier characters, dots and dashes only; `[0]` and `[a, b]` are not.
fn bracket_len(rest: &str) -> Option<usize> {
    if !rest.starts_with('[') {
        return None;
    }
    let (open, close) = if rest.starts_with("[[") {
        (2, "]]")
    } else {
        (1, "]")
    };
    let end = rest[open..].find(close)?;
    let name = &rest[open..open + end];
    let valid = name.starts_with(is_ident_start)
        && name.chars().all(|c| is_ident(c) || matches!(c, '.' | '-'));
    valid.then_some(open + end + close.len())
}

/// YAML: indentation and `- ` are structure, a `key:` is an Attribute, and the
/// rest of the line is scalars and comments.
fn yaml(line: &str) -> Vec<(Range<usize>, Token)> {
    let mut c = Cursor::new(line);
    c.emit(c.span(char::is_whitespace), Token::Plain);
    while c.rest() == "-" || c.rest().starts_with("- ") {
        c.emit(1, Token::Punct);
        c.emit(c.span(char::is_whitespace), Token::Plain);
    }
    let rest = c.rest();
    // The key runs to the first `:` followed by a space or the end of line,
    // and holds no quote, comment or flow marker.
    let key = rest
        .find(':')
        .filter(|&i| rest[i + 1..].is_empty() || rest[i + 1..].starts_with([' ', '\t']));
    if let Some(i) = key {
        let text = &rest[..i];
        if !text.is_empty() && !text.contains(['"', '\'', '#', '{', '[', '&', '*']) {
            c.emit(i, Token::Attribute);
            c.emit(1, Token::Punct);
        }
    }
    scan_rest(&YAML, &mut c);
    c.finish()
}

/// Markdown: headings and fences are whole-line, markers are punctuation,
/// `**bold**` and `` `code` `` are strings, links and bare URLs are functions.
fn markdown(line: &str) -> Vec<(Range<usize>, Token)> {
    let mut c = Cursor::new(line);
    let trimmed = line.trim_start();
    let hashes = trimmed.trim_start_matches('#');
    if trimmed.starts_with('#') && (hashes.is_empty() || hashes.starts_with([' ', '\t'])) {
        c.rest_as(Token::Keyword);
        return c.finish();
    }
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        c.rest_as(Token::String);
        return c.finish();
    }
    c.emit(c.span(char::is_whitespace), Token::Plain);
    loop {
        let rest = c.rest();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let bullet = rest.starts_with(['-', '*', '+']) && rest[1..].starts_with([' ', '\t']);
        let marker = if rest.starts_with('>') || bullet {
            1
        } else if digits > 0
            && rest[digits..].starts_with(['.', ')'])
            && rest[digits + 1..].starts_with(' ')
        {
            digits + 1
        } else if rest.starts_with("[ ] ") || rest.starts_with("[x] ") || rest.starts_with("[X] ") {
            3
        } else {
            break;
        };
        c.emit(marker, Token::Punct);
        c.emit(c.span(char::is_whitespace), Token::Plain);
    }
    while let Some(ch) = c.peek() {
        let rest = c.rest();
        if ch == '`' {
            match rest[1..].find('`') {
                Some(i) => c.emit(i + 2, Token::String),
                None => c.emit(1, Token::Plain),
            }
        } else if rest.starts_with("**") || rest.starts_with("__") {
            match rest[2..].find(&rest[..2]) {
                Some(i) => c.emit(i + 4, Token::String),
                None => c.emit(2, Token::Plain),
            }
        } else if rest.starts_with('[') || rest.starts_with("![") {
            let link = rest
                .find("](")
                .and_then(|i| rest[i..].find(')').map(|j| i + j + 1));
            match link {
                Some(len) => c.emit(len, Token::Function),
                None => c.emit(1, Token::Plain),
            }
        } else if rest.starts_with("http://") || rest.starts_with("https://") {
            c.emit(c.span(|c| !c.is_whitespace()), Token::Function);
        } else {
            // Plain text up to the next char that could open a construct.
            let len = rest
                .char_indices()
                .skip(1)
                .find(|(_, c)| matches!(c, '`' | '*' | '_' | '[' | '!' | 'h'))
                .map_or(rest.len(), |(i, _)| i);
            c.emit(len, Token::Plain);
        }
    }
    c.finish()
}

/// Unified diff lines by their first column. The diff formatter paints its own
/// backgrounds; this serves fenced ```diff blocks.
fn diff(line: &str) -> Vec<(Range<usize>, Token)> {
    let mut c = Cursor::new(line);
    let token = if ["diff ", "index ", "--- ", "+++ "]
        .iter()
        .any(|p| line.starts_with(p))
        || line == "---"
        || line == "+++"
    {
        Token::Comment
    } else if line.starts_with("@@") {
        Token::Function
    } else if line.starts_with('+') {
        Token::String
    } else if line.starts_with('-') {
        Token::Keyword
    } else {
        Token::Plain
    };
    c.rest_as(token);
    c.finish()
}

const TYPST_KEYWORDS: &[&str] = &[
    "let", "set", "show", "import", "include", "if", "else", "for", "while", "in", "return",
    "break", "continue", "not", "and", "or", "none", "auto", "true", "false",
];

/// Typst markup with embedded code: `#name` calls, `$math$`, `*strong*`,
/// `_emph_`, `= Heading`, `@ref` and `<label>`.
fn typst(line: &str) -> Vec<(Range<usize>, Token)> {
    let mut c = Cursor::new(line);
    let trimmed = line.trim_start();
    if trimmed.starts_with('=') && trimmed.trim_start_matches('=').starts_with(' ') {
        c.rest_as(Token::Keyword);
        return c.finish();
    }
    const STOPS: &str = "/$\"#@<*_[](){},:;=+-";
    while let Some(ch) = c.peek() {
        let rest = c.rest();
        let boundary = c.pos == 0 || !line[..c.pos].ends_with(is_ident);
        if ch.is_whitespace() {
            c.emit(c.span(char::is_whitespace), Token::Plain);
        } else if rest.starts_with("//") {
            c.rest_as(Token::Comment);
        } else if rest.starts_with("/*") {
            c.through(2, "*/", Token::Comment);
        } else if ch == '$' {
            c.through(1, "$", Token::String);
        } else if ch == '"' {
            c.quoted('"', Some('\\'));
        } else if ch == '#' || ch == '@' {
            let name = rest[1..]
                .chars()
                .take_while(|&c| is_ident(c) || c == '.')
                .map(char::len_utf8)
                .sum::<usize>();
            let token = match (ch, name) {
                (_, 0) => Token::Punct,
                ('@', _) => Token::Variable,
                _ if TYPST_KEYWORDS.contains(&rest[1..1 + name].trim_end_matches('.')) => {
                    Token::Keyword
                }
                _ => Token::Function,
            };
            c.emit(1 + name, token);
        } else if ch == '<'
            && rest[1..].find('>').is_some_and(|i| {
                i > 0
                    && rest[1..1 + i]
                        .chars()
                        .all(|c| is_ident(c) || matches!(c, '.' | ':' | '-'))
            })
        {
            c.through(1, ">", Token::Variable);
        } else if (ch == '*' || (ch == '_' && boundary)) && rest[1..].contains(ch) {
            c.through(1, &ch.to_string(), Token::Attribute);
        } else if ch == '_' {
            // An underscore inside a word, or one with no partner, is text.
            c.emit(1, Token::Plain);
        } else if ch.is_ascii_digit() {
            c.number();
        } else if STOPS.contains(ch) {
            c.emit(1, Token::Punct);
        } else {
            let len = c.span(|c| !c.is_whitespace() && !STOPS.contains(c));
            let token = if rest[len..].starts_with('(') {
                Token::Function
            } else {
                Token::Plain
            };
            c.emit(len, token);
        }
    }
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::Token::*;
    use super::*;

    /// Every token list must start at 0, end at the line's end, leave no gap,
    /// never overlap, and cut only at char boundaries.
    fn assert_covers(language: Language, line: &str) -> Vec<(Range<usize>, Token)> {
        let got = tokens(language, line);
        let mut end = 0;
        for (range, _) in &got {
            assert_eq!(
                range.start, end,
                "{language:?} gap/overlap in {line:?}: {got:?}"
            );
            assert!(
                range.end > range.start,
                "{language:?} empty token in {line:?}"
            );
            assert!(
                line.is_char_boundary(range.end),
                "{language:?} split a char in {line:?}"
            );
            end = range.end;
        }
        assert_eq!(
            end,
            line.len(),
            "{language:?} did not cover {line:?}: {got:?}"
        );
        got
    }

    fn check(language: Language, line: &str, expected: &[(&str, Token)]) {
        let got = assert_covers(language, line);
        let texts: Vec<(&str, Token)> = got.iter().map(|(r, t)| (&line[r.clone()], *t)).collect();
        assert_eq!(texts, expected, "{language:?}: {line:?}");
    }

    #[test]
    fn language_tags_resolve_by_first_word_case_insensitively() {
        assert_eq!(language("rust,ignore"), Some(Language::Rust));
        assert_eq!(language("RS"), Some(Language::Rust));
        assert_eq!(language("ts {1,3}"), Some(Language::TypeScript));
        assert_eq!(language(".jsx"), Some(Language::TypeScript));
        assert_eq!(language("zsh"), Some(Language::Shell));
        assert_eq!(language("py"), Some(Language::Python));
        assert_eq!(language("yml"), Some(Language::Yaml));
        assert_eq!(language("pwsh"), Some(Language::PowerShell));
        assert_eq!(language("typ"), Some(Language::Typst));
        assert_eq!(language("patch"), Some(Language::Diff));
        assert_eq!(language("md"), Some(Language::Markdown));
        assert_eq!(language("toml"), Some(Language::Toml));
        assert_eq!(language("jsonc"), Some(Language::Json));
        assert_eq!(language(""), None);
        assert_eq!(language("cobol"), None);
    }

    #[test]
    fn rust_golden() {
        check(
            Language::Rust,
            "pub fn parse<'a>(s: &'a str) -> Vec<u32> { s.len() } // note",
            &[
                ("pub", Keyword),
                (" ", Plain),
                ("fn", Keyword),
                (" ", Plain),
                ("parse", Function),
                ("<", Punct),
                ("'a", Type),
                (">(", Punct),
                ("s", Plain),
                (":", Punct),
                (" ", Plain),
                ("&", Punct),
                ("'a", Type),
                (" ", Plain),
                ("str", Type),
                (")", Punct),
                (" ", Plain),
                ("->", Punct),
                (" ", Plain),
                ("Vec", Type),
                ("<", Punct),
                ("u32", Type),
                (">", Punct),
                (" ", Plain),
                ("{", Punct),
                (" s", Plain),
                (".", Punct),
                ("len", Function),
                ("()", Punct),
                (" ", Plain),
                ("}", Punct),
                (" ", Plain),
                ("// note", Comment),
            ],
        );
        check(
            Language::Rust,
            "#[derive(Debug)] let c = 'x'; println!(\"{c}\"); /* 0.5 */",
            &[
                ("#[derive(Debug)]", Attribute),
                (" ", Plain),
                ("let", Keyword),
                (" c ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("'x'", String),
                (";", Punct),
                (" ", Plain),
                ("println", Function),
                ("!(", Punct),
                ("\"{c}\"", String),
                (");", Punct),
                (" ", Plain),
                ("/* 0.5 */", Comment),
            ],
        );
    }

    #[test]
    fn non_ascii_text_stays_on_char_boundaries() {
        check(
            Language::Rust,
            "let s = \"héllo → 世界\"; // ünïcode ✓",
            &[
                ("let", Keyword),
                (" s ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("\"héllo → 世界\"", String),
                (";", Punct),
                (" ", Plain),
                ("// ünïcode ✓", Comment),
            ],
        );
        check(
            Language::Python,
            "名前 = 'é' → 3",
            &[
                ("名前 ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("'é'", String),
                (" → ", Plain),
                ("3", Number),
            ],
        );
    }

    #[test]
    fn toml_golden() {
        check(Language::Toml, "[[bin]]", &[("[[bin]]", Keyword)]);
        check(
            Language::Toml,
            "serde.version = \"1.0\" # crate",
            &[
                ("serde.version", Attribute),
                (" ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("\"1.0\"", String),
                (" ", Plain),
                ("# crate", Comment),
            ],
        );
        check(
            Language::Toml,
            "debug = true # x",
            &[
                ("debug", Attribute),
                (" ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("true", Keyword),
                (" ", Plain),
                ("# x", Comment),
            ],
        );
    }

    #[test]
    fn yaml_golden() {
        check(
            Language::Yaml,
            "  - name: Build # step",
            &[
                ("  ", Plain),
                ("-", Punct),
                (" ", Plain),
                ("name", Attribute),
                (":", Punct),
                (" Build ", Plain),
                ("# step", Comment),
            ],
        );
        check(
            Language::Yaml,
            "enabled: true",
            &[
                ("enabled", Attribute),
                (":", Punct),
                (" ", Plain),
                ("true", Keyword),
            ],
        );
        check(
            Language::Yaml,
            "- http://x.y",
            &[
                ("-", Punct),
                (" http", Plain),
                ("://", Punct),
                ("x", Plain),
                (".", Punct),
                ("y", Plain),
            ],
        );
    }

    #[test]
    fn json_golden() {
        check(
            Language::Json,
            "  \"port\": 8080, \"ok\": true",
            &[
                ("  ", Plain),
                ("\"port\"", Attribute),
                (":", Punct),
                (" ", Plain),
                ("8080", Number),
                (",", Punct),
                (" ", Plain),
                ("\"ok\"", Attribute),
                (":", Punct),
                (" ", Plain),
                ("true", Keyword),
            ],
        );
    }

    #[test]
    fn shell_golden() {
        check(
            Language::Shell,
            "export PATH=\"${HOME}/bin:$PATH\" # path",
            &[
                ("export", Keyword),
                (" PATH", Plain),
                ("=", Punct),
                ("\"${HOME}/bin:$PATH\"", String),
                (" ", Plain),
                ("# path", Comment),
            ],
        );
        check(
            Language::Shell,
            "ls -la '$dir' || exit 1",
            &[
                ("ls ", Plain),
                ("-la", Attribute),
                (" ", Plain),
                ("'$dir'", String),
                (" ", Plain),
                ("||", Punct),
                (" ", Plain),
                ("exit", Keyword),
                (" ", Plain),
                ("1", Number),
            ],
        );
        check(
            Language::Shell,
            "echo ${name}_$1 $?",
            &[
                ("echo ", Plain),
                ("${name}", Variable),
                ("_", Plain),
                ("$1", Variable),
                (" ", Plain),
                ("$?", Variable),
            ],
        );
    }

    #[test]
    fn python_golden() {
        check(Language::Python, "@dataclass", &[("@dataclass", Attribute)]);
        check(
            Language::Python,
            "def greet(name: str) -> None:  # hi",
            &[
                ("def", Keyword),
                (" ", Plain),
                ("greet", Function),
                ("(", Punct),
                ("name", Plain),
                (":", Punct),
                (" ", Plain),
                ("str", Type),
                (")", Punct),
                (" ", Plain),
                ("->", Punct),
                (" ", Plain),
                ("None", Keyword),
                (":", Punct),
                ("  ", Plain),
                ("# hi", Comment),
            ],
        );
        check(
            Language::Python,
            "x = \"\"\"doc\"\"\"",
            &[
                ("x ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("\"\"\"doc\"\"\"", String),
            ],
        );
    }

    #[test]
    fn typescript_golden() {
        check(
            Language::TypeScript,
            "export async function load(id: number): Promise<User> {",
            &[
                ("export", Keyword),
                (" ", Plain),
                ("async", Keyword),
                (" ", Plain),
                ("function", Keyword),
                (" ", Plain),
                ("load", Function),
                ("(", Punct),
                ("id", Plain),
                (":", Punct),
                (" ", Plain),
                ("number", Type),
                ("):", Punct),
                (" ", Plain),
                ("Promise", Type),
                ("<", Punct),
                ("User", Type),
                (">", Punct),
                (" ", Plain),
                ("{", Punct),
            ],
        );
        check(
            Language::TypeScript,
            "const s = `hi ${x}`; // c",
            &[
                ("const", Keyword),
                (" s ", Plain),
                ("=", Punct),
                (" ", Plain),
                ("`hi ${x}`", String),
                (";", Punct),
                (" ", Plain),
                ("// c", Comment),
            ],
        );
    }

    #[test]
    fn markdown_golden() {
        check(Language::Markdown, "## Title", &[("## Title", Keyword)]);
        check(Language::Markdown, "```rust", &[("```rust", String)]);
        check(
            Language::Markdown,
            "- use **bold** and `code` at [docs](https://x.y) now",
            &[
                ("-", Punct),
                (" use ", Plain),
                ("**bold**", String),
                (" and ", Plain),
                ("`code`", String),
                (" at ", Plain),
                ("[docs](https://x.y)", Function),
                (" now", Plain),
            ],
        );
        check(
            Language::Markdown,
            "> 1. see https://a.b/c *x",
            &[
                (">", Punct),
                (" ", Plain),
                ("1.", Punct),
                (" see ", Plain),
                ("https://a.b/c", Function),
                (" *x", Plain),
            ],
        );
    }

    #[test]
    fn diff_golden() {
        check(
            Language::Diff,
            "@@ -1,2 +1,3 @@",
            &[("@@ -1,2 +1,3 @@", Function)],
        );
        check(Language::Diff, "-old", &[("-old", Keyword)]);
        check(Language::Diff, "+new", &[("+new", String)]);
        check(Language::Diff, "--- a/x", &[("--- a/x", Comment)]);
        check(
            Language::Diff,
            "diff --git a/x b/x",
            &[("diff --git a/x b/x", Comment)],
        );
        check(Language::Diff, " ctx", &[(" ctx", Plain)]);
    }

    #[test]
    fn typst_golden() {
        check(Language::Typst, "= Heading", &[("= Heading", Keyword)]);
        check(
            Language::Typst,
            "#set text(font: \"Inter\") // body",
            &[
                ("#set", Keyword),
                (" ", Plain),
                ("text", Function),
                ("(", Punct),
                ("font", Plain),
                (":", Punct),
                (" ", Plain),
                ("\"Inter\"", String),
                (")", Punct),
                (" ", Plain),
                ("// body", Comment),
            ],
        );
        check(
            Language::Typst,
            "Hello *world* and _em_ with $x^2$ #link(\"u\")[t] @ref <lbl> snake_case",
            &[
                ("Hello ", Plain),
                ("*world*", Attribute),
                (" and ", Plain),
                ("_em_", Attribute),
                (" with ", Plain),
                ("$x^2$", String),
                (" ", Plain),
                ("#link", Function),
                ("(", Punct),
                ("\"u\"", String),
                (")[", Punct),
                ("t", Plain),
                ("]", Punct),
                (" ", Plain),
                ("@ref", Variable),
                (" ", Plain),
                ("<lbl>", Variable),
                (" snake_case", Plain),
            ],
        );
    }

    #[test]
    fn powershell_golden() {
        check(
            Language::PowerShell,
            "function Get-Thing { param([string]$Name) Write-Host \"hi $Name\" -NoNewline } # c",
            &[
                ("function", Keyword),
                (" ", Plain),
                ("Get-Thing", Function),
                (" ", Plain),
                ("{", Punct),
                (" ", Plain),
                ("param", Keyword),
                ("(", Punct),
                ("[string]", Type),
                ("$Name", Variable),
                (")", Punct),
                (" ", Plain),
                ("Write-Host", Function),
                (" ", Plain),
                ("\"hi $Name\"", String),
                (" ", Plain),
                ("-NoNewline", Attribute),
                (" ", Plain),
                ("}", Punct),
                (" ", Plain),
                ("# c", Comment),
            ],
        );
        check(
            Language::PowerShell,
            "<# block #> IF ($env:PATH -eq 'x')",
            &[
                ("<# block #>", Comment),
                (" ", Plain),
                ("IF", Keyword),
                (" ", Plain),
                ("(", Punct),
                ("$env:PATH", Variable),
                (" ", Plain),
                ("-eq", Attribute),
                (" ", Plain),
                ("'x'", String),
                (")", Punct),
            ],
        );
    }

    #[test]
    fn awkward_lines_are_covered_in_every_language() {
        const LANGUAGES: [Language; 11] = [
            Language::Rust,
            Language::Toml,
            Language::Yaml,
            Language::Json,
            Language::Shell,
            Language::Python,
            Language::TypeScript,
            Language::Markdown,
            Language::Diff,
            Language::Typst,
            Language::PowerShell,
        ];
        let lines = [
            "",
            "   ",
            "\"unterminated",
            "'",
            "/* open",
            "<# open",
            "${",
            "#[",
            "[",
            "[[",
            "**",
            "`",
            "$",
            "- ",
            "-",
            ": ",
            "key:",
            "日本語 → ✓ \"é\"",
            "\t-",
            "[[x",
            "#",
            "@",
            "<",
            "$x:",
            "a-",
            "'\\",
            "\"\"\"",
            "1.",
            "1..2",
            "![",
            "[a](",
            "***",
            "_a",
            "= ",
            "#.",
            "é-ñ",
            "0xff_u8",
        ];
        for language in LANGUAGES {
            for line in lines {
                assert_covers(language, line);
            }
        }
    }
}
