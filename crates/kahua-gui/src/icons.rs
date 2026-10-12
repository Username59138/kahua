//! Small class badges for the Explorer (our own design, no Roblox assets).

use eframe::egui::{self, Color32, FontId, Rect, Sense, Ui, Vec2};
use kahua_core::kahua_io::reflect;

struct Badge {
    letter: &'static str,
    color: Color32,
}

fn badge_for(class: &str) -> Badge {
    let b = |letter, r, g, b| Badge { letter, color: Color32::from_rgb(r, g, b) };
    match class {
        "Workspace" => b("W", 0x3E, 0x8E, 0xDE),
        "Lighting" => b("L", 0xD9, 0xA4, 0x2B),
        "Players" | "Player" => b("P", 0x5A, 0x9E, 0x6F),
        "ReplicatedStorage" | "ReplicatedFirst" | "ServerStorage" => b("▣", 0x7A, 0x7F, 0x8C),
        "ServerScriptService" => b("S", 0x5E, 0x8C, 0x5A),
        "StarterGui" | "StarterPack" | "StarterPlayer" | "StarterPlayerScripts" | "StarterCharacterScripts" => {
            b("★", 0x8A, 0x7C, 0xC9)
        }
        "SoundService" | "Sound" => b("♪", 0x4F, 0xA3, 0xA5),
        "Teams" | "Team" => b("⚑", 0xB0, 0x5E, 0x5E),
        "Script" => b("S", 0x3F, 0x9E, 0x4F),
        "LocalScript" => b("L", 0x2F, 0x8F, 0xB0),
        "ModuleScript" => b("M", 0x9A, 0x5F, 0xC9),
        "Model" => b("M", 0xD0, 0x7A, 0x2E),
        "Folder" => b("F", 0xC9, 0xA2, 0x35),
        "Camera" => b("C", 0x6F, 0x78, 0x88),
        "SpawnLocation" => b("✚", 0x4C, 0xA8, 0x6A),
        "Terrain" => b("T", 0x6F, 0x8F, 0x4A),
        _ if reflect::is_base_part(class) => b("■", 0x4A, 0x7F, 0xC4),
        _ if reflect::is_a(class, "ValueBase") => b("V", 0x8C, 0x8C, 0x5A),
        _ if reflect::is_a(class, "GuiBase") => b("G", 0x5D, 0x6E, 0xC4),
        _ if reflect::is_a(class, "Constraint") || reflect::is_a(class, "JointInstance") => b("J", 0x8F, 0x6A, 0x4A),
        _ if reflect::is_a(class, "Light") => b("*", 0xD9, 0xC1, 0x4B),
        _ => b("•", 0x6A, 0x6D, 0x75),
    }
}

/// Paints a 15x15 badge for a class and returns its response.
pub fn class_badge(ui: &mut Ui, class: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
    paint_badge(ui, rect, class);
    response
}

pub fn paint_badge(ui: &Ui, rect: Rect, class: &str) {
    let badge = badge_for(class);
    let painter = ui.painter();
    painter.rect_filled(rect.shrink(0.5), 3.0, badge.color);
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, badge.letter, FontId::proportional(10.0), Color32::WHITE);
}
