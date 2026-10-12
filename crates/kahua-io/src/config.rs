//! `kahua.toml`: Kahua-specific project settings (Rojo ignores this file).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KahuaConfig {
    pub project: ProjectSection,
    /// Where `kahua publish` uploads the place. Public IDs, safe to commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<PublishSection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSection {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_id: Option<u64>,
}

impl KahuaConfig {
    pub fn new(name: &str) -> Self {
        Self { project: ProjectSection { name: name.to_owned() }, publish: None }
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).with_context(|| format!("{} is not valid", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut text = String::from(
            "# Kahua project settings. Never put cookies or API keys here:\n\
             # this file is meant to be committed.\n\n",
        );
        text.push_str(&toml::to_string_pretty(self)?);
        std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
    }
}
