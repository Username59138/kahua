//! Glue between the network clients and the rest of Kahua: the uploader used
//! by `kahua_io::publish`, and the downloader used by the renderer's AssetStore.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use kahua_io::config::{self, KahuaConfig};
use kahua_io::publish::{self, AssetUploader};

use crate::{AssetKind, Creator, Credentials, OpenCloud, Session};

/// Uploads assets and publishes places through Open Cloud. Meshes are
/// converted to Roblox's mesh format first.
pub struct RobloxUploader {
    cloud: OpenCloud,
}

impl RobloxUploader {
    pub fn new(cloud: OpenCloud) -> Self {
        Self { cloud }
    }

    /// An uploader for a project: the API key from the saved credentials and
    /// the creator from the project's `kahua.toml`.
    pub fn for_project(project_dir: &Path) -> Result<Self> {
        let api_key = Credentials::load()?.api_key.context(
            "no Open Cloud API key saved. Create one in the Creator Dashboard with the Assets \
             (read/write) and universe-places (write) permissions, then add it in File > Roblox Account (or run `kahua login --api-key`)",
        )?;
        let config = KahuaConfig::read(&project_dir.join(kahua_io::project::CONFIG_FILE))?;
        let target = config.publish_target()?;
        let creator = match target.creator {
            config::Creator::User(id) => Creator::User(id),
            config::Creator::Group(id) => Creator::Group(id),
        };
        Ok(Self::new(OpenCloud::new(api_key, creator)))
    }
}

impl AssetUploader for RobloxUploader {
    fn upload(&mut self, kind: publish::AssetKind, display_name: &str, file_name: &str, bytes: &[u8]) -> Result<u64> {
        match kind {
            publish::AssetKind::Image => self.cloud.upload_asset(AssetKind::Image, display_name, file_name, bytes),
            publish::AssetKind::Mesh => {
                let mesh = to_roblox_mesh_bytes(file_name, bytes)?;
                let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("mesh");
                self.cloud.upload_asset(AssetKind::Mesh, display_name, &format!("{stem}.mesh"), &mesh)
            }
        }
    }

    fn publish_place(&mut self, universe_id: u64, place_id: u64, rbxl: &[u8]) -> Result<u64> {
        self.cloud.publish_place(universe_id, place_id, rbxl, false)
    }
}

/// Converts a .glb/.gltf/.obj file to a Roblox .mesh (a .mesh is passed through).
fn to_roblox_mesh_bytes(file_name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if file_name.to_ascii_lowercase().ends_with(".mesh") {
        return Ok(bytes.to_vec());
    }
    // The converter reads files by extension, so give it one.
    let dir = tempfile::tempdir()?;
    let name = Path::new(file_name).file_name().context("mesh file has no name")?;
    let path = dir.path().join(name);
    std::fs::write(&path, bytes)?;
    kahua_render::roblox::convert_mesh_file(&path).with_context(|| format!("failed to convert {file_name}"))
}

/// A downloader for the renderer, if a Roblox login (cookie) is saved.
pub fn saved_downloader() -> Option<kahua_render::assets::Downloader> {
    let cookie = Credentials::load().ok()?.cookie?;
    let session = Arc::new(Session::new(cookie));
    Some(Arc::new(move |id| session.download_asset(id)))
}

/// Checks that a cookie works and returns the account it belongs to.
pub fn check_cookie(cookie: &str) -> Result<crate::User> {
    let user = Session::new(cookie).current_user()?;
    if user.id == 0 {
        bail!("Roblox did not accept this login");
    }
    Ok(user)
}
