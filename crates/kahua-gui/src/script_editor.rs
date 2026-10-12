//! A Luau code editor with syntax highlighting and line numbers.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::{self, Color32, FontId, Galley, RichText, TextFormat, Ui, text::LayoutJob};
use kahua_core::Document;
use kahua_core::kahua_io::rbx_dom_weak::types::{Ref, Variant};

use crate::theme;

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

/// Splits Luau source into colored spans.
pub fn highlight(code: &str, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let bytes = code.as_bytes();
    let mut i = 0;
    let push = |job: &mut LayoutJob, text: &str, color: Color32| {
        job.append(text, 0.0, TextFormat { font_id: font.clone(), color, ..Default::default() });
    };
    while i < bytes.len() {
        let rest = &code[i..];
        let c = bytes[i];
        // Comments (line and --[[ block ]] / --[==[ ]==]).
        if let Some(after) = rest.strip_prefix("--") {
            let end = if let Some(level) = long_bracket_level(after) {
                let close = format!("]{}]", "=".repeat(level));
                after.find(&close).map(|p| 2 + p + close.len()).unwrap_or(rest.len())
            } else {
                rest.find('\n').unwrap_or(rest.len())
            };
            push(&mut job, &rest[..end], C_COMMENT);
            i += end;
            continue;
        }
        // Long strings [[ ]] / [=[ ]=].
        if c == b'['
            && let Some(level) = long_bracket_level(rest)
        {
            let close = format!("]{}]", "=".repeat(level));
            let end = rest.find(&close).map(|p| p + close.len()).unwrap_or(rest.len());
            push(&mut job, &rest[..end], C_STRING);
            i += end;
            continue;
        }
        // Quoted strings, including `interpolated` ones.
        if c == b'"' || c == b'\'' || c == b'`' {
            let mut j = 1;
            while j < rest.len() {
                let b = rest.as_bytes()[j];
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
            push(&mut job, &rest[..j], C_STRING);
            i += j;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && rest.as_bytes().get(1).is_some_and(|b| b.is_ascii_digit())) {
            let end = rest
                .char_indices()
                .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '_'))
                .map(|(p, _)| p)
                .unwrap_or(rest.len());
            push(&mut job, &rest[..end], C_NUMBER);
            i += end;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let end = rest
                .char_indices()
                .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
                .map(|(p, _)| p)
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let next = rest[end..].trim_start().chars().next();
            let color = if KEYWORDS.contains(&word) {
                C_KEYWORD
            } else if BUILTINS.contains(&word) {
                C_BUILTIN
            } else if next == Some('(') {
                C_FUNCTION
            } else {
                C_TEXT
            };
            push(&mut job, word, color);
            i += end;
            continue;
        }
        let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
        let color = if c.is_ascii_punctuation() { C_OPERATOR } else { C_TEXT };
        push(&mut job, &rest[..ch_len], color);
        i += ch_len;
    }
    job
}

/// `[[` -> Some(0), `[==[` -> Some(2), anything else -> None.
fn long_bracket_level(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('[')?;
    let level = rest.chars().take_while(|c| *c == '=').count();
    rest[level..].starts_with('[').then_some(level)
}

/// Shows the editor for one script. Edits go straight into the document.
pub fn script_ui(ui: &mut Ui, doc: &mut Document, script: Ref) {
    let Some(inst) = doc.dom.get_by_ref(script) else {
        ui.label("This script was deleted.");
        return;
    };
    let mut source = match inst.properties.get(&"Source".into()) {
        Some(Variant::String(s)) => s.clone(),
        _ => String::new(),
    };
    let path = doc.dom.full_path_of(script, ".");
    let lines = source.lines().count().max(1) + usize::from(source.ends_with('\n'));

    ui.horizontal(|ui| {
        ui.label(RichText::new(&path).color(theme::TEXT_WEAK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{lines} lines")).small().color(theme::TEXT_WEAK));
        });
    });

    let font = FontId::monospace(13.5);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| -> Arc<Galley> {
        let mut job = highlight(buf.as_str(), font.clone());
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("script_scroll", script)).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let numbers: String = (1..=lines).map(|n| format!("{n}\n")).collect();
                ui.add(
                    egui::Label::new(
                        RichText::new(numbers).font(FontId::monospace(13.5)).color(Color32::from_gray(90)),
                    )
                    .selectable(false),
                );
                let response = ui.add(
                    egui::TextEdit::multiline(&mut source)
                        .id(egui::Id::new(("script_edit", script)))
                        .code_editor()
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(lines.max(30))
                        .lock_focus(true)
                        .layouter(&mut layouter),
                );
                if response.changed() {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    ("Source", script).hash(&mut h);
                    let edits =
                        doc.set_property(script, "Source", Some(Variant::String(source.clone()))).into_iter().collect();
                    doc.perform_coalesced("Edit Script", edits, h.finish());
                }
            });
        });
    });
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
}
