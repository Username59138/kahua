//! Finds the `.ROBLOSECURITY` cookie that Roblox Studio saved.
//!
//! Studio stores the cookie in the registry at
//! `HKCU\Software\Roblox\RobloxStudioBrowser\roblox.com`, value
//! `.ROBLOSECURITY`, as `SEC::<YES>,EXP::<date>,COOK::<value>`. On Linux,
//! Studio runs under Wine (Vinegar), whose registry is the text file `user.reg`
//! in the Wine prefix. Only that Linux path is implemented; Windows registry
//! access is not.

use std::fs;
use std::path::PathBuf;

const COOKIE_VALUE_NAME: &str = ".ROBLOSECURITY";
/// Registry key, with single backslashes, as it appears in `user.reg` after
/// unescaping.
const STUDIO_KEY: &str = r"Software\Roblox\RobloxStudioBrowser\roblox.com";

/// Returns the cookie saved by Roblox Studio, if a Vinegar prefix has one.
pub fn find_studio_cookie() -> Option<String> {
    candidate_registry_files().into_iter().find_map(|path| {
        let contents = fs::read_to_string(path).ok()?;
        parse_wine_user_reg(&contents)
    })
}

/// Vinegar prefix locations for `user.reg`, native first, then Flatpak.
fn candidate_registry_files() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(data) = dirs::data_dir() {
        paths.push(data.join("vinegar/prefixes/studio/user.reg"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".var/app/org.vinegarhq.Vinegar/data/vinegar/prefixes/studio/user.reg"));
    }
    paths
}

/// Extracts the cookie from a Wine `user.reg` file.
pub(crate) fn parse_wine_user_reg(contents: &str) -> Option<String> {
    let mut in_studio_key = false;
    for line in contents.lines() {
        if let Some(header) = line.strip_prefix('[') {
            let name = header.split(']').next().unwrap_or("");
            in_studio_key = unescape_wine(name).eq_ignore_ascii_case(STUDIO_KEY);
            continue;
        }
        if !in_studio_key {
            continue;
        }
        let quoted_name = format!("\"{COOKIE_VALUE_NAME}\"=\"");
        if let Some(rest) = line.strip_prefix(&quoted_name) {
            let value = rest.trim_end().strip_suffix('"')?;
            return parse_studio_cookie(&unescape_wine(value));
        }
    }
    None
}

/// Extracts the cookie from Studio's `SEC::<YES>,EXP::<date>,COOK::<value>`
/// format. Returns `None` if the value is missing or not wrapped in `<>`.
pub(crate) fn parse_studio_cookie(entry: &str) -> Option<String> {
    for pair in entry.split(',') {
        let mut parts = pair.splitn(2, "::");
        if parts.next() != Some("COOK") {
            continue;
        }
        let value = parts.next()?;
        let inner = value.strip_prefix('<')?.strip_suffix('>')?;
        return Some(inner.to_owned());
    }
    None
}

/// Undoes Wine's escaping of `\` and `"`.
fn unescape_wine(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made-up values; not real cookies.
    const FAKE_COOKIE: &str = "_|WARNING:-FAKE-TEST-COOKIE|_abc123";

    #[test]
    fn parses_studio_entry() {
        let entry = format!("SEC::<YES>,EXP::<2030-01-01T00:00:00Z>,COOK::<{FAKE_COOKIE}>");
        assert_eq!(parse_studio_cookie(&entry).as_deref(), Some(FAKE_COOKIE));
    }

    #[test]
    fn rejects_unwrapped_or_missing_cookie() {
        assert_eq!(parse_studio_cookie("SEC::<YES>,COOK::abc"), None);
        assert_eq!(parse_studio_cookie("SEC::<YES>,EXP::<x>"), None);
        assert_eq!(parse_studio_cookie("COOK::<unterminated"), None);
    }

    #[test]
    fn parses_wine_user_reg() {
        let contents = concat!(
            "WINE REGISTRY Version 2\n",
            ";; All keys relative to HKEY_CURRENT_USER\n",
            "\n",
            "[Software\\\\Other\\\\Key] 1700000000\n",
            "\"Ignored\"=\"SEC::<YES>,COOK::<wrong>\"\n",
            "\n",
            "[Software\\\\Roblox\\\\RobloxStudioBrowser\\\\roblox.com] 1700000001\n",
            "#time=1d9c0000\n",
            "\"SEC\"=\"ignored\"\n",
            "\".ROBLOSECURITY\"=\"SEC::<YES>,EXP::<2030-01-01>,COOK::<_|WARNING:-FAKE-TEST-COOKIE|_abc123>\"\n",
            "\n",
            "[Software\\\\Later] 1700000002\n",
        );
        assert_eq!(parse_wine_user_reg(contents).as_deref(), Some(FAKE_COOKIE));
    }

    #[test]
    fn wine_registry_without_key_yields_none() {
        let contents = "[Software\\\\Other] 1\n\"x\"=\"y\"\n";
        assert_eq!(parse_wine_user_reg(contents), None);
    }

    #[test]
    fn unescapes_wine_strings() {
        assert_eq!(unescape_wine(r"a\\b"), r"a\b");
        assert_eq!(unescape_wine(r#"x\"y"#), "x\"y");
    }
}
