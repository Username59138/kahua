//! `assets.lock`: the Roblox asset id of each project file that publishing uploaded.
//!
//! The file is TOML, sorted by path, so that changes show up as small diffs in
//! git. Each entry records the SHA-256 of the file that was uploaded. A file
//! whose hash no longer matches is uploaded again on the next publish.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::place::write_atomic;
use crate::publish::AssetKind;

pub const LOCK_FILE: &str = "assets.lock";

const HEADER: &str = "\
# assets.lock: Roblox asset IDs of the files in this project's assets folder.
# Kahua writes this file when you publish. Commit it to git, so everyone who
# publishes this place reuses the same uploads instead of uploading again.
# A file is uploaded again only when its SHA-256 hash changes. Delete an entry
# to force a new upload.
";

/// One uploaded file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// SHA-256 of the file bytes, in lowercase hex.
    pub hash: String,
    pub kind: AssetKind,
    pub asset_id: u64,
}

/// The contents of `assets.lock`, keyed by project-relative path with forward slashes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLock {
    #[serde(default)]
    pub assets: BTreeMap<String, LockEntry>,
}

impl AssetLock {
    /// The lock file of a project directory.
    pub fn path(project_dir: &Path) -> PathBuf {
        project_dir.join(LOCK_FILE)
    }

    /// Reads the lock of a project. A missing file is an empty lock.
    pub fn read(project_dir: &Path) -> Result<Self> {
        let path = Self::path(project_dir);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("{} is not valid", path.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The file contents, with the explanatory header.
    pub fn to_text(&self) -> Result<String> {
        Ok(format!("{HEADER}\n{}", toml::to_string_pretty(self)?))
    }

    /// Writes the lock to the project. The write is atomic.
    pub fn save(&self, project_dir: &Path) -> Result<()> {
        write_atomic(&Self::path(project_dir), self.to_text()?.as_bytes())
    }

    /// The entry for a project-relative path (forward slashes).
    pub fn get(&self, path: &str) -> Option<&LockEntry> {
        self.assets.get(path)
    }
}

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_sha256() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn lock_text_is_stable() {
        let mut lock = AssetLock::default();
        lock.assets.insert(
            "assets/textures/wood.png".into(),
            LockEntry { hash: "bb".repeat(32), kind: AssetKind::Image, asset_id: 9_000_000_002 },
        );
        lock.assets.insert(
            "assets/meshes/sword.glb".into(),
            LockEntry { hash: "aa".repeat(32), kind: AssetKind::Mesh, asset_id: 9_000_000_001 },
        );
        let text = lock.to_text().unwrap();
        let expected = format!(
            "{HEADER}\n\
             [assets.\"assets/meshes/sword.glb\"]\n\
             hash = \"{aa}\"\n\
             kind = \"mesh\"\n\
             asset_id = 9000000001\n\
             \n\
             [assets.\"assets/textures/wood.png\"]\n\
             hash = \"{bb}\"\n\
             kind = \"image\"\n\
             asset_id = 9000000002\n",
            aa = "aa".repeat(32),
            bb = "bb".repeat(32),
        );
        assert_eq!(text, expected);
        assert_eq!(AssetLock::parse(&text).unwrap(), lock);
    }

    #[test]
    fn missing_lock_is_empty_and_saves_atomically() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(AssetLock::read(dir.path()).unwrap(), AssetLock::default());

        let mut lock = AssetLock::default();
        lock.assets.insert(
            "assets/meshes/a.obj".into(),
            LockEntry { hash: sha256_hex(b"v"), kind: AssetKind::Mesh, asset_id: 7 },
        );
        lock.save(dir.path()).unwrap();
        assert_eq!(AssetLock::read(dir.path()).unwrap(), lock);
        assert!(!dir.path().join(".assets.lock.kahua-tmp").exists());
    }
}
