//! The Explorer: the instance tree with selection, rename, drag-and-drop
//! reparenting and a context menu.

use std::collections::HashSet;

use eframe::egui::{self, Color32, CursorIcon, Key, Rect, RichText, Sense, Ui};
use kahua_core::Document;
use kahua_core::kahua_io::rbx_dom_weak::types::Ref;
use kahua_core::kahua_io::reflect;

use crate::icons::{self, Icon};
use crate::ops::INSERTABLE;
use crate::theme;

const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 14.0;

pub enum ExplorerAction {
    Select(Vec<Ref>),
    OpenScript(Ref),
    Insert { parent: Ref, class: String },
    Rename(Ref, String),
    MoveInto { items: Vec<Ref>, parent: Ref },
    Delete,
    Duplicate,
    Copy,
    Paste { into: Option<Ref> },
    Group,
    Ungroup,
    Focus,
}

#[derive(Default)]
pub struct Explorer {
    expanded: HashSet<Ref>,
    filter: String,
    renaming: Option<(Ref, String, bool)>,
    reveal: Option<Ref>,
    dragging: Option<Ref>,
    anchor: Option<Ref>,
    initialized: bool,
}

struct Row {
    r: Ref,
    depth: usize,
    has_children: bool,
}

impl Explorer {
    pub fn reveal(&mut self, r: Ref) {
        self.reveal = Some(r);
    }

    pub fn start_rename(&mut self, doc: &Document) {
        if let Some(&r) = doc.selection.first()
            && let Some(inst) = doc.dom.get_by_ref(r)
        {
            self.renaming = Some((r, inst.name.clone(), true));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, doc: &Document) -> Vec<ExplorerAction> {
        let mut actions = Vec::new();
        if !self.initialized {
            // Start with Workspace open, like Studio.
            for &child in doc.dom.root().children() {
                if doc.dom.get_by_ref(child).is_some_and(|i| i.class == "Workspace") {
                    self.expanded.insert(child);
                }
            }
            self.initialized = true;
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Explorer").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter (name or class)")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(2.0);

        // Expand everything above the instance to reveal.
        let mut scroll_to = None;
        if let Some(target) = self.reveal.take()
            && doc.dom.get_by_ref(target).is_some()
        {
            let mut current = doc.dom.get_by_ref(target).unwrap().parent();
            while current.is_some() {
                self.expanded.insert(current);
                current = doc.dom.get_by_ref(current).map_or(Ref::none(), |i| i.parent());
            }
            scroll_to = Some(target);
        }

        let rows = self.flatten(doc);
        let scroll_index = scroll_to.and_then(|t| rows.iter().position(|row| row.r == t));

        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("explorer_scroll");
        if let Some(i) = scroll_index {
            let visible = ui.available_height();
            area = area.vertical_scroll_offset((i as f32 * ROW_HEIGHT - visible * 0.4).max(0.0));
        }

        let pointer = ui.input(|i| i.pointer.interact_pos());
        let released = ui.input(|i| i.pointer.any_released());
        let mut drop_target: Option<Ref> = None;

        area.show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for row in &rows[range] {
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click_and_drag());
                let Some(inst) = doc.dom.get_by_ref(row.r) else { continue };
                let selected = doc.selection.contains(&row.r);
                let is_drop_target =
                    self.dragging.is_some_and(|d| d != row.r) && pointer.is_some_and(|p| rect.contains(p));
                if is_drop_target {
                    drop_target = Some(row.r);
                }

                let fill = if is_drop_target {
                    theme::ACCENT.gamma_multiply(0.45)
                } else if selected {
                    theme::SELECTION
                } else if response.hovered() {
                    Color32::from_rgb(0x2E, 0x30, 0x36)
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 2.0, fill);

                let x = rect.left() + 4.0 + row.depth as f32 * INDENT;
                let arrow_rect = Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(12.0, ROW_HEIGHT));
                if row.has_children {
                    let open = self.expanded.contains(&row.r) || !self.filter.is_empty();
                    let icon = if open { Icon::ChevronDown } else { Icon::ChevronRight };
                    let chevron = Rect::from_center_size(arrow_rect.center(), egui::vec2(12.0, 12.0));
                    icons::paint(ui, chevron, icon, theme::TEXT_WEAK);
                }
                let icon_rect =
                    Rect::from_min_size(egui::pos2(x + 14.0, rect.center().y - 8.0), egui::vec2(16.0, 16.0));
                icons::paint_class(ui, icon_rect, &inst.class);
                let text_pos = egui::pos2(x + 34.0, rect.center().y);

                let is_renaming = self.renaming.as_ref().is_some_and(|(r, _, _)| *r == row.r);
                if is_renaming {
                    let (_, text, first) = self.renaming.as_mut().unwrap();
                    let edit_rect = Rect::from_min_max(
                        egui::pos2(text_pos.x - 2.0, rect.top() + 1.0),
                        egui::pos2(rect.right() - 4.0, rect.bottom() - 1.0),
                    );
                    let edit = ui.put(edit_rect, egui::TextEdit::singleline(text).margin(egui::vec2(2.0, 0.0)));
                    if *first {
                        edit.request_focus();
                        *first = false;
                    }
                    let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                    if escape {
                        self.renaming = None;
                    } else if enter || edit.lost_focus() {
                        let (r, text, _) = self.renaming.take().unwrap();
                        let text = text.trim().to_owned();
                        if !text.is_empty() {
                            actions.push(ExplorerAction::Rename(r, text));
                        }
                    }
                } else {
                    ui.painter().text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        &inst.name,
                        egui::FontId::proportional(13.5),
                        if selected { Color32::WHITE } else { Color32::from_gray(215) },
                    );
                }

                // Interaction.
                if response.clicked() {
                    let on_arrow = pointer.is_some_and(|p| arrow_rect.contains(p));
                    if on_arrow && row.has_children {
                        self.toggle(row.r);
                    } else {
                        self.click_select(ui, doc, &rows, row.r, &mut actions);
                    }
                }
                if response.double_clicked() {
                    if reflect::is_script(&inst.class) {
                        actions.push(ExplorerAction::OpenScript(row.r));
                    } else if row.has_children {
                        self.toggle(row.r);
                    }
                }
                if response.drag_started_by(egui::PointerButton::Primary) && !doc.is_service(row.r) {
                    self.dragging = Some(row.r);
                    if !selected {
                        actions.push(ExplorerAction::Select(vec![row.r]));
                    }
                }
                response.context_menu(|ui| {
                    if !doc.selection.contains(&row.r) {
                        actions.push(ExplorerAction::Select(vec![row.r]));
                    }
                    self.context_menu(ui, doc, row.r, &mut actions);
                });
            }
        });

        if self.dragging.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if released
            && let Some(dragged) = self.dragging.take()
            && let Some(target) = drop_target
        {
            let mut items: Vec<Ref> =
                if doc.selection.contains(&dragged) { doc.selection_roots() } else { vec![dragged] };
            items.retain(|r| !doc.is_service(*r));
            actions.push(ExplorerAction::MoveInto { items, parent: target });
            self.expanded.insert(target);
        }
        actions
    }

    fn toggle(&mut self, r: Ref) {
        if !self.expanded.remove(&r) {
            self.expanded.insert(r);
        }
    }

    fn click_select(&mut self, ui: &Ui, doc: &Document, rows: &[Row], r: Ref, actions: &mut Vec<ExplorerAction>) {
        let (command, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let mut selection = doc.selection.clone();
        if shift && let Some(anchor) = self.anchor {
            let a = rows.iter().position(|row| row.r == anchor);
            let b = rows.iter().position(|row| row.r == r);
            if let (Some(a), Some(b)) = (a, b) {
                let (lo, hi) = (a.min(b), a.max(b));
                selection = rows[lo..=hi].iter().map(|row| row.r).collect();
            }
        } else if command {
            if let Some(i) = selection.iter().position(|s| *s == r) {
                selection.remove(i);
            } else {
                selection.push(r);
            }
            self.anchor = Some(r);
        } else {
            selection = vec![r];
            self.anchor = Some(r);
        }
        actions.push(ExplorerAction::Select(selection));
    }

    fn context_menu(&mut self, ui: &mut Ui, doc: &Document, r: Ref, actions: &mut Vec<ExplorerAction>) {
        let is_service = doc.is_service(r);
        let class = doc.dom.get_by_ref(r).map(|i| i.class.to_string()).unwrap_or_default();
        if reflect::is_script(&class) && ui.button("Open Script").clicked() {
            actions.push(ExplorerAction::OpenScript(r));
            ui.close();
        }
        ui.menu_button("Insert Object", |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for class in INSERTABLE {
                    ui.horizontal(|ui| {
                        icons::class_icon(ui, class);
                        if ui.button(*class).clicked() {
                            actions.push(ExplorerAction::Insert { parent: r, class: (*class).to_owned() });
                            self.expanded.insert(r);
                            ui.close();
                        }
                    });
                }
            });
        });
        ui.separator();
        if ui.add_enabled(!is_service, egui::Button::new("Rename").shortcut_text("F2")).clicked() {
            self.renaming = doc.dom.get_by_ref(r).map(|i| (r, i.name.clone(), true));
            ui.close();
        }
        for (label, shortcut, enabled, action) in [
            ("Duplicate", "Ctrl+D", !is_service, ExplorerAction::Duplicate),
            ("Copy", "Ctrl+C", !is_service, ExplorerAction::Copy),
            ("Paste Into", "Ctrl+Shift+V", true, ExplorerAction::Paste { into: Some(r) }),
            ("Delete", "Del", !is_service, ExplorerAction::Delete),
        ] {
            if ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        ui.separator();
        if ui.add_enabled(!is_service, egui::Button::new("Group").shortcut_text("Ctrl+G")).clicked() {
            actions.push(ExplorerAction::Group);
            ui.close();
        }
        if ui
            .add_enabled(class == "Model" || class == "Folder", egui::Button::new("Ungroup").shortcut_text("Ctrl+U"))
            .clicked()
        {
            actions.push(ExplorerAction::Ungroup);
            ui.close();
        }
        if ui.add(egui::Button::new("Zoom To").shortcut_text("F")).clicked() {
            actions.push(ExplorerAction::Focus);
            ui.close();
        }
    }

    fn flatten(&self, doc: &Document) -> Vec<Row> {
        let filter = self.filter.trim().to_lowercase();
        let visible: Option<HashSet<Ref>> = (!filter.is_empty()).then(|| {
            let mut set = HashSet::new();
            for inst in doc.dom.descendants() {
                if inst.name.to_lowercase().contains(&filter) || inst.class.to_lowercase().contains(&filter) {
                    let mut current = inst.referent();
                    while current.is_some() && set.insert(current) {
                        current = doc.dom.get_by_ref(current).map_or(Ref::none(), |i| i.parent());
                    }
                }
            }
            set
        });
        let mut rows = Vec::new();
        let mut stack: Vec<(Ref, usize)> = doc.dom.root().children().iter().rev().map(|&r| (r, 0)).collect();
        while let Some((r, depth)) = stack.pop() {
            if visible.as_ref().is_some_and(|v| !v.contains(&r)) {
                continue;
            }
            let Some(inst) = doc.dom.get_by_ref(r) else { continue };
            let has_children = !inst.children().is_empty();
            rows.push(Row { r, depth, has_children });
            let open = visible.is_some() || self.expanded.contains(&r);
            if open {
                for &child in inst.children().iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
        rows
    }
}
