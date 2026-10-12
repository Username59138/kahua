//! The start screen: a project list like Godot's project manager.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Ui};
use kahua_core::kahua_io::{self, CreateOptions, project, template};

use crate::config::{AppConfig, ago};
use crate::log::Log;
use crate::theme;

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    last_opened: u64,
    is_file: bool,
    missing: bool,
}

#[derive(Default, Clone, PartialEq)]
enum Template {
    #[default]
    Baseplate,
    Empty,
}

#[derive(Default)]
struct NewDialog {
    name: String,
    location: String,
    template: Template,
    git: bool,
    /// Some(path) when importing a place file instead of starting from a template.
    import: Option<String>,
}

#[derive(Default)]
pub struct ProjectManager {
    filter: String,
    entries: Vec<Entry>,
    scanned: bool,
    dialog: Option<NewDialog>,
    projects_dir_text: String,
    selected: Option<PathBuf>,
}

pub enum ManagerAction {
    Open(PathBuf),
}

impl ProjectManager {
    pub fn rescan(&mut self, config: &AppConfig) {
        let mut entries: Vec<Entry> = config
            .recent
            .iter()
            .map(|r| Entry {
                path: r.path.clone(),
                name: r.name.clone(),
                last_opened: r.last_opened,
                is_file: r.path.is_file() || kahua_io::place::is_roblox_file(&r.path),
                missing: !r.path.exists(),
            })
            .collect();
        if let Ok(dir) = std::fs::read_dir(&config.projects_dir) {
            for entry in dir.flatten() {
                let path = entry.path();
                let path = path.canonicalize().unwrap_or(path);
                if kahua_io::is_project_dir(&path) && !entries.iter().any(|e| e.path == path) {
                    entries.push(Entry {
                        name: project_name(&path),
                        path,
                        last_opened: 0,
                        is_file: false,
                        missing: false,
                    });
                }
            }
        }
        entries.sort_by(|a, b| b.last_opened.cmp(&a.last_opened).then(a.name.cmp(&b.name)));
        self.entries = entries;
        self.projects_dir_text = config.projects_dir.display().to_string();
        self.scanned = true;
    }

    pub fn ui(&mut self, ui: &mut Ui, config: &mut AppConfig, log: &mut Log) -> Option<ManagerAction> {
        if !self.scanned {
            self.rescan(config);
        }
        let mut action = None;

        egui::Panel::top("manager_header")
            .frame(egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::symmetric(20, 14)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Kahua").size(26.0).strong().color(theme::ACCENT));
                    ui.add_space(6.0);
                    ui.label(RichText::new("Open source editor for Roblox places").color(theme::TEXT_WEAK));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).color(theme::TEXT_WEAK));
                    });
                });
            });

        egui::Panel::right("manager_actions")
            .exact_size(220.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| {
                let wide =
                    |ui: &mut Ui, text: &str| ui.add_sized([ui.available_width(), 30.0], egui::Button::new(text));
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(RichText::new("+  New Project").strong()).fill(theme::ACCENT),
                    )
                    .clicked()
                {
                    self.dialog = Some(NewDialog {
                        name: unique_name(&config.projects_dir, "NewGame"),
                        location: config.projects_dir.display().to_string(),
                        git: true,
                        ..Default::default()
                    });
                }
                ui.add_space(4.0);
                if wide(ui, "Import .rbxl…").clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .add_filter("Roblox place or model", &["rbxl", "rbxlx", "rbxm", "rbxmx"])
                        .pick_file()
                {
                    self.start_import(config, &file);
                }
                if wide(ui, "Open Folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_directory(&config.projects_dir).pick_folder()
                {
                    if kahua_io::is_project_dir(&dir) {
                        action = Some(ManagerAction::Open(dir));
                    } else {
                        log.error(format!(
                            "{} is not a Kahua/Rojo project (no default.project.json). Use Import for .rbxl files.",
                            dir.display()
                        ));
                    }
                }
                if wide(ui, "Open .rbxl File…").clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .add_filter("Roblox place or model", &["rbxl", "rbxlx", "rbxm", "rbxmx"])
                        .pick_file()
                {
                    action = Some(ManagerAction::Open(file));
                }
                ui.add_space(4.0);
                let can_open = self.selected.is_some();
                if ui
                    .add_enabled(
                        can_open,
                        egui::Button::new("Open Selected").min_size(egui::vec2(ui.available_width(), 30.0)),
                    )
                    .clicked()
                {
                    action = self.selected.clone().map(ManagerAction::Open);
                }
                if ui
                    .add_enabled(
                        can_open,
                        egui::Button::new("Remove from List").min_size(egui::vec2(ui.available_width(), 26.0)),
                    )
                    .on_hover_text("Forget this entry. Files on disk are not touched.")
                    .clicked()
                    && let Some(path) = self.selected.take()
                {
                    config.forget(&path);
                    self.rescan(config);
                }

                ui.add_space(16.0);
                ui.label(RichText::new("Projects folder").color(theme::TEXT_WEAK));
                let response =
                    ui.add(egui::TextEdit::singleline(&mut self.projects_dir_text).desired_width(f32::INFINITY));
                if response.lost_focus() {
                    config.projects_dir = PathBuf::from(self.projects_dir_text.trim());
                    config.save();
                    self.rescan(config);
                }
                if ui.button("Browse…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    config.projects_dir = dir;
                    config.save();
                    self.rescan(config);
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Projects").size(18.0).strong());
                    ui.add_space(12.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter).hint_text("Filter projects").desired_width(240.0),
                    );
                    if ui.button("⟳ Rescan").clicked() {
                        self.rescan(config);
                    }
                });
                ui.add_space(8.0);
                if self.entries.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No projects yet").size(16.0));
                        ui.label(
                            RichText::new("Create a new project or import an existing .rbxl place.")
                                .color(theme::TEXT_WEAK),
                        );
                    });
                }
                let filter = self.filter.to_lowercase();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for entry in self.entries.clone() {
                        if !filter.is_empty() && !entry.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        if let Some(a) = self.entry_row(ui, &entry) {
                            action = Some(a);
                        }
                    }
                });
            });

        if let Some(a) = self.dialog_ui(ui.ctx(), config, log) {
            action = Some(a);
        }
        action
    }

    fn entry_row(&mut self, ui: &mut Ui, entry: &Entry) -> Option<ManagerAction> {
        let selected = self.selected.as_ref() == Some(&entry.path);
        let height = 52.0;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
        let fill = if selected {
            theme::SELECTION.gamma_multiply(0.55)
        } else if response.hovered() {
            Color32::from_rgb(0x2A, 0x2C, 0x31)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 4.0, fill);
        let painter = ui.painter();
        let icon = rect.left_center() + egui::vec2(26.0, 0.0);
        painter.circle_filled(
            icon,
            15.0,
            if entry.is_file { Color32::from_rgb(0x4A, 0x6F, 0xA5) } else { theme::ACCENT },
        );
        painter.text(
            icon,
            egui::Align2::CENTER_CENTER,
            entry.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(),
            egui::FontId::proportional(16.0),
            Color32::WHITE,
        );
        let name_color = if entry.missing { theme::TEXT_WEAK } else { Color32::WHITE };
        painter.text(
            rect.left_top() + egui::vec2(52.0, 9.0),
            egui::Align2::LEFT_TOP,
            &entry.name,
            egui::FontId::proportional(15.0),
            name_color,
        );
        let mut subtitle = entry.path.display().to_string();
        if entry.is_file {
            subtitle.push_str("  ·  place file");
        }
        if entry.missing {
            subtitle.push_str("  ·  missing");
        }
        painter.text(
            rect.left_top() + egui::vec2(52.0, 30.0),
            egui::Align2::LEFT_TOP,
            subtitle,
            egui::FontId::proportional(12.0),
            theme::TEXT_WEAK,
        );
        painter.text(
            rect.right_center() - egui::vec2(14.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            ago(entry.last_opened),
            egui::FontId::proportional(12.0),
            theme::TEXT_WEAK,
        );
        if response.clicked() {
            self.selected = Some(entry.path.clone());
        }
        (response.double_clicked() && !entry.missing).then(|| ManagerAction::Open(entry.path.clone()))
    }

    fn start_import(&mut self, config: &AppConfig, file: &Path) {
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
        self.dialog = Some(NewDialog {
            name: unique_name(&config.projects_dir, &stem),
            location: config.projects_dir.display().to_string(),
            git: true,
            import: Some(file.display().to_string()),
            ..Default::default()
        });
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, config: &mut AppConfig, log: &mut Log) -> Option<ManagerAction> {
        let dialog = self.dialog.as_mut()?;
        let mut close = false;
        let mut action = None;
        let title = if dialog.import.is_some() { "Import Place" } else { "New Project" };
        egui::Modal::new(egui::Id::new("new_project_modal")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(title);
            ui.add_space(8.0);
            egui::Grid::new("new_project_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                if let Some(import) = &mut dialog.import {
                    ui.label("Place file");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(import).desired_width(300.0));
                        if ui.button("…").clicked()
                            && let Some(file) = rfd::FileDialog::new()
                                .add_filter("Roblox place or model", &["rbxl", "rbxlx", "rbxm", "rbxmx"])
                                .pick_file()
                        {
                            *import = file.display().to_string();
                        }
                    });
                    ui.end_row();
                }
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut dialog.name).desired_width(300.0));
                ui.end_row();
                ui.label("Location");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut dialog.location).desired_width(300.0));
                    if ui.button("…").clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        dialog.location = dir.display().to_string();
                    }
                });
                ui.end_row();
                if dialog.import.is_none() {
                    ui.label("Template");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut dialog.template, Template::Baseplate, "Baseplate");
                        ui.selectable_value(&mut dialog.template, Template::Empty, "Empty");
                    });
                    ui.end_row();
                }
                ui.label("");
                ui.checkbox(&mut dialog.git, "Initialize a git repository");
                ui.end_row();
            });
            let target = Path::new(dialog.location.trim()).join(dialog.name.trim());
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Will create {}", target.display())).small().color(theme::TEXT_WEAK));
            let name_ok = !dialog.name.trim().is_empty();
            let free = !target.exists() || std::fs::read_dir(&target).map(|mut d| d.next().is_none()).unwrap_or(false);
            if !free {
                ui.label(RichText::new("That folder already exists and is not empty.").color(theme::WARN));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if dialog.import.is_some() { "Import & Open" } else { "Create & Open" };
                if ui
                    .add_enabled(name_ok && free, egui::Button::new(RichText::new(label).strong()).fill(theme::ACCENT))
                    .clicked()
                {
                    let options = CreateOptions { git: dialog.git };
                    let name = dialog.name.trim().to_owned();
                    let result = match &dialog.import {
                        Some(file) => project::import(Path::new(file.trim()), &target, &name, &options),
                        None => {
                            let dom = match dialog.template {
                                Template::Baseplate => template::baseplate(),
                                Template::Empty => template::empty(),
                            };
                            project::create_from_dom(&target, &name, &dom, &options).map(|(_, w)| w)
                        }
                    };
                    match result {
                        Ok(warnings) => {
                            log.warnings(&name, &warnings);
                            if let Some(parent) = target.parent() {
                                config.projects_dir = parent.to_path_buf();
                                config.save();
                            }
                            action = Some(ManagerAction::Open(target.clone()));
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
            self.dialog = None;
            self.scanned = false;
        }
        action
    }
}

fn project_name(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(project::PROJECT_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn unique_name(dir: &Path, base: &str) -> String {
    if !dir.join(base).exists() {
        return base.to_owned();
    }
    (2..).map(|i| format!("{base}{i}")).find(|n| !dir.join(n).exists()).unwrap()
}
