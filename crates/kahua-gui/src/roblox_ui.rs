//! Roblox account and publishing dialogs.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, RichText};
use kahua_core::kahua_io::config::{KahuaConfig, PublishSection};
use kahua_core::kahua_io::{place, project};
use kahua_net::Credentials;

use crate::log::{Level, Log};
use crate::theme;

/// File > Roblox Account.
///
/// Network checks run on background threads so a slow or missing connection
/// never freezes the editor.
pub struct AccountDialog {
    api_key: String,
    cookie: String,
    has_api_key: bool,
    login: String,
    /// Checking the saved login when the dialog opens.
    status_check: Option<Receiver<String>>,
    /// Checking a new login before saving it.
    save_check: Option<Receiver<Result<(), String>>>,
    error: Option<String>,
}

fn describe_login(cookie: &str) -> String {
    match kahua_net::kahua::check_cookie(cookie) {
        Ok(user) => format!("{} (@{})", user.display_name, user.name),
        Err(e) => format!("saved, but it does not work: {e:#}"),
    }
}

impl AccountDialog {
    pub fn open() -> Self {
        let creds = Credentials::load().unwrap_or_default();
        let (login, status_check) = match creds.cookie {
            Some(cookie) => {
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let _ = tx.send(describe_login(&cookie));
                });
                ("checking…".to_owned(), Some(rx))
            }
            None => ("not logged in".to_owned(), None),
        };
        Self {
            api_key: String::new(),
            cookie: String::new(),
            has_api_key: creds.api_key.is_some(),
            login,
            status_check,
            save_check: None,
            error: None,
        }
    }

    /// Returns false when the dialog should close. `changed` is set when the
    /// login cookie changed (so asset downloads can be set up again).
    pub fn ui(&mut self, ctx: &egui::Context, log: &mut Log, changed: &mut bool) -> bool {
        let mut keep_open = true;
        if let Some(rx) = &self.status_check
            && let Ok(status) = rx.try_recv()
        {
            self.login = status;
            self.status_check = None;
        }
        if let Some(rx) = &self.save_check
            && let Ok(result) = rx.try_recv()
        {
            self.save_check = None;
            match result {
                Ok(()) => {
                    *changed = true;
                    log.toast(Level::Info, "Roblox account saved");
                    return false;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.status_check.is_some() || self.save_check.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let saving = self.save_check.is_some();

        egui::Modal::new(egui::Id::new("roblox_account")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Roblox Account");
            ui.add_space(6.0);

            ui.label(RichText::new("Open Cloud API key").strong());
            ui.label(
                RichText::new(
                    "Used to upload meshes and textures and to publish places. Create one in the Creator \
                     Dashboard with the Assets (read, write) and universe-places (write) permissions.",
                )
                .small()
                .color(theme::TEXT_WEAK),
            );
            ui.label(if self.has_api_key { "Status: saved" } else { "Status: not set" });
            ui.add(
                egui::TextEdit::singleline(&mut self.api_key)
                    .password(true)
                    .hint_text("Paste a new key to replace it")
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(10.0);

            ui.label(RichText::new("Roblox login").strong());
            ui.label(
                RichText::new(
                    "Used only to download meshes and textures of existing places. Never share this cookie: \
                     it gives full access to your account.",
                )
                .small()
                .color(theme::TEXT_WEAK),
            );
            ui.label(format!("Status: {}", self.login));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cookie)
                        .password(true)
                        .hint_text(".ROBLOSECURITY cookie")
                        .desired_width(300.0),
                );
                if ui
                    .button("Use Studio login")
                    .on_hover_text("Reuse the login of Roblox Studio running under Vinegar")
                    .clicked()
                {
                    match kahua_net::find_studio_cookie() {
                        Some(cookie) => self.cookie = cookie,
                        None => self.error = Some("No saved Roblox Studio login was found.".into()),
                    }
                }
            });

            if let Some(error) = &self.error {
                ui.add_space(6.0);
                ui.label(RichText::new(error).color(theme::ERROR));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if saving {
                    ui.spinner();
                    ui.label("Checking the login…");
                } else {
                    if ui.add(egui::Button::new("Save").fill(theme::ACCENT)).clicked() {
                        self.start_save();
                    }
                    if ui.button("Log Out").clicked() {
                        match Credentials::clear() {
                            Ok(()) => {
                                *changed = true;
                                log.toast(Level::Info, "Logged out");
                                keep_open = false;
                            }
                            Err(e) => self.error = Some(format!("{e:#}")),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                }
            });
        });
        keep_open
    }

    /// Saves the API key and, after checking it with Roblox, the login.
    fn start_save(&mut self) {
        self.error = None;
        let api_key = self.api_key.trim().to_owned();
        let cookie = self.cookie.trim().to_owned();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let mut creds = Credentials::load().unwrap_or_default();
                if !api_key.is_empty() {
                    creds.api_key = Some(api_key);
                }
                if !cookie.is_empty() {
                    kahua_net::kahua::check_cookie(&cookie)?;
                    creds.cookie = Some(cookie);
                }
                creds.save()
            })();
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
        self.save_check = Some(rx);
    }
}

/// File > Publish to Roblox.
pub struct PublishDialog {
    project_dir: PathBuf,
    universe_id: String,
    place_id: String,
    group: bool,
    creator_id: String,
    progress: Vec<String>,
    running: Option<Receiver<Message>>,
    finished: bool,
    error: Option<String>,
}

enum Message {
    Line(String),
    Done(Result<String, String>),
}

fn config_path(dir: &Path) -> PathBuf {
    dir.join(project::CONFIG_FILE)
}

impl PublishDialog {
    pub fn open(project_dir: &Path) -> Self {
        let section = KahuaConfig::read(&config_path(project_dir)).ok().and_then(|c| c.publish).unwrap_or_default();
        let id = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_default();
        Self {
            project_dir: project_dir.to_path_buf(),
            universe_id: id(section.universe_id),
            place_id: id(section.place_id),
            group: section.creator_group_id.is_some(),
            creator_id: id(section.creator_group_id.or(section.creator_user_id)),
            progress: Vec::new(),
            running: None,
            finished: false,
            error: None,
        }
    }

    /// Returns false when the dialog should close.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        dom: &kahua_core::kahua_io::rbx_dom_weak::WeakDom,
        log: &mut Log,
    ) -> bool {
        if let Some(rx) = &self.running {
            while let Ok(message) = rx.try_recv() {
                match message {
                    Message::Line(line) => self.progress.push(line),
                    Message::Done(result) => {
                        self.finished = true;
                        match result {
                            Ok(summary) => {
                                log.info(summary.clone());
                                log.toast(Level::Info, summary.clone());
                                self.progress.push(summary);
                            }
                            Err(e) => {
                                log.error(format!("Publish failed: {e}"));
                                self.error = Some(e);
                            }
                        }
                    }
                }
            }
            if self.finished {
                self.running = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        let mut keep_open = true;
        let busy = self.running.is_some();
        egui::Modal::new(egui::Id::new("publish_to_roblox")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Publish to Roblox");
            ui.label(
                RichText::new("Changed meshes and textures are uploaded first; their IDs are kept in assets.lock.")
                    .small()
                    .color(theme::TEXT_WEAK),
            );
            ui.add_space(6.0);
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("publish_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Universe ID");
                    ui.text_edit_singleline(&mut self.universe_id);
                    ui.end_row();
                    ui.label("Place ID");
                    ui.text_edit_singleline(&mut self.place_id);
                    ui.end_row();
                    ui.label("Creator");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.group, false, "User");
                        ui.selectable_value(&mut self.group, true, "Group");
                        ui.add(egui::TextEdit::singleline(&mut self.creator_id).hint_text("ID").desired_width(140.0));
                    });
                    ui.end_row();
                });
            });
            if !self.progress.is_empty() {
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &self.progress {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
            }
            if let Some(error) = &self.error {
                ui.label(RichText::new(error).color(theme::ERROR));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                    ui.label("Publishing…");
                } else if ui.add(egui::Button::new("Publish").fill(theme::ACCENT)).clicked() {
                    match self.start(dom) {
                        Ok(()) => {}
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if !busy && ui.button(if self.finished { "Close" } else { "Cancel" }).clicked() {
                    keep_open = false;
                }
            });
        });
        keep_open
    }

    fn start(&mut self, dom: &kahua_core::kahua_io::rbx_dom_weak::WeakDom) -> anyhow::Result<()> {
        let parse = |s: &str, what: &str| -> anyhow::Result<u64> {
            s.trim().parse::<u64>().map_err(|_| anyhow::anyhow!("{what} must be a number"))
        };
        let section = PublishSection {
            universe_id: Some(parse(&self.universe_id, "Universe ID")?),
            place_id: Some(parse(&self.place_id, "Place ID")?),
            creator_user_id: (!self.group).then(|| parse(&self.creator_id, "Creator ID")).transpose()?,
            creator_group_id: self.group.then(|| parse(&self.creator_id, "Creator ID")).transpose()?,
        };
        let path = config_path(&self.project_dir);
        let mut config = KahuaConfig::read(&path)
            .unwrap_or_else(|_| KahuaConfig::new(&self.project_dir.file_name().unwrap_or_default().to_string_lossy()));
        config.publish = Some(section);
        config.write(&path)?;

        // Publish a snapshot of the current place on a background thread.
        let bytes = place::encode(dom, false)?;
        let dir = self.project_dir.clone();
        let (tx, rx) = channel();
        self.progress.clear();
        self.error = None;
        self.finished = false;
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<String> {
                use kahua_core::kahua_io::publish::{self, SyncEvent};
                let dom = place::decode(&bytes)?;
                let mut uploader = kahua_net::kahua::RobloxUploader::for_project(&dir)?;
                let mut progress = |event: SyncEvent| {
                    let line = match event {
                        SyncEvent::Reused { .. } | SyncEvent::Planned { .. } => return,
                        SyncEvent::Uploading { path, index, total } => {
                            format!("[{}/{total}] uploading {path}", index + 1)
                        }
                        SyncEvent::Uploaded { path, asset_id } => format!("{path} -> rbxassetid://{asset_id}"),
                        SyncEvent::Failed { path, error } => format!("{path} failed: {error}"),
                        SyncEvent::Publishing { place_id, .. } => format!("publishing place {place_id}"),
                    };
                    let _ = tx.send(Message::Line(line));
                };
                let report = publish::publish(&dir, &dom, &mut uploader, &mut progress)?;
                Ok(format!("Published version {} ({} assets uploaded)", report.version, report.sync.uploaded.len()))
            })();
            let _ = tx.send(Message::Done(result.map_err(|e| format!("{e:#}"))));
        });
        self.running = Some(rx);
        Ok(())
    }
}
