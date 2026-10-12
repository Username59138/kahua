//! `kahua.toml`: Kahua-specific project settings (Rojo ignores this file).

use std::path::Path;

use anyhow::{Context, Result, bail};
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
    /// Publish as this user. Set exactly one of the two creator fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_user_id: Option<u64>,
    /// Publish as this group. Set exactly one of the two creator fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_group_id: Option<u64>,
}

/// Who a place is published as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creator {
    User(u64),
    Group(u64),
}

/// The validated `[publish]` settings that a publish needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishTarget {
    pub universe_id: u64,
    pub place_id: u64,
    pub creator: Creator,
}

const PUBLISH_EXAMPLE: &str = "[publish]\nuniverse_id = 1234567890\nplace_id = 1234567890\ncreator_user_id = 123";

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

    /// Checks the `[publish]` section and returns what publishing needs.
    pub fn publish_target(&self) -> Result<PublishTarget> {
        let publish = self.publish.clone().unwrap_or_default();
        let (Some(universe_id), Some(place_id)) = (publish.universe_id, publish.place_id) else {
            bail!(
                "kahua.toml needs universe_id and place_id in [publish] to publish. Add them, for example:\n\n{PUBLISH_EXAMPLE}"
            );
        };
        let creator = match (publish.creator_user_id, publish.creator_group_id) {
            (Some(id), None) => Creator::User(id),
            (None, Some(id)) => Creator::Group(id),
            (Some(_), Some(_)) => {
                bail!("[publish] sets both creator_user_id and creator_group_id. Set only one of them.")
            }
            (None, None) => bail!(
                "[publish] needs creator_user_id (publish as a user) or creator_group_id (publish as a group). \
                 Set one, for example:\n\n{PUBLISH_EXAMPLE}"
            ),
        };
        if matches!(creator, Creator::User(0) | Creator::Group(0)) {
            bail!("creator_user_id and creator_group_id must be positive numbers");
        }
        Ok(PublishTarget { universe_id, place_id, creator })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> KahuaConfig {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn publish_target_accepts_a_user_or_a_group() {
        let user = parse("[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\nplace_id = 2\ncreator_user_id = 3\n");
        assert_eq!(
            user.publish_target().unwrap(),
            PublishTarget { universe_id: 1, place_id: 2, creator: Creator::User(3) }
        );
        let group = parse("[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\nplace_id = 2\ncreator_group_id = 4\n");
        assert_eq!(group.publish_target().unwrap().creator, Creator::Group(4));
    }

    #[test]
    fn publish_target_explains_what_is_missing() {
        let none = parse("[project]\nname = \"a\"\n");
        let err = none.publish_target().unwrap_err().to_string();
        assert!(err.contains("universe_id and place_id") && err.contains("[publish]"), "{err}");

        let no_place = parse("[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\ncreator_user_id = 3\n");
        assert!(no_place.publish_target().unwrap_err().to_string().contains("universe_id and place_id"));

        let no_creator = parse("[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\nplace_id = 2\n");
        let err = no_creator.publish_target().unwrap_err().to_string();
        assert!(err.contains("creator_user_id") && err.contains("creator_group_id"), "{err}");

        let both = parse(
            "[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\nplace_id = 2\ncreator_user_id = 3\ncreator_group_id = 4\n",
        );
        assert!(both.publish_target().unwrap_err().to_string().contains("both"));

        let zero = parse("[project]\nname = \"a\"\n[publish]\nuniverse_id = 1\nplace_id = 2\ncreator_user_id = 0\n");
        assert!(zero.publish_target().unwrap_err().to_string().contains("positive"));
    }
}
