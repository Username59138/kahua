//! Turning instance names into safe, unique file names.

use std::collections::HashSet;

const RESERVED_SUFFIXES: &[&str] = &[".server", ".client", ".meta", ".model", ".project"];
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2",
    "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Makes an instance name safe to use as a file stem on Linux, macOS and Windows.
pub fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.starts_with('.') {
        out.replace_range(0..1, "_");
    }
    let lower = out.to_ascii_lowercase();
    for suffix in RESERVED_SUFFIXES {
        if lower.ends_with(suffix) {
            let at = out.len() - suffix.len();
            out.replace_range(at..at + 1, "_");
            break;
        }
    }
    let lower = out.to_ascii_lowercase();
    let base = lower.split('.').next().unwrap_or("");
    if lower == "init" || base == "init" || WINDOWS_RESERVED.contains(&base) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Allocates file stems for siblings, keeping them unique case-insensitively.
#[derive(Default)]
pub struct StemAllocator {
    used: HashSet<String>,
}

impl StemAllocator {
    pub fn reserve(&mut self, stem: &str) {
        self.used.insert(stem.to_lowercase());
    }

    pub fn allocate(&mut self, name: &str) -> String {
        let base = sanitize(name);
        if self.used.insert(base.to_lowercase()) {
            return base;
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}~{n}");
            if self.used.insert(candidate.to_lowercase()) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("Part"), "Part");
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize("init"), "_init");
        assert_eq!(sanitize("Main.server"), "Main_server");
        assert_eq!(sanitize("dots..."), "dots");
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("CON"), "_CON");
    }

    #[test]
    fn allocates_unique_stems() {
        let mut a = StemAllocator::default();
        assert_eq!(a.allocate("Part"), "Part");
        assert_eq!(a.allocate("Part"), "Part~2");
        assert_eq!(a.allocate("part"), "part~3");
    }
}
