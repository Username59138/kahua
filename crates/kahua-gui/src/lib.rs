//! Kahua's editor window.

mod config;
mod editor;
mod explorer;
mod icons;
mod log;
mod manager;
mod ops;
mod properties;
mod roblox_ui;
mod script_editor;
mod theme;
mod viewport;

use std::path::PathBuf;

use anyhow::anyhow;
use eframe::egui::{self, RichText, ViewportCommand};
use kahua_core::Document;

use config::AppConfig;
use editor::{Editor, EditorAction};
use log::Log;
use manager::{ManagerAction, ProjectManager};

/// Opens the editor window. With a path, opens that project or place file directly.
pub fn run(path: Option<PathBuf>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kahua")
            .with_app_id("kahua")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Kahua",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(KahuaApp::new(path)))
        }),
    )
    .map_err(|e| anyhow!("failed to open the window: {e}"))
}

enum Screen {
    Manager(ProjectManager),
    Editor(Box<Editor>),
}

/// What to do once the user answered "save changes?".
enum Pending {
    Quit,
    Close,
    Open(PathBuf),
}

struct KahuaApp {
    config: AppConfig,
    log: Log,
    screen: Screen,
    pending: Option<Pending>,
    allow_quit: bool,
    title: String,
}

impl KahuaApp {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            config: AppConfig::load(),
            log: Log::default(),
            screen: Screen::Manager(ProjectManager::default()),
            pending: None,
            allow_quit: false,
            title: String::new(),
        };
        if let Some(path) = path {
            app.open(path);
        }
        app
    }

    fn open(&mut self, path: PathBuf) {
        match Document::open(&path) {
            Ok((doc, warnings)) => {
                self.log.warnings("Open", &warnings);
                self.log.info(format!("Opened {}", path.display()));
                self.config.touch_recent(&path, &doc.title());
                self.screen = Screen::Editor(Box::new(Editor::new(doc)));
            }
            Err(e) => self.log.error(format!("Could not open {}: {e:#}", path.display())),
        }
    }

    fn dirty(&self) -> bool {
        matches!(&self.screen, Screen::Editor(e) if e.doc.is_dirty())
    }

    fn finish(&mut self, pending: Pending, ctx: &egui::Context) {
        match pending {
            Pending::Quit => {
                self.allow_quit = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            Pending::Close => self.screen = Screen::Manager(ProjectManager::default()),
            Pending::Open(path) => self.open(path),
        }
    }

    fn request(&mut self, pending: Pending, ctx: &egui::Context) {
        if self.dirty() {
            self.pending = Some(pending);
        } else {
            self.finish(pending, ctx);
        }
    }
}

impl eframe::App for KahuaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let title = match &self.screen {
            Screen::Manager(_) => "Kahua — Projects".to_owned(),
            Screen::Editor(e) => format!("{}{} — Kahua", e.doc.title(), if e.doc.is_dirty() { " •" } else { "" }),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_quit && self.dirty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }

        let mut next = None;
        match &mut self.screen {
            Screen::Manager(manager) => {
                if let Some(ManagerAction::Open(path)) = manager.ui(ui, &mut self.config, &mut self.log) {
                    next = Some(Pending::Open(path));
                }
            }
            Screen::Editor(editor) => match editor.ui(ui, frame, &mut self.config, &mut self.log) {
                Some(EditorAction::Close) => next = Some(Pending::Close),
                Some(EditorAction::Open(path)) => next = Some(Pending::Open(path)),
                Some(EditorAction::NewProject) => next = Some(Pending::Close),
                None => {}
            },
        }
        if let Some(pending) = next {
            self.request(pending, &ctx);
        }

        if self.pending.is_some() {
            let mut choice = None;
            let name = match &self.screen {
                Screen::Editor(e) => e.doc.title(),
                _ => String::new(),
            };
            egui::Modal::new(egui::Id::new("unsaved_changes")).show(&ctx, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new(format!("Save changes to {name}?")).size(16.0).strong());
                ui.label("Your changes will be lost if you don't save them.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Save").fill(theme::ACCENT)).clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Don't Save").clicked() {
                        choice = Some(2);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(3);
                    }
                });
            });
            match choice {
                Some(1) => {
                    let saved = match &mut self.screen {
                        Screen::Editor(e) => e.save(&mut self.log),
                        _ => true,
                    };
                    if saved {
                        let pending = self.pending.take().unwrap();
                        self.finish(pending, &ctx);
                    }
                }
                Some(2) => {
                    let pending = self.pending.take().unwrap();
                    self.finish(pending, &ctx);
                }
                Some(3) => self.pending = None,
                _ => {}
            }
        }

        self.log.toasts_ui(&ctx);
    }
}
