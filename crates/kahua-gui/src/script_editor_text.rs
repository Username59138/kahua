//! Pure text operations behind the script editor.
//!
//! Public positions are character indices, the unit egui uses for cursors. The
//! helpers work on byte offsets internally, and the functions that change text
//! return an [`Edit`] with the new text and the selection to show afterwards.

use std::collections::HashMap;
use std::ops::Range;

use super::highlight::{Kind, tokenize};

/// Spaces that one indent level removes when a line has no tab to remove.
const INDENT_WIDTH: usize = 4;

/// Words that, typed alone at the start of a line, move that line back one level.
const CLOSERS: &[&str] = &["end", "else", "elseif", "until", "}"];

/// New text and the selection to show afterwards (character indices, `.0 <= .1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub sel: (usize, usize),
}

/// Options for [`find_matches`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

/// A change at a byte offset of the original text. Changes are sorted by `at` and do not overlap.
#[derive(Clone, Debug)]
struct ByteChange {
    at: usize,
    remove: usize,
    insert: String,
}

pub fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(b, _)| b)
}

pub fn byte_to_char(s: &str, bi: usize) -> usize {
    let mut bi = bi.min(s.len());
    while !s.is_char_boundary(bi) {
        bi -= 1;
    }
    s[..bi].chars().count()
}

/// Orders a selection so the smaller index comes first.
pub fn sorted(sel: (usize, usize)) -> (usize, usize) {
    (sel.0.min(sel.1), sel.0.max(sel.1))
}

/// Number of lines, counting a trailing newline as starting a new (empty) line.
pub fn line_count(s: &str) -> usize {
    s.matches('\n').count() + 1
}

/// One-based line and column of a character index.
pub fn line_col(s: &str, ci: usize) -> (usize, usize) {
    let before = &s[..char_to_byte(s, ci)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

/// Character index where the one-based `line` starts. Lines past the end go to the last line.
pub fn line_start_char(s: &str, line: usize) -> usize {
    let mut byte = 0;
    for _ in 1..line.max(1) {
        match s[byte..].find('\n') {
            Some(p) => byte += p + 1,
            None => break,
        }
    }
    byte_to_char(s, byte)
}

/// Byte range of the line that contains byte `at`, without its newline.
fn line_at(s: &str, at: usize) -> Range<usize> {
    let start = s[..at].rfind('\n').map_or(0, |p| p + 1);
    let end = s[at..].find('\n').map_or(s.len(), |p| at + p);
    start..end
}

/// Byte ranges of the lines a selection touches (byte offsets `a..=b`).
///
/// A selection that ends exactly at the start of a line does not include that line.
fn selected_lines(s: &str, a: usize, b: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = line_at(s, a).start;
    loop {
        let end = s[start..].find('\n').map_or(s.len(), |p| start + p);
        out.push(start..end);
        if end == s.len() {
            break;
        }
        let next = end + 1;
        if next > b || (next == b && b > a) {
            break;
        }
        start = next;
    }
    out
}

/// Maps a byte offset of the original text to the same place after `changes`.
fn map_pos(p: usize, changes: &[ByteChange]) -> usize {
    let mut delta: isize = 0;
    for c in changes {
        if c.at < p {
            delta -= c.remove.min(p - c.at) as isize;
            delta += c.insert.len() as isize;
        } else if c.at == p {
            // Text inserted at the caret goes before the caret.
            delta += c.insert.len() as isize;
        }
    }
    (p as isize + delta).max(0) as usize
}

/// Applies byte changes to `s` and maps the selection (character indices) onto the result.
fn apply_changes(s: &str, sel: (usize, usize), changes: &[ByteChange]) -> Edit {
    let mut text = String::with_capacity(s.len());
    let mut last = 0;
    for c in changes {
        text.push_str(&s[last..c.at]);
        text.push_str(&c.insert);
        last = c.at + c.remove;
    }
    text.push_str(&s[last..]);
    let (lo, hi) = sorted(sel);
    let a = map_pos(char_to_byte(s, lo), changes);
    let b = map_pos(char_to_byte(s, hi), changes);
    Edit { sel: (byte_to_char(&text, a), byte_to_char(&text, b)), text }
}

/// Newline at the selection. The new line keeps the indentation of the current line,
/// plus one tab after a line that opens a block.
pub fn newline(s: &str, sel: (usize, usize)) -> Edit {
    let (lo, hi) = sorted(sel);
    let (a, b) = (char_to_byte(s, lo), char_to_byte(s, hi));
    let line = line_at(s, a);
    let prefix = &s[line.start..a];
    let indent_len = s[line.clone()].bytes().take_while(|&c| c == b' ' || c == b'\t').count();
    let mut insert = String::from("\n");
    insert.push_str(&s[line.start..line.start + indent_len.min(prefix.len())]);
    if opens_block(prefix) {
        insert.push('\t');
    }
    apply_changes(s, sel, &[ByteChange { at: a, remove: b - a, insert }])
}

/// Moves a line back one level when a closing word (`end`, `else`, `elseif`, `until`, `}`)
/// has just been typed at its start. `caret` is the character index right after the word.
pub fn dedent_after_typing(s: &str, caret: usize) -> Option<Edit> {
    let cb = char_to_byte(s, caret);
    let line = line_at(s, cb);
    let prefix = &s[line.start..cb];
    let word = prefix.trim_start_matches([' ', '\t']);
    if !CLOSERS.contains(&word) {
        return None;
    }
    let indent = &prefix[..prefix.len() - word.len()];
    let remove = outdent_unit_at_end(indent);
    if remove == 0 {
        return None;
    }
    let at = line.start + indent.len() - remove;
    Some(apply_changes(s, (caret, caret), &[ByteChange { at, remove, insert: String::new() }]))
}

/// Bytes to remove from the end of an indentation to take off one level.
fn outdent_unit_at_end(indent: &str) -> usize {
    if indent.ends_with('\t') { 1 } else { indent.bytes().rev().take(INDENT_WIDTH).take_while(|&b| b == b' ').count() }
}

/// Bytes to remove from the start of a line to take off one level.
fn outdent_unit_at_start(line: &str) -> usize {
    if line.starts_with('\t') { 1 } else { line.bytes().take(INDENT_WIDTH).take_while(|&b| b == b' ').count() }
}

/// Tab: inserts a tab at the caret, or indents the selected lines.
pub fn tab(s: &str, sel: (usize, usize)) -> Edit {
    let (lo, hi) = sorted(sel);
    if lo == hi {
        let at = char_to_byte(s, lo);
        return apply_changes(s, sel, &[ByteChange { at, remove: 0, insert: "\t".into() }]);
    }
    indent(s, sel)
}

/// Adds one tab to the start of each selected line.
pub fn indent(s: &str, sel: (usize, usize)) -> Edit {
    let (lo, hi) = sorted(sel);
    let lines = selected_lines(s, char_to_byte(s, lo), char_to_byte(s, hi));
    let changes: Vec<ByteChange> =
        lines.into_iter().map(|l| ByteChange { at: l.start, remove: 0, insert: "\t".into() }).collect();
    apply_changes(s, sel, &changes)
}

/// Removes one level of indentation from each selected line that has one.
pub fn unindent(s: &str, sel: (usize, usize)) -> Edit {
    let (lo, hi) = sorted(sel);
    let lines = selected_lines(s, char_to_byte(s, lo), char_to_byte(s, hi));
    let changes: Vec<ByteChange> = lines
        .into_iter()
        .filter_map(|l| {
            let remove = outdent_unit_at_start(&s[l.clone()]);
            (remove > 0).then(|| ByteChange { at: l.start, remove, insert: String::new() })
        })
        .collect();
    apply_changes(s, sel, &changes)
}

/// Comments the selected lines with `-- `, or uncomments them when every non-blank
/// selected line is already a comment. Blank lines are left alone.
pub fn toggle_comment(s: &str, sel: (usize, usize)) -> Edit {
    let (lo, hi) = sorted(sel);
    let lines = selected_lines(s, char_to_byte(s, lo), char_to_byte(s, hi));
    let targets: Vec<(usize, &str)> = lines
        .iter()
        .filter_map(|l| {
            let text = &s[l.clone()];
            let body = text.trim_start_matches([' ', '\t']);
            (!body.is_empty()).then(|| (l.start + text.len() - body.len(), body))
        })
        .collect();
    if targets.is_empty() {
        return Edit { text: s.to_owned(), sel: (lo, hi) };
    }
    let all_commented = targets.iter().all(|(_, body)| body.starts_with("--"));
    let changes: Vec<ByteChange> = targets
        .iter()
        .map(|&(at, body)| {
            if all_commented {
                let space = usize::from(body[2..].starts_with(' '));
                ByteChange { at, remove: 2 + space, insert: String::new() }
            } else {
                ByteChange { at, remove: 0, insert: "-- ".into() }
            }
        })
        .collect();
    apply_changes(s, sel, &changes)
}

/// Whether a line (or the text before the caret) opens a block, so the next line goes one level deeper.
pub fn opens_block(line: &str) -> bool {
    block_delta(line) > 0
}

/// Net number of blocks a line opens: `then`, `do`, `function`, `repeat`, a bare `else`,
/// or a trailing `{` or `(`, minus `end` and `until`. A line that closes its own block
/// (`if x then y() end`) nets to zero.
fn block_delta(line: &str) -> i32 {
    let code = code_only(line);
    let code = code.trim_end();
    let words: Vec<&str> = code.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    let mut delta = 0;
    for w in &words {
        match *w {
            "then" | "do" | "function" | "repeat" => delta += 1,
            "end" | "until" => delta -= 1,
            _ => {}
        }
    }
    if words.contains(&"else") && !words.contains(&"then") {
        delta += 1;
    }
    if code.ends_with(['{', '(']) {
        delta += 1;
    }
    delta
}

/// The code of a line with string contents and the trailing comment removed.
fn code_only(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '-' if chars.peek() == Some(&'-') => break,
            '"' | '\'' | '`' => {
                out.push_str("\"\"");
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == c {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// All matches of `query`, as non-overlapping byte ranges in document order.
pub fn find_matches(text: &str, query: &str, opts: FindOptions) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = |s: &str| if opts.case_sensitive { s.to_owned() } else { s.to_ascii_lowercase() };
    let hay = fold(text);
    let needle = fold(query);
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = hay[from..].find(&needle) {
        let start = from + p;
        let end = start + needle.len();
        let whole = !text[..start].chars().next_back().is_some_and(is_word_char)
            && !text[end..].chars().next().is_some_and(is_word_char);
        if !opts.whole_word || whole {
            out.push(start..end);
        }
        from = end;
    }
    out
}

/// Index of the match that is exactly the byte range `a..b`.
pub fn current_match(matches: &[Range<usize>], a: usize, b: usize) -> Option<usize> {
    matches.iter().position(|m| m.start == a && m.end == b)
}

/// Index of the first match starting at or after byte `from`, wrapping to the first match.
pub fn next_match(matches: &[Range<usize>], from: usize) -> Option<usize> {
    (!matches.is_empty()).then(|| matches.iter().position(|m| m.start >= from).unwrap_or(0))
}

/// Index of the last match starting before byte `from`, wrapping to the last match.
pub fn prev_match(matches: &[Range<usize>], from: usize) -> Option<usize> {
    (!matches.is_empty()).then(|| matches.iter().rposition(|m| m.start < from).unwrap_or(matches.len() - 1))
}

/// `text` with each of the (sorted, non-overlapping) `matches` replaced by `with`.
pub fn replace_all(text: &str, matches: &[Range<usize>], with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in matches {
        out.push_str(&text[last..m.start]);
        out.push_str(with);
        last = m.end;
    }
    out.push_str(&text[last..]);
    out
}

/// The bracket next to the caret and its partner, as byte ranges. The caret's left
/// neighbour wins over the right one. Brackets inside strings and comments do not count.
pub fn matching_bracket(text: &str, caret: usize) -> Option<(Range<usize>, Range<usize>)> {
    let cb = char_to_byte(text, caret);
    let before = text[..cb].char_indices().next_back().map(|(i, c)| i..i + c.len_utf8());
    let after = text[cb..].chars().next().map(|c| cb..cb + c.len_utf8());
    let pairs = bracket_pairs(text);
    [before, after].into_iter().flatten().find_map(|r| {
        let partner = *pairs.get(&r.start)?;
        Some((r, partner..partner + 1))
    })
}

/// Maps the byte position of each bracket that has a partner to the partner's position.
fn bracket_pairs(text: &str) -> HashMap<usize, usize> {
    let bytes = text.as_bytes();
    let mut pairs = HashMap::new();
    let mut open: Vec<usize> = Vec::new();
    for token in tokenize(text) {
        if token.kind != Kind::Operator || token.range.len() != 1 {
            continue;
        }
        let at = token.range.start;
        match bytes[at] {
            b'(' | b'[' | b'{' => open.push(at),
            close @ (b')' | b']' | b'}') => {
                let want = match close {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if let Some(o) = open.last().copied().filter(|&o| bytes[o] == want) {
                    open.pop();
                    pairs.insert(o, at);
                    pairs.insert(at, o);
                }
            }
            _ => {}
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: FindOptions = FindOptions { case_sensitive: true, whole_word: false };
    const CI: FindOptions = FindOptions { case_sensitive: false, whole_word: false };
    const WORD: FindOptions = FindOptions { case_sensitive: false, whole_word: true };

    /// Applies an edit given as (text, selection) and returns the result.
    fn run(text: &str, sel: (usize, usize), f: fn(&str, (usize, usize)) -> Edit) -> (String, (usize, usize)) {
        let e = f(text, sel);
        (e.text, e.sel)
    }

    #[test]
    fn char_and_byte_positions_agree_on_multibyte_text() {
        let s = "\u{e9} = \"\u{fc}\"\n\u{2713}x";
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 2), 3);
        assert_eq!(char_to_byte(s, 99), s.len());
        assert_eq!(byte_to_char(s, 3), 2);
        assert_eq!(byte_to_char(s, 1), 0, "byte 1 is inside \u{e9}, so it rounds down");
        assert_eq!(line_col(s, 10), (2, 3));
        assert_eq!(line_count("a\nb\n"), 3);
        assert_eq!(line_count(""), 1);
    }

    #[test]
    fn line_start_clamps_to_the_last_line() {
        let s = "one\ntwo\nthree";
        assert_eq!(line_start_char(s, 1), 0);
        assert_eq!(line_start_char(s, 2), 4);
        assert_eq!(line_start_char(s, 3), 8);
        assert_eq!(line_start_char(s, 50), 8);
        assert_eq!(line_start_char(s, 0), 0);
    }

    #[test]
    fn newline_keeps_indentation() {
        assert_eq!(run("\t\tx = 1", (7, 7), newline), ("\t\tx = 1\n\t\t".into(), (10, 10)));
        assert_eq!(run("x = 1", (5, 5), newline), ("x = 1\n".into(), (6, 6)));
    }

    #[test]
    fn newline_adds_a_level_after_block_openers() {
        for line in [
            "if a then",
            "for i = 1, 2 do",
            "while x do",
            "local function f(a)",
            "repeat",
            "else",
            "elseif b then",
            "t = {",
            "call(",
        ] {
            let caret = line.chars().count();
            let (out, _) = run(line, (caret, caret), newline);
            assert_eq!(out, format!("{line}\n\t"), "after {line:?}");
        }
    }

    #[test]
    fn newline_does_not_add_a_level_for_closed_blocks() {
        for line in [
            "if a then b() end",
            "local f = function() return 1 end",
            "x = 1 -- do this then",
            "print(\"then\")",
            "t = {}",
        ] {
            let caret = line.chars().count();
            let (out, _) = run(line, (caret, caret), newline);
            assert_eq!(out, format!("{line}\n"), "after {line:?}");
        }
    }

    #[test]
    fn newline_indents_inside_a_block_on_the_current_indent() {
        let s = "\tif a then";
        let (out, sel) = run(s, (10, 10), newline);
        assert_eq!(out, "\tif a then\n\t\t");
        assert_eq!(sel, (13, 13));
    }

    #[test]
    fn newline_replaces_the_selection() {
        assert_eq!(run("ab cd", (2, 4), newline), ("ab\nd".into(), (3, 3)));
    }

    #[test]
    fn typing_end_dedents_one_level() {
        let s = "if a then\n\tx()\n\t\tend";
        let caret = s.chars().count();
        let edit = dedent_after_typing(s, caret).unwrap();
        assert_eq!(edit.text, "if a then\n\tx()\n\tend");
        assert_eq!(edit.sel, (caret - 1, caret - 1));
    }

    #[test]
    fn dedent_handles_space_indentation_and_closers() {
        let s = "    }";
        let edit = dedent_after_typing(s, 5).unwrap();
        assert_eq!(edit.text, "}");
        assert_eq!(dedent_after_typing("\telse", 5).unwrap().text, "else");
        assert_eq!(dedent_after_typing("\t\tuntil", 7).unwrap().text, "\tuntil");
        assert_eq!(dedent_after_typing("\te", 2), None, "the word is not finished yet");
        assert_eq!(dedent_after_typing("end", 3), None, "nothing to remove at column 0");
        assert_eq!(dedent_after_typing("\tendx", 5), None);
        assert_eq!(dedent_after_typing("\tx = end", 8), None, "only a line that starts with the word");
        assert_eq!(dedent_after_typing("\tend ", 5), None, "trailing text after the word");
    }

    #[test]
    fn dedent_only_looks_at_the_caret_line() {
        let s = "\t\tx\n\tend";
        let caret = s.chars().count();
        assert_eq!(dedent_after_typing(s, caret).unwrap().text, "\t\tx\nend");
        assert_eq!(dedent_after_typing("\tend\n\t\tx", 4).unwrap().text, "end\n\t\tx");
    }

    #[test]
    fn tab_inserts_at_the_caret_without_a_selection() {
        assert_eq!(run("ab", (1, 1), tab), ("a\tb".into(), (2, 2)));
    }

    #[test]
    fn tab_indents_selected_lines() {
        let s = "a\nb\nc";
        // Select from the middle of line one to the middle of line two.
        assert_eq!(run(s, (1, 3), tab), ("\ta\n\tb\nc".into(), (2, 5)));
    }

    #[test]
    fn selection_ending_at_a_line_start_skips_that_line() {
        let s = "a\nb\nc";
        assert_eq!(run(s, (0, 2), indent).0, "\ta\nb\nc");
    }

    #[test]
    fn unindent_removes_a_tab_or_spaces() {
        assert_eq!(run("\ta\n\t\tb\nc", (0, 8), unindent).0, "a\n\tb\nc");
        assert_eq!(run("      a\n  b\nc", (0, 10), unindent).0, "  a\nb\nc");
        assert_eq!(run("a\n\tb", (0, 4), unindent).0, "a\nb");
    }

    #[test]
    fn unindent_without_selection_only_touches_the_caret_line() {
        assert_eq!(run("\ta\n\tb", (4, 4), unindent).0, "\ta\nb");
    }

    #[test]
    fn toggle_comment_comments_and_uncomments() {
        let commented = run("local a\n\tlocal b", (0, 16), toggle_comment);
        assert_eq!(commented.0, "-- local a\n\t-- local b");
        let back = run(&commented.0, (0, commented.0.chars().count()), toggle_comment);
        assert_eq!(back.0, "local a\n\tlocal b");
    }

    #[test]
    fn toggle_comment_leaves_blank_lines_and_mixed_selections() {
        assert_eq!(run("a\n\nb", (0, 4), toggle_comment).0, "-- a\n\n-- b");
        // A mix of commented and plain lines comments everything.
        assert_eq!(run("-- a\nb", (0, 6), toggle_comment).0, "-- -- a\n-- b");
        assert_eq!(run("  \n", (0, 3), toggle_comment).0, "  \n");
    }

    #[test]
    fn toggle_comment_on_one_line_keeps_the_caret_after_the_marker() {
        let e = toggle_comment("x = 1", (4, 4));
        assert_eq!(e.text, "-- x = 1");
        assert_eq!(e.sel, (7, 7));
    }

    #[test]
    fn toggle_comment_removes_marker_without_space() {
        assert_eq!(run("--x\n-- y", (0, 7), toggle_comment).0, "x\ny");
    }

    #[test]
    fn find_is_case_insensitive_by_default() {
        let text = "Foo foo FOO fo";
        assert_eq!(find_matches(text, "foo", CI), vec![0..3, 4..7, 8..11]);
        assert_eq!(find_matches(text, "foo", CS), vec![4..7]);
        assert!(find_matches(text, "", CI).is_empty());
    }

    #[test]
    fn find_whole_words() {
        let text = "print printer _print print_ x.print";
        let hits: Vec<&str> = find_matches(text, "print", WORD).into_iter().map(|r| &text[r]).collect();
        assert_eq!(hits, vec!["print", "print"]);
        // The dot is not a word character, so x.print matches; underscores are.
        assert_eq!(find_matches(text, "print", FindOptions { whole_word: true, case_sensitive: true }).len(), 2);
    }

    #[test]
    fn find_does_not_overlap() {
        assert_eq!(find_matches("aaaa", "aa", CS), vec![0..2, 2..4]);
    }

    #[test]
    fn find_handles_multibyte_text() {
        let text = "\u{fc} = 1 -- \u{fc}";
        let hits = find_matches(text, "\u{fc}", CS);
        assert_eq!(hits.len(), 2);
        assert!(text.is_char_boundary(hits[1].start) && text.is_char_boundary(hits[1].end));
    }

    #[test]
    fn match_navigation_wraps() {
        let m = vec![2..4, 10..12, 20..22];
        assert_eq!(next_match(&m, 0), Some(0));
        assert_eq!(next_match(&m, 4), Some(1));
        assert_eq!(next_match(&m, 21), Some(0), "wraps to the first");
        assert_eq!(prev_match(&m, 10), Some(0));
        assert_eq!(prev_match(&m, 0), Some(2), "wraps to the last");
        assert_eq!(current_match(&m, 10, 12), Some(1));
        assert_eq!(current_match(&m, 10, 11), None);
        assert_eq!(next_match(&[], 0), None);
        assert_eq!(prev_match(&[], 0), None);
    }

    #[test]
    fn replace_all_rebuilds_the_text() {
        let text = "a1 b a1";
        let m = find_matches(text, "a1", CS);
        assert_eq!(replace_all(text, &m, "x"), "x b x");
        assert_eq!(replace_all(text, &m[..1], "--"), "-- b a1");
        assert_eq!(replace_all(text, &[], "x"), text);
        assert_eq!(replace_all(text, &m, ""), " b ");
    }

    #[test]
    fn bracket_matching_pairs_nested_brackets() {
        let s = "f(a[1], {b})";
        // Caret after "(": the bracket to its left is "(" and matches the last ")".
        let (x, y) = matching_bracket(s, 2).unwrap();
        assert_eq!((&s[x], &s[y.clone()]), ("(", ")"));
        assert_eq!(y.start, s.len() - 1);
        // Caret right after "]": the left neighbour is that "]", which pairs with the "[" before "1".
        let (x, y) = matching_bracket(s, 6).unwrap();
        assert_eq!((&s[x], &s[y]), ("]", "["));
    }

    #[test]
    fn bracket_matching_uses_the_right_neighbour_when_left_is_not_a_bracket() {
        let s = "x{}";
        let (x, y) = matching_bracket(s, 1).unwrap();
        assert_eq!((x, y), (1..2, 2..3));
    }

    #[test]
    fn bracket_matching_ignores_strings_and_comments() {
        let s = "print(\")\") -- ]\nx = [[ } ]]";
        // The ")" in the string is not a bracket, so the "(" pairs with the real ")".
        let (x, y) = matching_bracket(s, 6).unwrap();
        assert_eq!((&s[x], &s[y.clone()]), ("(", ")"));
        let real_close = s.find("\")\")").unwrap() + 3;
        assert_eq!(y.start, real_close);
        assert_eq!(matching_bracket(s, 8), None, "a bracket in a string has no partner");
        assert_eq!(matching_bracket(s, 15), None, "a bracket in a comment has no partner");
    }

    #[test]
    fn bracket_matching_needs_a_partner() {
        assert_eq!(matching_bracket("(a", 0), None);
        assert_eq!(matching_bracket("abc", 1), None);
        assert_eq!(matching_bracket("(]", 1), None, "mismatched kinds do not pair");
        assert_eq!(matching_bracket("", 0), None);
    }
}
