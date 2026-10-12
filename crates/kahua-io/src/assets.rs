//! Files stored inside a project (meshes, textures) and the `kahua://` URIs
//! that refer to them.
//!
//! A URI is `kahua://` followed by a path relative to the project root, with
//! forward slashes, e.g. `kahua://assets/meshes/sword.glb`. Publishing will
//! later replace these with `rbxassetid://` URIs.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const SCHEME: &str = "kahua://";
pub const MESH_FOLDER: &str = "assets/meshes";
pub const TEXTURE_FOLDER: &str = "assets/textures";

/// The project-relative path a `kahua://` URI points to, or `None` for other URIs.
pub fn relative_path(uri: &str) -> Option<&str> {
    uri.strip_prefix(SCHEME).filter(|p| !p.is_empty())
}

/// The `kahua://` URI for a project-relative path.
pub fn to_uri(relative: &str) -> String {
    format!("{SCHEME}{}", relative.replace('\\', "/").trim_start_matches('/'))
}

/// The file a `kahua://` URI points to inside `root`. Paths that try to leave
/// the project (absolute paths, `..`) are rejected.
pub fn resolve(root: &Path, uri: &str) -> Result<PathBuf> {
    let Some(relative) = relative_path(uri) else {
        bail!("{uri} is not a kahua:// URI");
    };
    let relative = Path::new(relative);
    if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("{uri} points outside the project");
    }
    Ok(root.join(relative))
}

/// Copies `source` into `folder` (relative to the project) under a unique name
/// and returns the URI of the copy. A file that is already inside that folder
/// is not copied again.
pub fn import_file(project_dir: &Path, source: &Path, folder: &str) -> Result<String> {
    let dir = project_dir.join(folder);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let stem = source.file_stem().and_then(|s| s.to_str()).context("file has no name")?;
    let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("");
    let stem = sanitize_file_stem(stem);

    if let Ok(existing) = source.canonicalize()
        && existing.parent().and_then(|p| p.canonicalize().ok()) == dir.canonicalize().ok()
    {
        let name = existing.file_name().and_then(|n| n.to_str()).context("file has no name")?;
        return Ok(to_uri(&format!("{folder}/{name}")));
    }

    let mut name = if ext.is_empty() { stem.clone() } else { format!("{stem}.{ext}") };
    let mut n = 2;
    while dir.join(&name).exists() {
        name = if ext.is_empty() { format!("{stem}-{n}") } else { format!("{stem}-{n}.{ext}") };
        n += 1;
    }
    let dest = dir.join(&name);
    fs::copy(source, &dest).with_context(|| format!("failed to copy {} into the project", source.display()))?;
    Ok(to_uri(&format!("{folder}/{name}")))
}

fn sanitize_file_stem(stem: &str) -> String {
    let clean: String =
        stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if clean.is_empty() { "asset".to_owned() } else { clean }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_and_stay_inside_the_project() {
        assert_eq!(relative_path("kahua://assets/meshes/a.glb"), Some("assets/meshes/a.glb"));
        assert_eq!(relative_path("rbxassetid://123"), None);
        assert_eq!(to_uri("assets/textures/wood.png"), "kahua://assets/textures/wood.png");
        let root = Path::new("/project");
        assert_eq!(resolve(root, "kahua://assets/a.obj").unwrap(), Path::new("/project/assets/a.obj"));
        assert!(resolve(root, "kahua://../secret.txt").is_err());
        assert!(resolve(root, "kahua:///etc/passwd").is_err());
        assert!(resolve(root, "rbxassetid://1").is_err());
    }

    #[test]
    fn import_gives_unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("my sword.obj");
        fs::write(&src, "v 0 0 0").unwrap();
        let first = import_file(dir.path(), &src, MESH_FOLDER).unwrap();
        let second = import_file(dir.path(), &src, MESH_FOLDER).unwrap();
        assert_eq!(first, "kahua://assets/meshes/my_sword.obj");
        assert_eq!(second, "kahua://assets/meshes/my_sword-2.obj");
        assert!(dir.path().join("assets/meshes/my_sword-2.obj").is_file());
    }
}
