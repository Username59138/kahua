//! Editor settings stored in the user's config directory (not in projects).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub name: String,
    /// Unix seconds.
    pub last_opened: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Where new projects are created and scanned for.
    pub projects_dir: PathBuf,
    pub recent: Vec<RecentEntry>,
    pub snap: f32,
    pub snap_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self { projects_dir: home.join("KahuaProjects"), recent: Vec::new(), snap: 1.0, snap_enabled: true }
    }
}

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("KAHUA_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("config.json"));
    }
    dirs::config_dir().map(|d| d.join("kahua").join("config.json"))
}

impl AppConfig {
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn touch_recent(&mut self, path: &Path, name: &str) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, RecentEntry { path, name: name.to_owned(), last_opened: now() });
        self.recent.truncate(50);
        self.save();
    }

    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|r| r.path != path);
        self.save();
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "5 minutes ago" style text.
pub fn ago(timestamp: u64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    let secs = now().saturating_sub(timestamp);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}
