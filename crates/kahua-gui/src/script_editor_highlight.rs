//! Luau tokenizer and syntax-colored layout jobs.

use std::ops::Range;

use eframe::egui::{Color32, FontId, TextFormat, text::LayoutJob};

const KEYWORDS: &[&str] = &[
    "and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for", "function", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "type", "typeof", "until", "while",
];
const BUILTINS: &[&str] = &[
    "game",
    "workspace",
    "script",
    "self",
    "print",
    "warn",
    "error",
    "require",
    "pairs",
    "ipairs",
    "next",
    "select",
    "tostring",
    "tonumber",
    "assert",
    "pcall",
    "xpcall",
    "setmetatable",
    "getmetatable",
    "rawget",
    "rawset",
    "Instance",
    "Vector3",
    "Vector2",
    "CFrame",
    "Color3",
    "BrickColor",
    "UDim",
    "UDim2",
    "Enum",
    "Ray",
    "TweenInfo",
    "NumberRange",
    "NumberSequence",
    "ColorSequence",
    "Region3",
    "task",
    "math",
    "string",
    "table",
    "coroutine",
    "os",
    "debug",
    "utf8",
    "bit32",
    "buffer",
    "tick",
    "wait",
    "spawn",
    "delay",
];

const C_TEXT: Color32 = Color32::from_rgb(0xD4, 0xD4, 0xD4);
const C_KEYWORD: Color32 = Color32::from_rgb(0xF8, 0x6D, 0x7C);
const C_BUILTIN: Color32 = Color32::from_rgb(0x84, 0xD6, 0xF7);
const C_STRING: Color32 = Color32::from_rgb(0xAD, 0xF1, 0x95);
const C_NUMBER: Color32 = Color32::from_rgb(0xFF, 0xC6, 0x00);
const C_COMMENT: Color32 = Color32::from_rgb(0x6A, 0x73, 0x7D);
const C_OPERATOR: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);
const C_FUNCTION: Color32 = Color32::from_rgb(0xFD, 0xFB, 0xAC);

/// What a token is, for coloring and for bracket matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Keyword,
    Builtin,
    Function,
    String,
    Number,
    Comment,
    /// Punctuation, one character per token.
    Operator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Byte range in the source.
    pub range: Range<usize>,
    pub kind: Kind,
}

/// A background color for a byte range of the source.
#[derive(Clone, Debug)]
pub struct Mark {
    pub range: Range<usize>,
    pub background: Color32,
}

/// Splits Luau source into tokens that cover the whole input.
pub fn tokenize(code: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < code.len() {
        let (len, kind) = scan(&code[i..]);
        out.push(Token { range: i..i + len, kind });
        i += len;
    }
    out
}

/// Length in bytes and kind of the token at the start of `rest` (which must not be empty).
fn scan(rest: &str) -> (usize, Kind) {
    let bytes = rest.as_bytes();
    let c = bytes[0];
    // Comments: line and --[[ block ]] / --[==[ ]==].
    if let Some(after) = rest.strip_prefix("--") {
        let end = if let Some(level) = long_bracket_level(after) {
            let close = format!("]{}]", "=".repeat(level));
            after.find(&close).map(|p| 2 + p + close.len()).unwrap_or(rest.len())
        } else {
            rest.find('\n').unwrap_or(rest.len())
        };
        return (end, Kind::Comment);
    }
    // Long strings [[ ]] / [=[ ]=].
    if c == b'['
        && let Some(level) = long_bracket_level(rest)
    {
        let close = format!("]{}]", "=".repeat(level));
        let end = rest.find(&close).map(|p| p + close.len()).unwrap_or(rest.len());
        return (end, Kind::String);
    }
    // Quoted strings, including `interpolated` ones.
    if c == b'"' || c == b'\'' || c == b'`' {
        let mut j = 1;
        while j < bytes.len() {
            let b = bytes[j];
            if b == b'\\' {
                j += 2;
                continue;
            }
            if b == c || b == b'\n' {
                j += 1;
                break;
            }
            j += 1;
        }
        let j = j.min(rest.len());
        return (j, Kind::String);
    }
    if c.is_ascii_digit() || (c == b'.' && bytes.get(1).is_some_and(|b| b.is_ascii_digit())) {
        let end = rest
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '_'))
            .map(|(p, _)| p)
            .unwrap_or(rest.len());
        return (end, Kind::Number);
    }
    if c.is_ascii_alphabetic() || c == b'_' {
        let end = rest
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
            .map(|(p, _)| p)
            .unwrap_or(rest.len());
        let word = &rest[..end];
        let next = rest[end..].trim_start().chars().next();
        let kind = if KEYWORDS.contains(&word) {
            Kind::Keyword
        } else if BUILTINS.contains(&word) {
            Kind::Builtin
        } else if next == Some('(') {
            Kind::Function
        } else {
            Kind::Text
        };
        return (end, kind);
    }
    let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
    let kind = if c.is_ascii_punctuation() { Kind::Operator } else { Kind::Text };
    (ch_len, kind)
}

fn color_of(kind: Kind) -> Color32 {
    match kind {
        Kind::Text => C_TEXT,
        Kind::Keyword => C_KEYWORD,
        Kind::Builtin => C_BUILTIN,
        Kind::Function => C_FUNCTION,
        Kind::String => C_STRING,
        Kind::Number => C_NUMBER,
        Kind::Comment => C_COMMENT,
        Kind::Operator => C_OPERATOR,
    }
}

/// Colors Luau source. Same as [`highlight_with`] without marks.
#[cfg(test)]
pub fn highlight(code: &str, font: FontId) -> LayoutJob {
    highlight_with(code, font, &[])
}

/// Colors Luau source and paints the background of each mark.
///
/// Where marks overlap, the one earlier in `marks` wins. The marks are expected in
/// order of their start, so callers should sort them.
pub fn highlight_with(code: &str, font: FontId, marks: &[Mark]) -> LayoutJob {
    let mut job = LayoutJob::default();
    for token in tokenize(code) {
        let color = color_of(token.kind);
        let mut pos = token.range.start;
        while pos < token.range.end {
            let mut next = token.range.end;
            let mut background = None;
            for mark in marks {
                if mark.range.start <= pos && pos < mark.range.end && background.is_none() {
                    background = Some(mark.background);
                }
                for edge in [mark.range.start, mark.range.end] {
                    if edge > pos && edge < next {
                        next = edge;
                    }
                }
            }
            job.append(
                &code[pos..next],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color,
                    background: background.unwrap_or(Color32::TRANSPARENT),
                    ..Default::default()
                },
            );
            pos = next;
        }
    }
    job
}

/// `[[` -> Some(0), `[==[` -> Some(2), anything else -> None.
fn long_bracket_level(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('[')?;
    let level = rest.chars().take_while(|c| *c == '=').count();
    rest[level..].starts_with('[').then_some(level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(code: &str) -> Vec<(String, Color32)> {
        let job = highlight(code, FontId::monospace(12.0));
        job.sections
            .iter()
            .map(|s| (job.text[s.byte_range.start.0..s.byte_range.end.0].to_owned(), s.format.color))
            .filter(|(t, _)| !t.trim().is_empty())
            .collect()
    }

    #[test]
    fn highlights_luau() {
        let s = spans("local x = \"hi\" -- note\nprint(x, 42)");
        assert_eq!(s[0], ("local".into(), C_KEYWORD));
        assert!(s.contains(&("\"hi\"".into(), C_STRING)));
        assert!(s.contains(&("-- note".into(), C_COMMENT)));
        assert!(s.contains(&("print".into(), C_BUILTIN)));
        assert!(s.contains(&("42".into(), C_NUMBER)));
    }

    #[test]
    fn highlights_long_strings_and_comments() {
        let s = spans("--[[ a\nb ]] local s = [==[x]]y]==]");
        assert_eq!(s[0], ("--[[ a\nb ]]".into(), C_COMMENT));
        assert!(s.contains(&("[==[x]]y]==]".into(), C_STRING)));
    }

    #[test]
    fn tokens_cover_the_whole_input() {
        let code = "local s = \"\u{e9}\\\"x\" -- \u{fc}\nif a then end \u{2713}";
        let tokens = tokenize(code);
        let rebuilt: String = tokens.iter().map(|t| &code[t.range.clone()]).collect();
        assert_eq!(rebuilt, code);
        assert!(tokens.iter().all(|t| code.is_char_boundary(t.range.start) && code.is_char_boundary(t.range.end)));
    }

    #[test]
    fn marks_paint_backgrounds_and_keep_text() {
        let code = "print(a) print(a)";
        let bg = Color32::from_rgb(1, 2, 3);
        let marks = [Mark { range: 6..7, background: bg }, Mark { range: 12..17, background: bg }];
        let job = highlight_with(code, FontId::monospace(12.0), &marks);
        assert_eq!(job.text, code);
        let painted: String = job
            .sections
            .iter()
            .filter(|s| s.format.background == bg)
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(painted, "ant(a)");
    }
}
