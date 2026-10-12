//! The Luau script editor: syntax highlighting, line numbers, find and replace,
//! go to line, auto-indent, bracket matching and a status bar.
//!
//! Every change is written to the script's `Source` property. Typing shares one
//! undo step per burst; each command (indent, comment, replace) is its own step.

#[path = "script_editor_highlight.rs"]
mod highlight;
#[path = "script_editor_text.rs"]
mod text;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, FontId, Galley, Key, Modifiers, Rect, RichText, Shape, Ui,
    text::{CCursor, CCursorRange},
};
use kahua_core::Document;
use kahua_core::kahua_io::rbx_dom_weak::types::{Ref, Variant};

use crate::icons::{self, Icon};
use crate::theme;
use highlight::{Mark, highlight_with};
use text::{Edit, FindOptions};

const FONT_SIZE: f32 = 13.5;
const MATCH_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xE5, 0xB4, 0x4C, 60);
const CURRENT_MATCH_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xE5, 0xB4, 0x4C, 170);
const BRACKET_BG: Color32 = Color32::from_rgba_unmultiplied_const(0x5A, 0xA6, 0xF0, 110);
const CURRENT_LINE_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 9);

/// Find, replace and go-to state for one script. Kept in egui memory, per script.
#[derive(Clone, Default)]
struct FindState {
    find_open: bool,
    replace_open: bool,
    goto_open: bool,
    query: String,
    replacement: String,
    goto_text: String,
    case_sensitive: bool,
    whole_word: bool,
    /// Short result of the last replace command, such as "Replaced 3".
    message: String,
    /// Character index to scroll the editor to on the next frame.
    scroll_to: Option<usize>,
    /// Counter that makes each command's undo key unique.
    edit_seq: u64,
}

impl FindState {
    fn options(&self) -> FindOptions {
        FindOptions { case_sensitive: self.case_sensitive, whole_word: self.whole_word }
    }
}

/// Which widget of this script has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Edit,
    Find,
    Replace,
    Goto,
}

/// Egui ids of the widgets of one script.
#[derive(Clone, Copy)]
struct Ids {
    script: Ref,
    edit: egui::Id,
    find: egui::Id,
    replace: egui::Id,
    goto: egui::Id,
    state: egui::Id,
}

impl Ids {
    fn new(script: Ref) -> Self {
        Self {
            script,
            edit: egui::Id::new(("script_edit", script)),
            find: egui::Id::new(("script_find", script)),
            replace: egui::Id::new(("script_replace", script)),
            goto: egui::Id::new(("script_goto", script)),
            state: egui::Id::new(("script_editor_state", script)),
        }
    }

    /// Undo key for one command. It never merges with the typing burst before it.
    fn fresh_key(&self, st: &mut FindState) -> u64 {
        st.edit_seq += 1;
        hash_of(("Edit Script", self.script, st.edit_seq))
    }

    /// Undo key for typing. Consecutive keystrokes share it, so they become one step.
    fn typing_key(&self) -> u64 {
        hash_of(("Source", self.script))
    }
}

fn hash_of(v: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
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

    let ctx = ui.ctx().clone();
    let ids = Ids::new(script);
    let mut st = ui.data(|d| d.get_temp::<FindState>(ids.state)).unwrap_or_default();
    // Set when this frame changed the source, to the undo key the change uses.
    let mut write_key: Option<u64> = None;

    // Keys go first, before any widget can take them.
    let focus = focus_of(&ctx, ids);
    if focus != Focus::None {
        handle_keys(&ctx, ids, &mut st, &mut source, &mut write_key, focus);
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new(&path).color(theme::TEXT_WEAK));
    });

    find_bar(ui, &ctx, ids, &mut st, &mut source, &mut write_key);

    let font = FontId::monospace(FONT_SIZE);
    let lines = text::line_count(&source);
    let query = if st.find_open { st.query.clone() } else { String::new() };
    let opts = st.options();
    let sel = selection(&ctx, ids.edit);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| -> Arc<Galley> {
        let text = buf.as_str();
        let marks = decorations(text, &query, opts, sel);
        let mut job = highlight_with(text, font.clone(), &marks);
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let scroll_to = st.scroll_to.take();
    let mut caret = sel.caret;
    let mut typed_change = false;

    egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("script_scroll", script)).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let numbers: String = (1..=lines).map(|n| format!("{n}\n")).collect();
                ui.add(
                    egui::Label::new(
                        RichText::new(numbers).font(FontId::monospace(FONT_SIZE)).color(Color32::from_gray(90)),
                    )
                    .selectable(false),
                );
                // Reserved so the current-line background can be painted under the text.
                let line_shape = ui.painter().add(Shape::Noop);
                let output = egui::TextEdit::multiline(&mut source)
                    .id(ids.edit)
                    .code_editor()
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY)
                    .desired_rows(lines.max(30))
                    .lock_focus(true)
                    .layouter(&mut layouter)
                    .show(ui);

                if let Some(range) = output.cursor_range {
                    let row = output.galley.pos_from_cursor(range.primary).translate(output.galley_pos.to_vec2());
                    let band = Rect::from_x_y_ranges(output.response.rect.x_range(), row.y_range());
                    ui.painter().set(line_shape, Shape::rect_filled(band, 0.0, CURRENT_LINE_BG));
                    caret = range.primary.index.0;
                }

                if let Some(ci) = scroll_to {
                    let ci = ci.min(source.chars().count());
                    let rect = output.galley.pos_from_cursor(CCursor::new(ci)).translate(output.galley_pos.to_vec2());
                    ui.scroll_to_rect(rect, Some(Align::Center));
                }

                if output.response.changed() {
                    typed_change = true;
                    // Typing a closing word at the start of a line moves that line back one level.
                    if typed_text(&ctx)
                        && let Some(edit) = text::dedent_after_typing(&source, caret)
                    {
                        apply_edit(&ctx, ids.edit, &mut source, edit);
                        caret = selection(&ctx, ids.edit).caret;
                    }
                }
            });
        });
    });

    if typed_change {
        write_key.get_or_insert(ids.typing_key());
    }
    if let Some(key) = write_key {
        let edits = doc.set_property(script, "Source", Some(Variant::String(source.clone()))).into_iter().collect();
        doc.perform_coalesced("Edit Script", edits, key);
    }

    let (line, col) = text::line_col(&source, caret);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Ln {line}, Col {col}")).small().color(theme::TEXT_WEAK));
        ui.label(RichText::new(format!("{} lines", text::line_count(&source))).small().color(theme::TEXT_WEAK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("Luau").small().color(theme::TEXT_WEAK));
        });
    });

    ui.data_mut(|d| d.insert_temp(ids.state, st));
}

/// Whether this frame typed text (as opposed to a key or a paste-free command).
fn typed_text(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if !t.is_empty())))
}

fn focus_of(ctx: &egui::Context, ids: Ids) -> Focus {
    ctx.memory(|m| {
        if m.has_focus(ids.edit) {
            Focus::Edit
        } else if m.has_focus(ids.find) {
            Focus::Find
        } else if m.has_focus(ids.replace) {
            Focus::Replace
        } else if m.has_focus(ids.goto) {
            Focus::Goto
        } else {
            Focus::None
        }
    })
}

/// Shortcuts and keys for the editor and its bars. Runs only while one of them has focus.
fn handle_keys(
    ctx: &egui::Context,
    ids: Ids,
    st: &mut FindState,
    source: &mut String,
    write_key: &mut Option<u64>,
    focus: Focus,
) {
    let consume = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    // Modifiers::NONE also matches with Shift held, so the Shift variants come first.
    let shift = ctx.input(|i| i.modifiers.shift);

    if consume(Modifiers::COMMAND, Key::F) {
        open_find(ctx, ids, st, source, false);
        return;
    }
    if consume(Modifiers::COMMAND, Key::H) {
        open_find(ctx, ids, st, source, true);
        return;
    }
    if consume(Modifiers::COMMAND, Key::G) {
        st.goto_open = true;
        ctx.memory_mut(|m| m.request_focus(ids.goto));
        return;
    }
    if (st.find_open || st.goto_open) && consume(Modifiers::NONE, Key::Escape) {
        st.find_open = false;
        st.goto_open = false;
        ctx.memory_mut(|m| m.request_focus(ids.edit));
        return;
    }
    if st.find_open && consume(Modifiers::NONE, Key::F3) {
        navigate(ctx, ids, st, source, !shift);
        return;
    }
    if consume(Modifiers::NONE, Key::Enter) {
        match focus {
            Focus::Find => navigate(ctx, ids, st, source, !shift),
            Focus::Replace => replace_current(ctx, ids, st, source, write_key),
            Focus::Goto => go_to_line(ctx, ids, st, source),
            Focus::Edit => {
                let edit = text::newline(source, selected(ctx, ids));
                apply_edit(ctx, ids.edit, source, edit);
                *write_key = Some(ids.typing_key());
            }
            Focus::None => {}
        }
        return;
    }
    if focus == Focus::Edit {
        let command = if consume(Modifiers::SHIFT, Key::Tab) {
            Some(text::unindent(source, selected(ctx, ids)))
        } else if consume(Modifiers::NONE, Key::Tab) {
            Some(text::tab(source, selected(ctx, ids)))
        } else if consume(Modifiers::COMMAND, Key::Slash) {
            Some(text::toggle_comment(source, selected(ctx, ids)))
        } else {
            None
        };
        if let Some(edit) = command {
            apply_edit(ctx, ids.edit, source, edit);
            *write_key = Some(ids.fresh_key(st));
        }
    }
}

/// The find bar and the go-to-line row, drawn above the editor.
fn find_bar(
    ui: &mut Ui,
    ctx: &egui::Context,
    ids: Ids,
    st: &mut FindState,
    source: &mut String,
    write_key: &mut Option<u64>,
) {
    if !st.find_open && !st.goto_open {
        return;
    }
    egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
        if st.find_open {
            ui.horizontal(|ui| {
                let chevron = if st.replace_open { Icon::ChevronDown } else { Icon::ChevronRight };
                if ui
                    .add(egui::Button::image(icons::small(chevron)).frame(false))
                    .on_hover_text("Show replace")
                    .clicked()
                {
                    st.replace_open = !st.replace_open;
                }
                ui.add(egui::TextEdit::singleline(&mut st.query).id(ids.find).hint_text("Find").desired_width(200.0));
                if ui
                    .add(egui::Button::selectable(st.case_sensitive, icons::small(Icon::CaseSensitive)))
                    .on_hover_text("Match case")
                    .clicked()
                {
                    st.case_sensitive = !st.case_sensitive;
                    st.message.clear();
                }
                if ui
                    .add(egui::Button::selectable(st.whole_word, icons::small(Icon::WholeWord)))
                    .on_hover_text("Whole word")
                    .clicked()
                {
                    st.whole_word = !st.whole_word;
                    st.message.clear();
                }

                let matches = find_in(source, st);
                let sel = selection(ctx, ids.edit);
                let current = text::current_match(&matches, to_byte(source, sel.start), to_byte(source, sel.end));
                let (label, color) = if st.query.is_empty() {
                    (String::new(), theme::TEXT_WEAK)
                } else if matches.is_empty() {
                    ("No results".to_owned(), theme::WARN)
                } else if let Some(i) = current {
                    (format!("{} of {}", i + 1, matches.len()), theme::TEXT_WEAK)
                } else {
                    (format!("{} matches", matches.len()), theme::TEXT_WEAK)
                };
                ui.label(RichText::new(label).color(color).small());

                let has_matches = !matches.is_empty();
                if ui
                    .add_enabled(has_matches, egui::Button::image(icons::small(Icon::ChevronUp)).frame(false))
                    .on_hover_text("Previous match (Shift+F3)")
                    .clicked()
                {
                    navigate(ctx, ids, st, source, false);
                }
                if ui
                    .add_enabled(has_matches, egui::Button::image(icons::small(Icon::ChevronDown)).frame(false))
                    .on_hover_text("Next match (F3)")
                    .clicked()
                {
                    navigate(ctx, ids, st, source, true);
                }
                if ui.add(egui::Button::image(icons::small(Icon::Close)).frame(false)).on_hover_text("Close").clicked()
                {
                    st.find_open = false;
                    ctx.memory_mut(|m| m.request_focus(ids.edit));
                }
            });
            if st.replace_open {
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut st.replacement)
                            .id(ids.replace)
                            .hint_text("Replace")
                            .desired_width(200.0),
                    );
                    if ui.add(icons::button(Icon::Replace, "Replace")).clicked() {
                        replace_current(ctx, ids, st, source, write_key);
                    }
                    if ui.add(icons::button(Icon::ReplaceAll, "Replace all")).clicked() {
                        replace_all_matches(ctx, ids, st, source, write_key);
                    }
                    ui.label(RichText::new(&st.message).small().color(theme::TEXT_WEAK));
                });
            }
        }
        if st.goto_open {
            ui.horizontal(|ui| {
                ui.label("Go to line");
                ui.add(
                    egui::TextEdit::singleline(&mut st.goto_text).id(ids.goto).hint_text("Line").desired_width(80.0),
                );
                if ui.add(egui::Button::image(icons::small(Icon::Close)).frame(false)).on_hover_text("Close").clicked()
                {
                    st.goto_open = false;
                    ctx.memory_mut(|m| m.request_focus(ids.edit));
                }
            });
        }
    });
}

/// Byte offset of character index `ci`.
fn to_byte(source: &str, ci: usize) -> usize {
    text::char_to_byte(source, ci)
}

/// The matches for the current query, or nothing when the find bar is closed.
fn find_in(source: &str, st: &FindState) -> Vec<Range<usize>> {
    if st.find_open { text::find_matches(source, &st.query, st.options()) } else { Vec::new() }
}

/// The editor's selection as character indices, for a command.
fn selected(ctx: &egui::Context, ids: Ids) -> (usize, usize) {
    let s = selection(ctx, ids.edit);
    (s.start, s.end)
}

/// The editor's selection in character indices.
#[derive(Clone, Copy)]
struct Sel {
    start: usize,
    end: usize,
    caret: usize,
}

fn selection(ctx: &egui::Context, id: egui::Id) -> Sel {
    match egui::TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()) {
        Some(r) => {
            let (a, b) = (r.primary.index.0, r.secondary.index.0);
            Sel { start: a.min(b), end: a.max(b), caret: a }
        }
        None => Sel { start: 0, end: 0, caret: 0 },
    }
}

fn set_selection(ctx: &egui::Context, id: egui::Id, start: usize, end: usize) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(start), CCursor::new(end))));
    egui::TextEdit::store_state(ctx, id, state);
}

/// Replaces the source with an edit's text and moves the editor's selection to the edit's selection.
fn apply_edit(ctx: &egui::Context, id: egui::Id, source: &mut String, edit: Edit) {
    *source = edit.text;
    set_selection(ctx, id, edit.sel.0, edit.sel.1);
}

/// Opens the find bar. Ctrl+F and Ctrl+H both use it. A single-line selection becomes the query.
fn open_find(ctx: &egui::Context, ids: Ids, st: &mut FindState, source: &str, replace: bool) {
    let sel = selection(ctx, ids.edit);
    if sel.start != sel.end {
        let picked = &source[to_byte(source, sel.start)..to_byte(source, sel.end)];
        if !picked.contains('\n') {
            st.query = picked.to_owned();
        }
    }
    st.find_open = true;
    st.replace_open = replace;
    st.message.clear();
    ctx.memory_mut(|m| m.request_focus(ids.find));
    let len = st.query.chars().count();
    set_selection(ctx, ids.find, 0, len);
}

/// Selects a match in the editor and asks for it to be scrolled into view.
fn select_match(ctx: &egui::Context, ids: Ids, st: &mut FindState, source: &str, range: &Range<usize>) {
    let (a, b) = (text::byte_to_char(source, range.start), text::byte_to_char(source, range.end));
    set_selection(ctx, ids.edit, a, b);
    st.scroll_to = Some(a);
}

/// Moves to the next or previous match from the selection.
fn navigate(ctx: &egui::Context, ids: Ids, st: &mut FindState, source: &str, forward: bool) {
    let matches = find_in(source, st);
    let sel = selection(ctx, ids.edit);
    let (a, b) = (to_byte(source, sel.start), to_byte(source, sel.end));
    let index = if forward { text::next_match(&matches, b) } else { text::prev_match(&matches, a) };
    if let Some(i) = index {
        select_match(ctx, ids, st, source, &matches[i]);
    }
}

/// Replaces the selected match and moves on to the next one. Selects the next match
/// when the selection is not a match.
fn replace_current(
    ctx: &egui::Context,
    ids: Ids,
    st: &mut FindState,
    source: &mut String,
    write_key: &mut Option<u64>,
) {
    let matches = find_in(source, st);
    let sel = selection(ctx, ids.edit);
    let (a, b) = (to_byte(source, sel.start), to_byte(source, sel.end));
    let Some(i) = text::current_match(&matches, a, b) else {
        navigate(ctx, ids, st, source, true);
        return;
    };
    let replacement = st.replacement.clone();
    let new = text::replace_all(source.as_str(), &matches[i..=i], &replacement);
    let caret = matches[i].start + replacement.len();
    let c = text::byte_to_char(&new, caret);
    apply_edit(ctx, ids.edit, source, Edit { text: new, sel: (c, c) });
    *write_key = Some(ids.fresh_key(st));
    let next = find_in(source, st);
    if let Some(j) = text::next_match(&next, caret) {
        select_match(ctx, ids, st, source.as_str(), &next[j]);
    }
}

/// Replaces every match as one undo step.
fn replace_all_matches(
    ctx: &egui::Context,
    ids: Ids,
    st: &mut FindState,
    source: &mut String,
    write_key: &mut Option<u64>,
) {
    let matches = find_in(source, st);
    let Some(first) = matches.first().cloned() else {
        st.message = "No matches".to_owned();
        return;
    };
    let replacement = st.replacement.clone();
    let new = text::replace_all(source.as_str(), &matches, &replacement);
    let caret = text::byte_to_char(&new, first.start + replacement.len());
    st.message = format!("Replaced {}", matches.len());
    apply_edit(ctx, ids.edit, source, Edit { text: new, sel: (caret, caret) });
    *write_key = Some(ids.fresh_key(st));
}

/// Moves the caret to the line typed in the go-to field.
fn go_to_line(ctx: &egui::Context, ids: Ids, st: &mut FindState, source: &str) {
    let Ok(line) = st.goto_text.trim().parse::<usize>() else {
        return;
    };
    let ci = text::line_start_char(source, line);
    set_selection(ctx, ids.edit, ci, ci);
    st.scroll_to = Some(ci);
    st.goto_open = false;
    ctx.memory_mut(|m| m.request_focus(ids.edit));
}

/// Marks drawn in the editor: the current match, the bracket pair, and the other matches.
/// Marks are sorted by start, and where they overlap the earlier one wins.
fn decorations(text: &str, query: &str, opts: FindOptions, sel: Sel) -> Vec<Mark> {
    let matches = text::find_matches(text, query, opts);
    let (a, b) = (text::char_to_byte(text, sel.start), text::char_to_byte(text, sel.end));
    let mut marks = Vec::new();
    if let Some(i) = text::current_match(&matches, a, b) {
        marks.push(Mark { range: matches[i].clone(), background: CURRENT_MATCH_BG });
    }
    if let Some((x, y)) = text::matching_bracket(text, sel.caret) {
        marks.push(Mark { range: x, background: BRACKET_BG });
        marks.push(Mark { range: y, background: BRACKET_BG });
    }
    marks.extend(matches.into_iter().map(|range| Mark { range, background: MATCH_BG }));
    marks.sort_by_key(|m| m.range.start);
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorations_put_the_current_match_first() {
        let text = "ab ab";
        let sel = Sel { start: 3, end: 5, caret: 3 };
        let marks = decorations(text, "ab", FindOptions::default(), sel);
        let at_3: Vec<Color32> = marks.iter().filter(|m| m.range.start == 3).map(|m| m.background).collect();
        assert_eq!(at_3[0], CURRENT_MATCH_BG, "the current match wins where marks overlap");
        assert!(marks.windows(2).all(|w| w[0].range.start <= w[1].range.start));
    }

    #[test]
    fn decorations_mark_the_bracket_pair() {
        let marks = decorations("(x)", "", FindOptions::default(), Sel { start: 0, end: 0, caret: 0 });
        let ranges: Vec<Range<usize>> = marks.iter().map(|m| m.range.clone()).collect();
        assert_eq!(ranges, vec![0..1, 2..3]);
    }
}
