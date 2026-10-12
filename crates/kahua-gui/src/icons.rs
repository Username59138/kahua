//! Vector icons (Lucide, ISC license, see assets/icons/LICENSE).
//!
//! The SVGs are drawn white and tinted at runtime, so one file serves every color.

use eframe::egui::{self, Color32, ImageSource, Rect, Sense, Ui, Vec2};
use kahua_core::kahua_io::reflect;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($name),* }

        impl Icon {
            #[cfg(test)]
            pub const ALL: &[Icon] = &[$(Icon::$name),*];

            pub fn source(self) -> ImageSource<'static> {
                match self {
                    $(Icon::$name => egui::include_image!(concat!("../assets/icons/", $file, ".svg"))),*
                }
            }
        }
    };
}

icons! {
    Globe => "globe",
    Users => "users",
    User => "user",
    Sun => "sun",
    Archive => "archive",
    Database => "database",
    Server => "server",
    Layout => "layout-panel-top",
    Hammer => "hammer",
    FolderCode => "folder-code",
    Music => "music",
    Flag => "flag",
    FileCode => "file-code",
    FileBraces => "file-braces",
    Boxes => "boxes",
    Folder => "folder",
    Camera => "camera",
    MapPin => "map-pin",
    Mountain => "mountain",
    Cube => "box",
    Variable => "variable",
    Link => "link",
    Lightbulb => "lightbulb",
    Shapes => "shapes",
    Pointer => "mouse-pointer-2",
    Move => "move",
    Circle => "circle",
    Cylinder => "cylinder",
    Wedge => "triangle-right",
    Pyramid => "pyramid",
    Magnet => "magnet",
    Undo => "undo-2",
    Redo => "redo-2",
    Save => "save",
    Close => "x",
    Refresh => "refresh-cw",
    Plus => "plus",
    FolderOpen => "folder-open",
    File => "file",
    Import => "import",
    Pen => "square-pen",
    ChevronRight => "chevron-right",
    ChevronDown => "chevron-down",
    ChevronUp => "chevron-up",
    CaseSensitive => "case-sensitive",
    WholeWord => "whole-word",
    Replace => "replace",
    ReplaceAll => "replace-all",
}

/// Default color for icons in buttons and text.
pub const TEXT: Color32 = Color32::from_gray(215);

pub fn image(icon: Icon, size: f32, color: Color32) -> egui::Image<'static> {
    egui::Image::new(icon.source()).fit_to_exact_size(Vec2::splat(size)).tint(color)
}

/// An icon at the normal UI size in the normal text color.
pub fn small(icon: Icon) -> egui::Image<'static> {
    image(icon, 14.0, TEXT)
}

pub fn paint(ui: &Ui, rect: Rect, icon: Icon, color: Color32) {
    image(icon, rect.width(), color).paint_at(ui, rect);
}

/// Icon and color for an instance class in the Explorer.
pub fn for_class(class: &str) -> (Icon, Color32) {
    let c = Color32::from_rgb;
    match class {
        "Workspace" => (Icon::Globe, c(0x5A, 0xA6, 0xF0)),
        "Players" => (Icon::Users, c(0x6F, 0xC2, 0x85)),
        "Player" => (Icon::User, c(0x6F, 0xC2, 0x85)),
        "Lighting" => (Icon::Sun, c(0xF0, 0xC0, 0x40)),
        "ReplicatedFirst" | "ReplicatedStorage" => (Icon::Archive, c(0x9A, 0xA5, 0xB8)),
        "ServerStorage" => (Icon::Database, c(0x9A, 0xA5, 0xB8)),
        "ServerScriptService" => (Icon::Server, c(0x7C, 0xC4, 0x7A)),
        "StarterGui" => (Icon::Layout, c(0xA8, 0x95, 0xF0)),
        "StarterPack" => (Icon::Hammer, c(0xA8, 0x95, 0xF0)),
        "StarterPlayer" => (Icon::User, c(0xA8, 0x95, 0xF0)),
        "StarterPlayerScripts" | "StarterCharacterScripts" => (Icon::FolderCode, c(0xA8, 0x95, 0xF0)),
        "SoundService" | "Sound" => (Icon::Music, c(0x5C, 0xC8, 0xC8)),
        "Teams" | "Team" => (Icon::Flag, c(0xE0, 0x70, 0x70)),
        "Script" => (Icon::FileCode, c(0x6C, 0xC2, 0x5E)),
        "LocalScript" => (Icon::FileCode, c(0x50, 0xA8, 0xF0)),
        "ModuleScript" => (Icon::FileBraces, c(0xB9, 0x8C, 0xF0)),
        "Model" => (Icon::Boxes, c(0xF0, 0x96, 0x50)),
        "Folder" => (Icon::Folder, c(0xE8, 0xC0, 0x50)),
        "Camera" => (Icon::Camera, c(0x9A, 0xA5, 0xB8)),
        "SpawnLocation" => (Icon::MapPin, c(0x6C, 0xC2, 0x85)),
        "Terrain" => (Icon::Mountain, c(0x8C, 0xB8, 0x60)),
        "Tool" => (Icon::Hammer, c(0xD8, 0xA0, 0x60)),
        _ if reflect::is_base_part(class) => (Icon::Cube, c(0x6A, 0xA8, 0xF0)),
        _ if reflect::is_a(class, "ValueBase") => (Icon::Variable, c(0xC8, 0xC0, 0x70)),
        _ if reflect::is_a(class, "GuiBase") => (Icon::Layout, c(0x86, 0x9A, 0xF0)),
        _ if reflect::is_a(class, "Constraint") || reflect::is_a(class, "JointInstance") => {
            (Icon::Link, c(0xC8, 0x96, 0x70))
        }
        _ if reflect::is_a(class, "Light") => (Icon::Lightbulb, c(0xF0, 0xD0, 0x60)),
        _ => (Icon::Shapes, c(0x9A, 0xA0, 0xAA)),
    }
}

/// Paints a class icon into `rect`.
pub fn paint_class(ui: &Ui, rect: Rect, class: &str) {
    let (icon, color) = for_class(class);
    paint(ui, rect, icon, color);
}

/// Allocates space for a 16px class icon and paints it.
pub fn class_icon(ui: &mut Ui, class: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    paint_class(ui, rect, class);
    response
}

/// A button with an icon and a label.
pub fn button<'a>(icon: Icon, text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::image_and_text(small(icon), text)
}

/// A toggle button with an icon and a label.
pub fn toggle<'a>(selected: bool, icon: Icon, text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::selectable(selected, (small(icon), text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_tintable_svg() {
        for icon in Icon::ALL {
            let ImageSource::Bytes { bytes, .. } = icon.source() else { panic!("{icon:?} is not embedded") };
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.contains("<svg"), "{icon:?} is not an SVG");
            assert!(!text.contains("currentColor"), "{icon:?} must be white so it can be tinted");
        }
    }
}
