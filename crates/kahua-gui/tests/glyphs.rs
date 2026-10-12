//! Every symbol the UI uses must exist in egui's bundled fonts.
use eframe::egui;

#[test]
fn ui_symbols_have_glyphs() {
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    let mut sources = String::new();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
        sources.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    // Only check characters inside string literals of UI code: anything non-ASCII.
    let mut missing = std::collections::BTreeSet::new();
    for line in sources.lines() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        for c in line.chars().filter(|c| !c.is_ascii()) {
            let ok = ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(14.0), c));
            if !ok {
                missing.insert(c);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "missing glyphs: {:?}",
        missing.iter().map(|c| format!("{c} U+{:04X}", *c as u32)).collect::<Vec<_>>()
    );
}
