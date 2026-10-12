//! The Output panel and short pop-up notifications.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, RichText, Ui};

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn color(self) -> Color32 {
        match self {
            Level::Info => Color32::from_gray(200),
            Level::Warn => theme::WARN,
            Level::Error => theme::ERROR,
        }
    }
}

pub struct Line {
    pub level: Level,
    pub text: String,
    pub time: String,
}

#[derive(Default)]
pub struct Log {
    pub lines: Vec<Line>,
    toasts: Vec<(Level, String, Instant)>,
}

impl Log {
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        let secs = crate::config::now() % 86_400;
        let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
        if level != Level::Info {
            eprintln!("{}", text);
        }
        self.lines.push(Line { level, text, time });
        if self.lines.len() > 2000 {
            self.lines.drain(..500);
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.push(Level::Warn, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.toast(Level::Error, text.clone());
        self.push(Level::Error, text);
    }

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        self.toasts.push((level, text.into(), Instant::now()));
    }

    pub fn warnings(&mut self, context: &str, warnings: &[String]) {
        for w in warnings {
            self.warn(format!("{context}: {w}"));
        }
        if !warnings.is_empty() {
            self.toast(Level::Warn, format!("{context}: {} warning(s), see Output", warnings.len()));
        }
    }

    pub fn output_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Output").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear").clicked() {
                    self.lines.clear();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            for line in &self.lines {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&line.time).monospace().color(theme::TEXT_WEAK));
                    ui.label(RichText::new(&line.text).monospace().color(line.level.color()));
                });
            }
        });
    }

    /// Draws pending notifications in the bottom-right corner.
    pub fn toasts_ui(&mut self, ctx: &egui::Context) {
        const LIFETIME: Duration = Duration::from_secs(5);
        self.toasts.retain(|(_, _, t)| t.elapsed() < LIFETIME);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("kahua_toasts"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (level, text, _) in &self.toasts {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(380.0);
                        let color = match level {
                            Level::Info => theme::OK,
                            other => other.color(),
                        };
                        ui.label(RichText::new(text).color(color));
                    });
                }
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}
