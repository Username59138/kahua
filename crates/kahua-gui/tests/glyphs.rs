//! The UI uses real icons (see src/icons.rs), never emoji or symbol characters.
//! Only these typographic characters may appear in UI code.
const ALLOWED: &[char] = &['…', '·', '—', '°', '•'];

#[test]
fn ui_uses_icons_not_symbols() {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for c in code.chars().filter(|c| !c.is_ascii() && !ALLOWED.contains(c)) {
                found.push(format!("{}:{} {c:?}", path.file_name().unwrap().to_string_lossy(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "use an icon from icons.rs instead of: {found:?}");
}
