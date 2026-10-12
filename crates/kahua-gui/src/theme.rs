//! Kahua's look: a dark theme with a warm volcanic accent.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(0xE8, 0x6A, 0x33);
pub const SELECTION: Color32 = Color32::from_rgb(0x2F, 0x6F, 0xB8);
pub const PANEL: Color32 = Color32::from_rgb(0x23, 0x24, 0x28);
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x1B, 0x1C, 0x1F);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x8B, 0x8E, 0x96);
pub const WARN: Color32 = Color32::from_rgb(0xE5, 0xB4, 0x4C);
pub const ERROR: Color32 = Color32::from_rgb(0xE5, 0x5C, 0x5C);
pub const OK: Color32 = Color32::from_rgb(0x6C, 0xC2, 0x7A);

/// Selection outline color in the 3D view (linear RGBA).
pub const SELECTION_3D: [f32; 4] = [0.05, 0.55, 1.0, 1.0];
pub const HOVER_3D: [f32; 4] = [0.55, 0.8, 1.0, 0.7];

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = PANEL_DARK;
    visuals.faint_bg_color = Color32::from_rgb(0x28, 0x29, 0x2E);
    visuals.selection.bg_fill = SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = ACCENT;
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);
    for w in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(3);
    }
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x30, 0x32, 0x37);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3A, 0x3D, 0x44);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x45, 0x48, 0x50);
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 20.0;
    });
}
