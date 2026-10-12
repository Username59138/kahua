//! The editor screen: menus, toolbar, Explorer, Properties, viewport and script tabs.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, RichText, Ui};
use kahua_core::kahua_io::rbx_dom_weak::types::{CFrame, Content, Matrix3, Ref, Vector3};
use kahua_core::{Document, Snapshot, Source};
use kahua_render::glam::Vec3;
use kahua_render::scene::world_root;
use kahua_render::{AssetStore, Scene};

use crate::config::AppConfig;
use crate::explorer::{Explorer, ExplorerAction};
use crate::icons::{self, Icon};
use crate::log::{Level, Log};
use crate::ops::{self, PartKind};
use crate::properties::{Properties, PropertiesAction};
use crate::roblox_ui::{AccountDialog, PublishDialog};
use crate::script_editor;
use crate::theme;
use crate::viewport::{Tool, ViewCtx, Viewport};

pub enum EditorAction {
    Close,
    Open(PathBuf),
    NewProject,
}

struct SaveAsDialog {
    name: String,
    location: String,
    git: bool,
}

pub struct Editor {
    pub doc: Document,
    viewport: Viewport,
    explorer: Explorer,
    properties: Properties,
    tool: Tool,
    tabs: Vec<Ref>,
    active_tab: Option<Ref>,
    clipboard: Vec<Snapshot>,
    scene: Scene,
    index: HashMap<Ref, usize>,
    /// Meshes and textures stored in the project (`kahua://` URIs).
    assets: AssetStore,
    reported_warnings: std::collections::HashSet<String>,
    scene_revision: Option<u64>,
    show_output: bool,
    save_as: Option<SaveAsDialog>,
    about: bool,
    account: Option<AccountDialog>,
    publish: Option<PublishDialog>,
}

const fn shortcut(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}
const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

const SAVE: KeyboardShortcut = shortcut(CMD, Key::S);
const UNDO: KeyboardShortcut = shortcut(CMD, Key::Z);
const REDO: KeyboardShortcut = shortcut(CMD, Key::Y);
const REDO_ALT: KeyboardShortcut = shortcut(CMD_SHIFT, Key::Z);
const DUPLICATE: KeyboardShortcut = shortcut(CMD, Key::D);
const COPY: KeyboardShortcut = shortcut(CMD, Key::C);
const PASTE: KeyboardShortcut = shortcut(CMD, Key::V);
const PASTE_INTO: KeyboardShortcut = shortcut(CMD_SHIFT, Key::V);
const GROUP: KeyboardShortcut = shortcut(CMD, Key::G);
const UNGROUP: KeyboardShortcut = shortcut(CMD, Key::U);
const ROTATE: KeyboardShortcut = shortcut(CMD, Key::R);
const TILT: KeyboardShortcut = shortcut(CMD, Key::T);
const SELECT_TOOL: KeyboardShortcut = shortcut(CMD, Key::Num1);
const MOVE_TOOL: KeyboardShortcut = shortcut(CMD, Key::Num2);
const SCALE_TOOL: KeyboardShortcut = shortcut(CMD, Key::Num3);
const ROTATE_TOOL: KeyboardShortcut = shortcut(CMD, Key::Num4);
const LOCAL_AXES: KeyboardShortcut = shortcut(CMD, Key::L);
const SELECT_ALL: KeyboardShortcut = shortcut(CMD, Key::A);
const CLOSE_TAB: KeyboardShortcut = shortcut(CMD, Key::W);

impl Editor {
    pub fn new(doc: Document) -> Self {
        let mut assets = AssetStore::new(asset_root(&doc));
        if let Some(downloader) = kahua_net::kahua::saved_downloader() {
            assets.set_downloader(downloader);
        }
        Self {
            doc,
            viewport: Viewport::default(),
            explorer: Explorer::default(),
            properties: Properties::default(),
            tool: Tool::Select,
            tabs: Vec::new(),
            active_tab: None,
            clipboard: Vec::new(),
            scene: Scene::default(),
            index: HashMap::new(),
            assets,
            reported_warnings: Default::default(),
            scene_revision: None,
            show_output: true,
            save_as: None,
            about: false,
            account: None,
            publish: None,
        }
    }

    pub fn save(&mut self, log: &mut Log) -> bool {
        match self.doc.save() {
            Ok(warnings) => {
                log.warnings("Save", &warnings);
                log.info(format!("Saved {}", self.doc.path().display()));
                log.toast(Level::Info, "Saved");
                true
            }
            Err(e) => {
                log.error(format!("Save failed: {e:#}"));
                false
            }
        }
    }

    fn refresh_scene(&mut self) -> bool {
        if self.scene_revision == Some(self.doc.revision()) {
            return false;
        }
        self.scene = Scene::extract_with_assets(&self.doc.dom, &mut self.assets);
        self.index = self.scene.parts.iter().enumerate().map(|(i, p)| (p.owner, i)).collect();
        self.scene_revision = Some(self.doc.revision());
        true
    }

    fn snap(&self, config: &AppConfig) -> Option<f32> {
        (config.snap_enabled && config.snap > 0.0).then_some(config.snap)
    }

    fn focus_selection(&mut self) {
        let mut bounds = ops::selection_bounds(&self.doc, &self.scene, &self.index);
        if bounds.is_none() {
            bounds = self.scene.focus_bounds();
        }
        self.viewport.focus(bounds);
    }

    fn open_script(&mut self, r: Ref) {
        if !self.tabs.contains(&r) {
            self.tabs.push(r);
        }
        self.active_tab = Some(r);
    }

    fn insert_parent(&self) -> Ref {
        self.doc
            .selection
            .first()
            .copied()
            .filter(|r| self.doc.dom.get_by_ref(*r).is_some())
            .unwrap_or_else(|| world_root(&self.doc.dom))
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        frame: &eframe::Frame,
        config: &mut AppConfig,
        log: &mut Log,
    ) -> Option<EditorAction> {
        let mut action = None;
        // Meshes and textures downloaded from Roblox arrive in the background.
        if self.assets.poll() {
            self.scene_revision = None;
        }
        if self.assets.pending() > 0 {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        }
        let scene_changed = self.refresh_scene();
        for warning in &self.scene.warnings {
            if self.reported_warnings.insert(warning.clone()) {
                log.warn(warning.clone());
            }
        }
        self.shortcuts(ui, config, log);
        self.tabs.retain(|r| self.doc.dom.get_by_ref(*r).is_some());
        if self.active_tab.is_some_and(|r| !self.tabs.contains(&r)) {
            self.active_tab = None;
        }

        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                if let Some(a) = self.menus(ui, config, log) {
                    action = Some(a);
                }
            });
        });
        egui::Panel::top("toolbar")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(8, 5)))
            .show(ui, |ui| self.toolbar(ui, config, log));

        egui::Panel::right("side")
            .resizable(true)
            .default_size(330.0)
            .size_range(240.0..=700.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| {
                let height = ui.available_height();
                egui::Panel::top("explorer_panel")
                    .resizable(true)
                    .default_size(height * 0.5)
                    .size_range(120.0..=height - 120.0)
                    .frame(egui::Frame::new())
                    .show(ui, |ui| {
                        let actions = self.explorer.ui(ui, &self.doc);
                        self.explorer_actions(actions, log);
                    });
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 6)))
                    .show(ui, |ui| {
                        if let Some(PropertiesAction::OpenScript(r)) = self.properties.ui(ui, &mut self.doc) {
                            self.open_script(r);
                        }
                    });
            });

        if self.show_output {
            egui::Panel::bottom("output")
                .resizable(true)
                .default_size(130.0)
                .size_range(60.0..=500.0)
                .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(6)))
                .show(ui, |ui| log.output_ui(ui));
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::PANEL_DARK)).show(ui, |ui| {
            self.tab_bar(ui);
            match self.active_tab {
                None => {
                    let snap = self.snap(config);
                    let out = self.viewport.ui(
                        ui,
                        frame,
                        ViewCtx {
                            doc: &mut self.doc,
                            scene: &self.scene,
                            index: &self.index,
                            scene_changed,
                            tool: self.tool,
                            snap,
                        },
                    );
                    if let Some(r) = out.picked {
                        self.explorer.reveal(r);
                    }
                }
                Some(script) => {
                    egui::Frame::new().inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                        script_editor::script_ui(ui, &mut self.doc, script);
                    });
                }
            }
        });

        self.dialogs(ui.ctx(), config, log);
        action
    }

    fn shortcuts(&mut self, ui: &mut Ui, config: &AppConfig, log: &mut Log) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        let pressed = |ui: &mut Ui, s: &KeyboardShortcut| ui.input_mut(|i| i.consume_shortcut(s));
        if pressed(ui, &SAVE) {
            self.save(log);
        }
        if pressed(ui, &CLOSE_TAB) && self.active_tab.is_some() {
            let tab = self.active_tab.take().unwrap();
            self.tabs.retain(|r| *r != tab);
        }
        if typing {
            return;
        }
        if pressed(ui, &UNDO) {
            self.doc.undo();
        }
        if pressed(ui, &REDO) || pressed(ui, &REDO_ALT) {
            self.doc.redo();
        }
        if pressed(ui, &DUPLICATE) {
            self.doc.duplicate_selection();
        }
        if pressed(ui, &COPY) {
            self.clipboard = ops::copy(&self.doc);
        }
        if pressed(ui, &PASTE_INTO) {
            let into = self.doc.selection.first().copied();
            ops::paste(&mut self.doc, &self.clipboard, into);
        }
        if pressed(ui, &PASTE) {
            ops::paste(&mut self.doc, &self.clipboard, None);
        }
        if pressed(ui, &GROUP) {
            ops::group(&mut self.doc);
        }
        if pressed(ui, &UNGROUP) {
            ops::ungroup(&mut self.doc);
        }
        if pressed(ui, &ROTATE) {
            ops::rotate_selection(&mut self.doc, &self.scene, &self.index, Vec3::Y, "Rotate");
        }
        if pressed(ui, &TILT) {
            ops::rotate_selection(&mut self.doc, &self.scene, &self.index, Vec3::X, "Tilt");
        }
        if pressed(ui, &SELECT_TOOL) {
            self.tool = Tool::Select;
        }
        if pressed(ui, &MOVE_TOOL) {
            self.tool = Tool::Move;
        }
        if pressed(ui, &SCALE_TOOL) {
            self.tool = Tool::Scale;
        }
        if pressed(ui, &ROTATE_TOOL) {
            self.tool = Tool::Rotate;
        }
        if pressed(ui, &LOCAL_AXES) {
            self.viewport.local_axes = !self.viewport.local_axes;
        }
        if pressed(ui, &SELECT_ALL) {
            ops::select_all(&mut self.doc);
        }
        let (delete, focus, rename) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace),
                i.consume_key(Modifiers::NONE, Key::F),
                i.consume_key(Modifiers::NONE, Key::F2),
            )
        });
        if delete && !self.viewport.is_dragging() {
            self.doc.delete_selection();
        }
        if focus && self.active_tab.is_none() {
            self.focus_selection();
        }
        if rename {
            self.explorer.start_rename(&self.doc);
        }
        let _ = config;
    }

    fn explorer_actions(&mut self, actions: Vec<ExplorerAction>, log: &mut Log) {
        for action in actions {
            match action {
                ExplorerAction::Select(selection) => self.doc.selection = selection,
                ExplorerAction::OpenScript(r) => self.open_script(r),
                ExplorerAction::Insert { parent, class } => {
                    let r = ops::insert_object(&mut self.doc, parent, &class, &self.viewport.camera, &self.scene);
                    self.explorer.reveal(r);
                    if kahua_core::kahua_io::reflect::is_script(&class) {
                        self.open_script(r);
                    }
                }
                ExplorerAction::Rename(r, name) => {
                    if let Some(edit) = self.doc.rename(r, &name) {
                        self.doc.perform("Rename", vec![edit]);
                    }
                }
                ExplorerAction::MoveInto { items, parent } => {
                    let mut edits = Vec::new();
                    for r in items {
                        if let Some(edit) = self.doc.move_to(r, parent) {
                            edit.apply(&mut self.doc.dom);
                            edits.push(edit);
                        } else if r != parent && self.doc.is_ancestor_or_self(r, parent) {
                            log.warn("Can't move an instance into itself or its descendants");
                        }
                    }
                    self.doc.record("Move Into", edits);
                }
                ExplorerAction::Delete => self.doc.delete_selection(),
                ExplorerAction::Duplicate => self.doc.duplicate_selection(),
                ExplorerAction::Copy => self.clipboard = ops::copy(&self.doc),
                ExplorerAction::Paste { into } => ops::paste(&mut self.doc, &self.clipboard, into),
                ExplorerAction::Group => ops::group(&mut self.doc),
                ExplorerAction::Ungroup => ops::ungroup(&mut self.doc),
                ExplorerAction::Focus => {
                    self.active_tab = None;
                    self.focus_selection();
                }
            }
        }
    }

    fn menus(&mut self, ui: &mut Ui, config: &mut AppConfig, log: &mut Log) -> Option<EditorAction> {
        let mut action = None;
        let fmt = |s: &KeyboardShortcut, ui: &Ui| ui.ctx().format_shortcut(s);
        ui.menu_button("File", |ui| {
            if ui.button("New Project…").clicked() {
                action = Some(EditorAction::NewProject);
            }
            if ui.button("Open…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Roblox place or model", &["rbxl", "rbxlx", "rbxm", "rbxmx"])
                    .set_directory(&config.projects_dir)
                    .pick_file()
            {
                action = Some(EditorAction::Open(path));
            }
            if ui.button("Open Project Folder…").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_directory(&config.projects_dir).pick_folder()
            {
                action = Some(EditorAction::Open(dir));
            }
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text(fmt(&SAVE, ui))).clicked() {
                self.save(log);
            }
            if matches!(self.doc.source, Source::File(_)) && ui.button("Save as Project…").clicked() {
                let name = self
                    .doc
                    .path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Place".into());
                self.save_as =
                    Some(SaveAsDialog { name, location: config.projects_dir.display().to_string(), git: true });
            }
            if ui.button("Export as .rbxl…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Roblox place", &["rbxl", "rbxlx"])
                    .add_filter("Roblox model", &["rbxm", "rbxmx"])
                    .set_file_name(format!("{}.rbxl", self.doc.title()))
                    .save_file()
            {
                match self.doc.export(&path) {
                    Ok(()) => log.info(format!("Exported {}", path.display())),
                    Err(e) => log.error(format!("Export failed: {e:#}")),
                }
            }
            ui.separator();
            if ui.add(icons::button(Icon::User, "Roblox Account…")).clicked() {
                self.account = Some(AccountDialog::open());
            }
            let is_project = matches!(self.doc.source, Source::Project(_));
            if ui
                .add_enabled(is_project, icons::button(Icon::Upload, "Publish to Roblox…"))
                .on_disabled_hover_text("Save as a project first")
                .clicked()
            {
                self.publish = Some(PublishDialog::open(self.doc.path()));
            }
            ui.separator();
            if ui.button("Close Project").clicked() {
                action = Some(EditorAction::Close);
            }
        });
        ui.menu_button("Edit", |ui| {
            let undo = self.doc.undo_label().map(|l| format!("Undo {l}")).unwrap_or_else(|| "Undo".into());
            if ui.add_enabled(self.doc.can_undo(), egui::Button::new(undo).shortcut_text(fmt(&UNDO, ui))).clicked() {
                self.doc.undo();
            }
            let redo = self.doc.redo_label().map(|l| format!("Redo {l}")).unwrap_or_else(|| "Redo".into());
            if ui.add_enabled(self.doc.can_redo(), egui::Button::new(redo).shortcut_text(fmt(&REDO, ui))).clicked() {
                self.doc.redo();
            }
            ui.separator();
            let has_sel = !self.doc.selection.is_empty();
            if ui.add_enabled(has_sel, egui::Button::new("Copy").shortcut_text(fmt(&COPY, ui))).clicked() {
                self.clipboard = ops::copy(&self.doc);
            }
            if ui
                .add_enabled(!self.clipboard.is_empty(), egui::Button::new("Paste").shortcut_text(fmt(&PASTE, ui)))
                .clicked()
            {
                ops::paste(&mut self.doc, &self.clipboard, None);
            }
            if ui.add_enabled(has_sel, egui::Button::new("Duplicate").shortcut_text(fmt(&DUPLICATE, ui))).clicked() {
                self.doc.duplicate_selection();
            }
            if ui.add_enabled(has_sel, egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                self.doc.delete_selection();
            }
            ui.separator();
            if ui.add_enabled(has_sel, egui::Button::new("Group").shortcut_text(fmt(&GROUP, ui))).clicked() {
                ops::group(&mut self.doc);
            }
            if ui.add_enabled(has_sel, egui::Button::new("Ungroup").shortcut_text(fmt(&UNGROUP, ui))).clicked() {
                ops::ungroup(&mut self.doc);
            }
            if ui.add_enabled(has_sel, egui::Button::new("Rotate 90°").shortcut_text(fmt(&ROTATE, ui))).clicked() {
                ops::rotate_selection(&mut self.doc, &self.scene, &self.index, Vec3::Y, "Rotate");
            }
            if ui.add_enabled(has_sel, egui::Button::new("Tilt 90°").shortcut_text(fmt(&TILT, ui))).clicked() {
                ops::rotate_selection(&mut self.doc, &self.scene, &self.index, Vec3::X, "Tilt");
            }
        });
        ui.menu_button("Insert", |ui| {
            if ui.add(icons::button(Icon::Cube, "Mesh from File…")).clicked() {
                self.insert_mesh(log);
                ui.close();
            }
            ui.separator();
            for (label, kind) in [
                ("Block", PartKind::Block),
                ("Sphere", PartKind::Sphere),
                ("Cylinder", PartKind::Cylinder),
                ("Wedge", PartKind::Wedge),
                ("Corner Wedge", PartKind::CornerWedge),
            ] {
                if ui.button(label).clicked() {
                    self.insert_part(kind, config);
                }
            }
            ui.separator();
            ui.menu_button("Object", |ui| {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for class in ops::INSERTABLE {
                        if ui.button(*class).clicked() {
                            let parent = self.insert_parent();
                            let r =
                                ops::insert_object(&mut self.doc, parent, class, &self.viewport.camera, &self.scene);
                            self.explorer.reveal(r);
                            if kahua_core::kahua_io::reflect::is_script(class) {
                                self.open_script(r);
                            }
                        }
                    }
                });
            });
        });
        ui.menu_button("View", |ui| {
            ui.checkbox(&mut self.show_output, "Output");
            if ui.add(egui::Button::new("Focus Selection").shortcut_text("F")).clicked() {
                self.active_tab = None;
                self.focus_selection();
            }
        });
        ui.menu_button("Help", |ui| {
            if ui.button("About Kahua").clicked() {
                self.about = true;
            }
        });
        action
    }

    /// Copies a mesh file into the project and inserts a MeshPart in front of the camera.
    fn insert_mesh(&mut self, log: &mut Log) {
        let Some(file) = rfd::FileDialog::new().add_filter("3D model", &["glb", "gltf", "obj"]).pick_file() else {
            return;
        };
        let root = asset_root(&self.doc);
        match kahua_render::import_mesh(&root, &file) {
            Ok(imported) => {
                let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Mesh".into());
                let size = imported.size;
                let camera = &self.viewport.camera;
                let (origin, dir) = (camera.position, camera.forward());
                let p = match self.scene.pick(origin, dir) {
                    Some((_, t)) if t < 500.0 => origin + dir * t + Vec3::Y * size.y * 0.5,
                    _ => origin + dir * 20.0,
                };
                let v = |v: Vec3| Vector3::new(v.x, v.y, v.z);
                let snapshot = Snapshot::new("MeshPart", &name)
                    .with_property("Anchored", true)
                    .with_property("MeshContent", Content::from_uri(imported.uri.clone()))
                    .with_property("InitialSize", v(size))
                    .with_property("Size", v(size))
                    .with_property("CFrame", CFrame::new(v(p), Matrix3::identity()));
                let r = snapshot.referent;
                let edit = self.doc.create(world_root(&self.doc.dom), snapshot);
                self.doc.perform("Insert Mesh", vec![edit]);
                self.doc.selection = vec![r];
                self.explorer.reveal(r);
                self.active_tab = None;
                log.info(format!("Added {} as {}", file.display(), imported.uri));
            }
            Err(e) => log.error(format!("Could not import {}: {e:#}", file.display())),
        }
    }

    fn insert_part(&mut self, kind: PartKind, config: &AppConfig) {
        let snap = self.snap(config);
        let r = ops::insert_part(&mut self.doc, &self.scene, &self.viewport.camera, kind, snap);
        self.explorer.reveal(r);
        self.active_tab = None;
    }

    fn toolbar(&mut self, ui: &mut Ui, config: &mut AppConfig, log: &mut Log) {
        ui.horizontal(|ui| {
            let tool_button = |ui: &mut Ui, tool: &mut Tool, value: Tool, icon: Icon, label: &str, tip: &str| {
                let selected = *tool == value;
                if ui
                    .add(icons::toggle(selected, icon, label).min_size(egui::vec2(0.0, 26.0)))
                    .on_hover_text(tip)
                    .clicked()
                {
                    *tool = value;
                }
            };
            tool_button(ui, &mut self.tool, Tool::Select, Icon::Pointer, "Select", "Select (Ctrl+1)");
            tool_button(ui, &mut self.tool, Tool::Move, Icon::Move, "Move", "Move (Ctrl+2)");
            tool_button(ui, &mut self.tool, Tool::Scale, Icon::Scale, "Scale", "Scale (Ctrl+3)");
            tool_button(ui, &mut self.tool, Tool::Rotate, Icon::Rotate, "Rotate", "Rotate (Ctrl+4)");
            let local = self.viewport.local_axes;
            let (axes_icon, axes_label) = if local { (Icon::Cube, "Local") } else { (Icon::Globe, "World") };
            if ui
                .add(icons::toggle(local, axes_icon, axes_label).min_size(egui::vec2(0.0, 26.0)))
                .on_hover_text("Move and rotate along world or local axes (Ctrl+L)")
                .clicked()
            {
                self.viewport.local_axes = !local;
            }
            ui.separator();
            ui.label(RichText::new("Part").color(theme::TEXT_WEAK));
            for (icon, label, kind, tip) in [
                (Icon::Cube, "Block", PartKind::Block, "Insert a block"),
                (Icon::Circle, "Ball", PartKind::Sphere, "Insert a sphere"),
                (Icon::Cylinder, "Cylinder", PartKind::Cylinder, "Insert a cylinder"),
                (Icon::Wedge, "Wedge", PartKind::Wedge, "Insert a wedge"),
                (Icon::Pyramid, "Corner", PartKind::CornerWedge, "Insert a corner wedge"),
            ] {
                if ui.add(icons::button(icon, label).min_size(egui::vec2(0.0, 26.0))).on_hover_text(tip).clicked() {
                    self.insert_part(kind, config);
                }
            }
            ui.separator();
            ui.add(icons::image(Icon::Magnet, 14.0, icons::TEXT));
            let mut changed = ui.checkbox(&mut config.snap_enabled, "Snap").changed();
            changed |= ui
                .add_enabled(
                    config.snap_enabled,
                    egui::DragValue::new(&mut config.snap).speed(0.05).range(0.01..=64.0).suffix(" studs"),
                )
                .changed();
            if changed {
                config.save();
            }
            ui.separator();
            if ui
                .add(egui::DragValue::new(&mut config.rotate_snap).speed(0.5).range(0.0..=180.0).suffix("°"))
                .on_hover_text("Rotation snap in degrees (0 turns it off)")
                .changed()
            {
                config.save();
            }
            self.viewport.rotate_snap = config.rotate_snap;
            ui.separator();
            if ui
                .add_enabled(self.doc.can_undo(), icons::button(Icon::Undo, "Undo"))
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                self.doc.undo();
            }
            if ui
                .add_enabled(self.doc.can_redo(), icons::button(Icon::Redo, "Redo"))
                .on_hover_text("Redo (Ctrl+Y)")
                .clicked()
            {
                self.doc.redo();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = self.doc.is_dirty();
                let save = icons::button(Icon::Save, if dirty { "Save*" } else { "Save" });
                let save = if dirty { save.fill(theme::ACCENT) } else { save };
                if ui.add(save).on_hover_text("Save (Ctrl+S)").clicked() {
                    self.save(log);
                }
                let source = match &self.doc.source {
                    Source::Project(_) => "project",
                    Source::File(_) => "place file",
                };
                ui.label(
                    RichText::new(format!("{} · {source}", self.doc.path().display())).small().color(theme::TEXT_WEAK),
                );
            });
        });
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add(icons::toggle(self.active_tab.is_none(), Icon::Cube, "Scene")).clicked() {
                    self.active_tab = None;
                }
                let mut close = None;
                for &r in &self.tabs {
                    let Some(inst) = self.doc.dom.get_by_ref(r) else { continue };
                    let (icon, _) = icons::for_class(&inst.class);
                    let response = ui.add(icons::toggle(self.active_tab == Some(r), icon, inst.name.clone()));
                    if response.clicked() {
                        self.active_tab = Some(r);
                    }
                    if response.middle_clicked() {
                        close = Some(r);
                    }
                    let close_button = egui::Button::image(icons::image(Icon::Close, 12.0, icons::TEXT)).frame(false);
                    if ui.add(close_button).on_hover_text("Close (Ctrl+W)").clicked() {
                        close = Some(r);
                    }
                }
                if let Some(r) = close {
                    self.tabs.retain(|t| *t != r);
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, config: &mut AppConfig, log: &mut Log) {
        if let Some(dialog) = &mut self.account {
            let mut login_changed = false;
            if !dialog.ui(ctx, log, &mut login_changed) {
                self.account = None;
            }
            if login_changed {
                // Start over so assets that failed without a login are fetched again.
                let mut assets = AssetStore::new(asset_root(&self.doc));
                if let Some(downloader) = kahua_net::kahua::saved_downloader() {
                    assets.set_downloader(downloader);
                }
                self.assets = assets;
                self.scene_revision = None;
            }
        }
        if let Some(dialog) = &mut self.publish
            && !dialog.ui(ctx, &self.doc.dom, log)
        {
            self.publish = None;
        }
        if let Some(dialog) = &mut self.save_as {
            let mut close = false;
            egui::Modal::new(egui::Id::new("save_as_project")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Save as Project");
                ui.label(
                    RichText::new("Turns this place file into a project folder you can keep in git.")
                        .color(theme::TEXT_WEAK),
                );
                ui.add_space(6.0);
                egui::Grid::new("save_as_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut dialog.name);
                    ui.end_row();
                    ui.label("Location");
                    ui.text_edit_singleline(&mut dialog.location);
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut dialog.git, "Initialize a git repository");
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button("Create").clicked() {
                        let dir = PathBuf::from(dialog.location.trim()).join(dialog.name.trim());
                        match self.doc.save_as_project(&dir, dialog.name.trim(), dialog.git) {
                            Ok(warnings) => {
                                log.warnings("Save as Project", &warnings);
                                log.info(format!("Created project {}", dir.display()));
                                config.touch_recent(&dir, dialog.name.trim());
                                close = true;
                            }
                            Err(e) => log.error(format!("{e:#}")),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.save_as = None;
            }
        }
        if self.about {
            let response = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("Kahua").size(22.0).strong().color(theme::ACCENT));
                ui.label(concat!("Version ", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label("An open source editor for Roblox places, native on Linux.");
                ui.label("Not affiliated with or endorsed by Roblox Corporation.");
                ui.label("Licensed under the GNU GPL v3 or later.");
                ui.add_space(6.0);
                ui.hyperlink("https://github.com/Username59138/kahua");
                ui.add_space(6.0);
                ui.button("Close").clicked()
            });
            if response.inner || response.should_close() {
                self.about = false;
            }
        }
    }
}

/// Where `kahua://` asset URIs are resolved: the project folder, or the folder
/// of a place file opened directly.
fn asset_root(doc: &Document) -> PathBuf {
    let path = doc.path();
    if path.is_dir() { path.to_path_buf() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_default() }
}
