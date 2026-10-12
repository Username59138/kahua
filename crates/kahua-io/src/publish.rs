//! Publishing: uploading the project's `kahua://` assets to Roblox and sending
//! the place with `rbxassetid://` URIs in their place.
//!
//! Project files keep their `kahua://` URIs. Publishing never edits them. It
//! uploads the files that changed since the last publish (recorded in
//! `assets.lock`) and encodes a copy of the place with the URIs replaced.
//! Network access is behind [`AssetUploader`], so this module does no I/O
//! over the network.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rbx_dom_weak::types::{Content, ContentId, Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};
use serde::{Deserialize, Serialize};

use crate::asset_lock::{AssetLock, LockEntry, sha256_hex};
use crate::assets;
use crate::config::{KahuaConfig, PublishTarget};
use crate::place;
use crate::project::CONFIG_FILE;

/// The kinds of file that can be uploaded as Roblox assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Image,
    Mesh,
}

impl AssetKind {
    /// The kind for a file name, from its extension. `None` for types Roblox
    /// does not take (or that Kahua does not know).
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "glb" | "gltf" | "obj" | "mesh" => Some(Self::Mesh),
            "png" | "jpg" | "jpeg" | "bmp" | "tga" => Some(Self::Image),
            _ => None,
        }
    }
}

/// Uploads assets and places to Roblox. The network client implements this.
pub trait AssetUploader {
    /// Uploads one asset file. `bytes` are the file's original bytes; the
    /// implementation converts meshes to Roblox's format if needed.
    fn upload(&mut self, kind: AssetKind, display_name: &str, file_name: &str, bytes: &[u8]) -> anyhow::Result<u64>;
    /// Publishes a binary place file and returns the new version number.
    fn publish_place(&mut self, universe_id: u64, place_id: u64, rbxl: &[u8]) -> anyhow::Result<u64>;
}

/// A property that holds a `kahua://` URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    /// The instance that has the property.
    pub instance: Ref,
    pub property: String,
    /// The URI as written, e.g. `kahua://assets/meshes/sword.glb`.
    pub uri: String,
    /// The project-relative path, with forward slashes.
    pub path: String,
    /// `None` when the file type is not one that can be uploaded.
    pub kind: Option<AssetKind>,
}

/// Every Content or ContentId property in the DOM whose URI is `kahua://`.
/// Results are in tree order, then by property name.
pub fn collect_asset_refs(dom: &WeakDom) -> Vec<AssetRef> {
    let mut refs = Vec::new();
    for inst in dom.descendants() {
        let mut found: Vec<(String, String)> = inst
            .properties
            .iter()
            .filter_map(|(name, value)| {
                let uri = match value {
                    Variant::Content(content) => content.as_uri()?,
                    Variant::ContentId(id) => id.as_str(),
                    _ => return None,
                };
                uri.starts_with(assets::SCHEME).then(|| (name.as_str().to_owned(), uri.to_owned()))
            })
            .collect();
        found.sort();
        for (property, uri) in found {
            let Some(path) = asset_key(&uri) else { continue };
            refs.push(AssetRef { instance: inst.referent(), property, kind: AssetKind::from_path(&path), path, uri });
        }
    }
    refs
}

/// The lock key for a `kahua://` URI: its project-relative path, with forward slashes.
fn asset_key(uri: &str) -> Option<String> {
    assets::relative_path(uri).map(|p| p.replace('\\', "/"))
}

/// Options for [`sync_assets_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOptions {
    /// Only report what would be uploaded. Nothing is uploaded and the lock is not written.
    pub dry_run: bool,
}

/// Progress reported while publishing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    /// The file's hash matches the lock, so it is not uploaded again.
    Reused {
        path: String,
    },
    /// Dry run: the file would be uploaded.
    Planned {
        path: String,
    },
    /// About to upload the `index`-th of `total` files.
    Uploading {
        path: String,
        index: usize,
        total: usize,
    },
    Uploaded {
        path: String,
        asset_id: u64,
    },
    Failed {
        path: String,
        error: String,
    },
    /// About to send the place.
    Publishing {
        universe_id: u64,
        place_id: u64,
    },
}

/// An upload that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFailure {
    pub path: String,
    pub error: String,
}

/// What a sync did. Paths are project-relative with forward slashes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub uploaded: Vec<String>,
    /// Files whose lock entry was still valid.
    pub reused: Vec<String>,
    /// Uploads that failed. The other files were still processed, and the
    /// lock records every upload that succeeded.
    pub failed: Vec<AssetFailure>,
    /// Dry run only: the files that would be uploaded.
    pub planned: Vec<String>,
    /// Lock entries removed because the project no longer uses them.
    pub removed: Vec<String>,
}

/// Uploads the referenced files that changed since the last sync. See [`sync_assets_with`].
pub fn sync_assets(
    project_dir: &Path,
    dom: &WeakDom,
    uploader: &mut dyn AssetUploader,
    progress: &mut dyn FnMut(SyncEvent),
) -> Result<SyncReport> {
    sync_assets_with(project_dir, dom, uploader, &SyncOptions::default(), progress)
}

/// Makes sure every `kahua://` asset the DOM uses is uploaded and recorded in
/// `assets.lock`.
///
/// Missing files, unsupported file types and paths outside the project stop
/// the sync before anything is uploaded. After that, each file is uploaded
/// only if it is not in the lock or its hash changed, and the lock is saved
/// after each successful upload. A failed upload is reported in
/// [`SyncReport::failed`] and the rest of the files are still processed.
pub fn sync_assets_with(
    project_dir: &Path,
    dom: &WeakDom,
    uploader: &mut dyn AssetUploader,
    options: &SyncOptions,
    progress: &mut dyn FnMut(SyncEvent),
) -> Result<SyncReport> {
    let prepared = prepare(project_dir, dom)?;
    let mut lock = AssetLock::read(project_dir)?;
    let mut report = SyncReport::default();

    let mut to_upload = Vec::new();
    for (path, file) in &prepared {
        match lock.get(path) {
            Some(entry) if entry.hash == file.hash && entry.kind == file.kind => {
                report.reused.push(path.clone());
                progress(SyncEvent::Reused { path: path.clone() });
            }
            _ => to_upload.push(path.clone()),
        }
    }

    // Entries for files the project no longer uses are dropped, so the lock matches the project.
    report.removed = lock.assets.keys().filter(|k| !prepared.contains_key(*k)).cloned().collect();
    for path in &report.removed {
        lock.assets.remove(path);
    }
    let mut dirty = !report.removed.is_empty();

    if options.dry_run {
        for path in &to_upload {
            progress(SyncEvent::Planned { path: path.clone() });
        }
        report.planned = to_upload;
        return Ok(report);
    }

    let total = to_upload.len();
    for (i, path) in to_upload.iter().enumerate() {
        let file = &prepared[path];
        progress(SyncEvent::Uploading { path: path.clone(), index: i + 1, total });

        let upload = read_asset(project_dir, &file.uri).and_then(|bytes| {
            let hash = sha256_hex(&bytes);
            let file_name = file_name_of(path);
            let display_name = file_stem_of(path);
            let id = uploader.upload(file.kind, &display_name, &file_name, &bytes)?;
            Ok((hash, id))
        });
        match upload {
            Ok((hash, asset_id)) => {
                lock.assets.insert(path.clone(), LockEntry { hash, kind: file.kind, asset_id });
                lock.save(project_dir).with_context(|| {
                    format!("{path} was uploaded as asset {asset_id} but assets.lock could not be saved")
                })?;
                dirty = false;
                report.uploaded.push(path.clone());
                progress(SyncEvent::Uploaded { path: path.clone(), asset_id });
            }
            Err(e) => {
                let error = format!("{e:#}");
                progress(SyncEvent::Failed { path: path.clone(), error: error.clone() });
                report.failed.push(AssetFailure { path: path.clone(), error });
            }
        }
    }

    if dirty {
        lock.save(project_dir)?;
    }
    Ok(report)
}

/// A file the DOM uses, checked and ready to upload.
struct Prepared {
    uri: String,
    kind: AssetKind,
    hash: String,
}

/// Hashes every referenced file. Returns an error that lists every problem,
/// so nothing gets uploaded when the project is incomplete.
fn prepare(project_dir: &Path, dom: &WeakDom) -> Result<BTreeMap<String, Prepared>> {
    // Group the references by file: many properties can use the same file.
    let mut usage: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    for r in collect_asset_refs(dom) {
        let place = format!("{}.{}", dom.full_path_of(r.instance, "."), r.property);
        usage.entry(r.path).or_insert_with(|| (r.uri, BTreeSet::new())).1.insert(place);
    }

    let mut prepared = BTreeMap::new();
    let mut problems = Vec::new();
    for (path, (uri, used_by)) in usage {
        let used_by = used_by.into_iter().collect::<Vec<_>>().join(", ");
        let Some(kind) = AssetKind::from_path(&path) else {
            problems.push(format!(
                "{uri}: unsupported file type; meshes are .glb .gltf .obj .mesh, images are .png .jpg .jpeg .bmp .tga. Used by {used_by}"
            ));
            continue;
        };
        match read_asset(project_dir, &uri) {
            Ok(bytes) => {
                prepared.insert(path, Prepared { uri, kind, hash: sha256_hex(&bytes) });
            }
            Err(e) => problems.push(format!("{uri}: {e:#}. Used by {used_by}")),
        }
    }
    if !problems.is_empty() {
        bail!("cannot upload assets:\n  {}", problems.join("\n  "));
    }
    Ok(prepared)
}

/// Reads the file a `kahua://` URI points to. The path must stay inside the project.
fn read_asset(project_dir: &Path, uri: &str) -> Result<Vec<u8>> {
    let path = assets::resolve(project_dir, uri)?;
    if !path.is_file() {
        bail!("file not found");
    }
    fs::read(&path).with_context(|| format!("cannot read {}", path.display()))
}

fn file_name_of(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

fn file_stem_of(path: &str) -> String {
    Path::new(path).file_stem().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Returns a copy of `dom` in which every `kahua://` URI whose file is in the
/// lock, and unchanged since then, is replaced by `rbxassetid://<id>`. The
/// Content or ContentId type of each property is kept. The original DOM is
/// not changed.
///
/// The warnings list the URIs that stay local: they are not in the lock, the
/// file changed since it was uploaded, or the file is missing.
pub fn resolve_for_roblox(dom: &WeakDom, lock: &AssetLock, project_dir: &Path) -> Result<(WeakDom, Vec<String>)> {
    let mut copy = copy_dom(dom);
    let mut resolver = Resolver { lock, project_dir, cache: HashMap::new() };
    let mut unresolved: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();

    for r in collect_asset_refs(&copy) {
        let place = format!("{}.{}", copy.full_path_of(r.instance, "."), r.property);
        match resolver.asset_id(&r.uri) {
            Ok(id) => {
                let inst = copy.get_by_ref_mut(r.instance).expect("ref came from this DOM");
                for (name, value) in inst.properties.iter_mut() {
                    if name.as_str() != r.property {
                        continue;
                    }
                    let url = format!("rbxassetid://{id}");
                    match value {
                        Variant::Content(_) => *value = Variant::Content(Content::from_uri(url)),
                        Variant::ContentId(_) => *value = Variant::ContentId(ContentId::from(url)),
                        _ => {}
                    }
                }
            }
            Err(reason) => {
                unresolved.entry(r.uri).or_insert_with(|| (reason, BTreeSet::new())).1.insert(place);
            }
        }
    }

    let warnings = unresolved
        .into_iter()
        .map(|(uri, (reason, used_by))| {
            let used_by = used_by.into_iter().collect::<Vec<_>>().join(", ");
            format!("{uri} is not published ({reason}), so it stays a local URI in the place. Used by {used_by}")
        })
        .collect();
    Ok((copy, warnings))
}

/// Finds the asset id of a `kahua://` URI, checking the lock and the file's hash.
struct Resolver<'a> {
    lock: &'a AssetLock,
    project_dir: &'a Path,
    cache: HashMap<String, std::result::Result<u64, String>>,
}

impl Resolver<'_> {
    fn asset_id(&mut self, uri: &str) -> std::result::Result<u64, String> {
        if let Some(result) = self.cache.get(uri) {
            return result.clone();
        }
        let result = self.lookup(uri);
        self.cache.insert(uri.to_owned(), result.clone());
        result
    }

    fn lookup(&self, uri: &str) -> std::result::Result<u64, String> {
        let Some(path) = asset_key(uri) else {
            return Err("not a kahua:// asset".into());
        };
        let Some(entry) = self.lock.get(&path) else {
            return Err("not uploaded yet, run publish".into());
        };
        let bytes = read_asset(self.project_dir, uri).map_err(|e| format!("{e:#}"))?;
        if sha256_hex(&bytes) != entry.hash {
            return Err("the file changed since it was uploaded, run publish".into());
        }
        Ok(entry.asset_id)
    }
}

/// A copy of the DOM with the same referents, so references between
/// instances stay valid in the copy.
fn copy_dom(dom: &WeakDom) -> WeakDom {
    let root = dom.root();
    let mut copy = WeakDom::new(builder_of(dom, root.referent()));
    copy_children(dom, root.referent(), &mut copy);
    copy
}

fn copy_children(src: &WeakDom, parent: Ref, dst: &mut WeakDom) {
    for &child in src.get_by_ref(parent).expect("parent is in the DOM").children() {
        dst.insert(parent, builder_of(src, child));
        copy_children(src, child, dst);
    }
}

fn builder_of(src: &WeakDom, referent: Ref) -> InstanceBuilder {
    let inst = src.get_by_ref(referent).expect("referent is in the DOM");
    InstanceBuilder::new(inst.class)
        .with_name(inst.name.clone())
        .with_referent(referent)
        .with_properties(inst.properties.iter().map(|(k, v)| (*k, v.clone())))
}

/// The result of a publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    pub target: PublishTarget,
    /// The new version of the place.
    pub version: u64,
    pub sync: SyncReport,
    /// Assets that stayed local. Empty after a successful sync.
    pub warnings: Vec<String>,
}

/// Uploads the assets that changed, then sends the place with Roblox asset
/// URIs. Requires `[publish]` in `kahua.toml`. The place is not sent if any
/// asset failed to upload.
pub fn publish(
    project_dir: &Path,
    dom: &WeakDom,
    uploader: &mut dyn AssetUploader,
    progress: &mut dyn FnMut(SyncEvent),
) -> Result<PublishReport> {
    let config_path = project_dir.join(CONFIG_FILE);
    if !config_path.is_file() {
        bail!("{} has no {CONFIG_FILE}, so it cannot be published", project_dir.display());
    }
    let target = KahuaConfig::read(&config_path)?.publish_target()?;

    let sync = sync_assets(project_dir, dom, uploader, progress)?;
    if !sync.failed.is_empty() {
        let list = sync.failed.iter().map(|f| format!("  {}: {}", f.path, f.error)).collect::<Vec<_>>().join("\n");
        bail!(
            "{} asset(s) failed to upload, so the place was not published. The assets that uploaded are \
             recorded in assets.lock; run publish again to upload the rest:\n{list}",
            sync.failed.len()
        );
    }

    let lock = AssetLock::read(project_dir)?;
    let (resolved, warnings) = resolve_for_roblox(dom, &lock, project_dir)?;
    let rbxl = place::encode(&resolved, false)?;
    progress(SyncEvent::Publishing { universe_id: target.universe_id, place_id: target.place_id });
    let version = uploader.publish_place(target.universe_id, target.place_id, &rbxl)?;
    Ok(PublishReport { target, version, sync, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Creator;
    use rbx_dom_weak::types::Vector3;

    /// Records what it was asked to upload. Ids start at 1001 and grow.
    #[derive(Default)]
    struct FakeUploader {
        /// (kind, display name, file name) of each upload call, in order.
        calls: Vec<(AssetKind, String, String)>,
        /// Fail the Nth upload call (1-based).
        fail_on: Option<usize>,
        places: Vec<(u64, u64, Vec<u8>)>,
    }

    impl AssetUploader for FakeUploader {
        fn upload(&mut self, kind: AssetKind, display_name: &str, file_name: &str, _bytes: &[u8]) -> Result<u64> {
            self.calls.push((kind, display_name.to_owned(), file_name.to_owned()));
            if self.fail_on == Some(self.calls.len()) {
                bail!("network is down");
            }
            Ok(1000 + self.calls.len() as u64)
        }

        fn publish_place(&mut self, universe_id: u64, place_id: u64, rbxl: &[u8]) -> Result<u64> {
            self.places.push((universe_id, place_id, rbxl.to_vec()));
            Ok(self.places.len() as u64)
        }
    }

    const CONFIG: &str =
        "[project]\nname = \"test\"\n\n[publish]\nuniverse_id = 11\nplace_id = 22\ncreator_user_id = 33\n";

    /// A project with three assets, and a DOM that uses them.
    fn project() -> (tempfile::TempDir, WeakDom) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("kahua.toml"), CONFIG).unwrap();
        write(dir.path(), "assets/meshes/sword.glb", b"sword v1");
        write(dir.path(), "assets/meshes/shield.obj", b"shield");
        write(dir.path(), "assets/textures/crate.png", b"crate");

        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let workspace = dom.insert(root, InstanceBuilder::new("Workspace").with_name("Workspace"));
        dom.insert(
            workspace,
            InstanceBuilder::new("MeshPart")
                .with_name("Sword")
                .with_property("MeshContent", Content::from_uri("kahua://assets/meshes/sword.glb"))
                .with_property("Size", Vector3::new(1.0, 2.0, 3.0)),
        );
        dom.insert(
            workspace,
            InstanceBuilder::new("MeshPart")
                .with_name("Shield")
                .with_property("MeshContent", Content::from_uri("kahua://assets/meshes/shield.obj"))
                .with_property("TextureID", ContentId::from("kahua://assets/textures/crate.png")),
        );
        dom.insert(
            workspace,
            InstanceBuilder::new("Decal")
                .with_name("Crate")
                .with_property("Texture", Content::from_uri("kahua://assets/textures/crate.png")),
        );
        (dir, dom)
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn sync(dir: &Path, dom: &WeakDom, uploader: &mut FakeUploader) -> Result<SyncReport> {
        sync_assets(dir, dom, uploader, &mut |_| {})
    }

    fn property<'a>(dom: &'a WeakDom, name: &str, prop: &str) -> &'a Variant {
        let inst = dom.descendants().find(|i| i.name == name).expect("instance exists");
        inst.properties.iter().find(|(k, _)| k.as_str() == prop).expect("property exists").1
    }

    #[test]
    fn collects_every_kahua_reference_with_its_kind() {
        let (_dir, dom) = project();
        let refs = collect_asset_refs(&dom);
        let found: Vec<(&str, &str, Option<AssetKind>)> =
            refs.iter().map(|r| (r.property.as_str(), r.path.as_str(), r.kind)).collect();
        assert_eq!(
            found,
            vec![
                ("MeshContent", "assets/meshes/sword.glb", Some(AssetKind::Mesh)),
                ("MeshContent", "assets/meshes/shield.obj", Some(AssetKind::Mesh)),
                ("TextureID", "assets/textures/crate.png", Some(AssetKind::Image)),
                ("Texture", "assets/textures/crate.png", Some(AssetKind::Image)),
            ]
        );
        assert_eq!(AssetKind::from_path("A/B.PNG"), Some(AssetKind::Image));
        assert_eq!(AssetKind::from_path("x.fbx"), None);
    }

    #[test]
    fn first_sync_uploads_everything_and_the_second_uploads_nothing() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();

        let first = sync(dir.path(), &dom, &mut uploader).unwrap();
        assert_eq!(
            first.uploaded,
            ["assets/meshes/shield.obj", "assets/meshes/sword.glb", "assets/textures/crate.png"]
        );
        assert!(first.reused.is_empty() && first.failed.is_empty());
        assert_eq!(uploader.calls.len(), 3);
        assert_eq!(uploader.calls[0], (AssetKind::Mesh, "shield".into(), "shield.obj".into()));

        let lock = AssetLock::read(dir.path()).unwrap();
        assert_eq!(lock.assets.len(), 3);
        assert_eq!(lock.get("assets/meshes/sword.glb").unwrap().asset_id, 1002);

        let second = sync(dir.path(), &dom, &mut uploader).unwrap();
        assert!(second.uploaded.is_empty());
        assert_eq!(second.reused.len(), 3);
        assert_eq!(uploader.calls.len(), 3);
    }

    #[test]
    fn changing_one_file_uploads_only_that_file() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();
        sync(dir.path(), &dom, &mut uploader).unwrap();

        write(dir.path(), "assets/meshes/sword.glb", b"sword v2");
        let report = sync(dir.path(), &dom, &mut uploader).unwrap();
        assert_eq!(report.uploaded, ["assets/meshes/sword.glb"]);
        assert_eq!(report.reused.len(), 2);
        assert_eq!(uploader.calls.len(), 4);
        let lock = AssetLock::read(dir.path()).unwrap();
        assert_eq!(lock.get("assets/meshes/sword.glb").unwrap().asset_id, 1004);
        assert_eq!(lock.get("assets/meshes/sword.glb").unwrap().hash, sha256_hex(b"sword v2"));
    }

    #[test]
    fn a_failure_halfway_keeps_the_earlier_ids() {
        let (dir, dom) = project();
        // Uploads run in path order: shield, sword, crate. The second one fails.
        let mut uploader = FakeUploader { fail_on: Some(2), ..Default::default() };
        let report = sync(dir.path(), &dom, &mut uploader).unwrap();
        assert_eq!(report.uploaded, ["assets/meshes/shield.obj", "assets/textures/crate.png"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, "assets/meshes/sword.glb");
        assert!(report.failed[0].error.contains("network is down"));

        let lock = AssetLock::read(dir.path()).unwrap();
        assert!(lock.get("assets/meshes/shield.obj").is_some());
        assert!(lock.get("assets/textures/crate.png").is_some());
        assert!(lock.get("assets/meshes/sword.glb").is_none());

        // The next run uploads only what failed.
        let mut retry = FakeUploader::default();
        let report = sync(dir.path(), &dom, &mut retry).unwrap();
        assert_eq!(report.uploaded, ["assets/meshes/sword.glb"]);
        assert_eq!(retry.calls.len(), 1);
    }

    #[test]
    fn dry_run_reports_without_uploading_or_writing_the_lock() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();
        let mut events = Vec::new();
        let options = SyncOptions { dry_run: true };
        let report = sync_assets_with(dir.path(), &dom, &mut uploader, &options, &mut |e| events.push(e)).unwrap();
        assert_eq!(report.planned.len(), 3);
        assert!(report.uploaded.is_empty());
        assert!(uploader.calls.is_empty());
        assert!(!AssetLock::path(dir.path()).exists());
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], SyncEvent::Planned { .. }));
    }

    #[test]
    fn a_missing_file_stops_before_anything_is_uploaded() {
        let (dir, dom) = project();
        fs::remove_file(dir.path().join("assets/meshes/sword.glb")).unwrap();
        let mut uploader = FakeUploader::default();
        let err = sync(dir.path(), &dom, &mut uploader).unwrap_err().to_string();
        assert!(err.contains("kahua://assets/meshes/sword.glb"), "{err}");
        assert!(err.contains("file not found"), "{err}");
        assert!(err.contains("Workspace.Sword.MeshContent"), "{err}");
        assert!(uploader.calls.is_empty());
    }

    #[test]
    fn an_unsupported_file_type_stops_the_sync() {
        let (dir, dom) = project();
        let mut dom = dom;
        let workspace = dom.descendants().find(|i| i.name == "Workspace").unwrap().referent();
        dom.insert(
            workspace,
            InstanceBuilder::new("MeshPart")
                .with_name("Car")
                .with_property("MeshContent", Content::from_uri("kahua://assets/meshes/car.fbx")),
        );
        write(dir.path(), "assets/meshes/car.fbx", b"fbx");
        let mut uploader = FakeUploader::default();
        let err = sync(dir.path(), &dom, &mut uploader).unwrap_err().to_string();
        assert!(err.contains("unsupported file type") && err.contains("car.fbx"), "{err}");
        assert!(uploader.calls.is_empty());
    }

    #[test]
    fn substitution_keeps_variant_types_and_leaves_the_original_alone() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();
        sync(dir.path(), &dom, &mut uploader).unwrap();
        let lock = AssetLock::read(dir.path()).unwrap();

        let (resolved, warnings) = resolve_for_roblox(&dom, &lock, dir.path()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let sword = lock.get("assets/meshes/sword.glb").unwrap().asset_id;
        let crate_id = lock.get("assets/textures/crate.png").unwrap().asset_id;
        assert_eq!(
            property(&resolved, "Sword", "MeshContent"),
            &Variant::Content(Content::from_uri(format!("rbxassetid://{sword}")))
        );
        assert_eq!(
            property(&resolved, "Shield", "TextureID"),
            &Variant::ContentId(ContentId::from(format!("rbxassetid://{crate_id}")))
        );
        assert_eq!(
            property(&resolved, "Crate", "Texture"),
            &Variant::Content(Content::from_uri(format!("rbxassetid://{crate_id}")))
        );
        // Other properties and instance refs survive the copy.
        assert_eq!(property(&resolved, "Sword", "Size"), &Variant::Vector3(Vector3::new(1.0, 2.0, 3.0)));
        assert_eq!(resolved.descendants().count(), dom.descendants().count());
        for inst in dom.descendants() {
            assert!(resolved.get_by_ref(inst.referent()).is_some());
        }

        // The original still has the project's URIs.
        assert_eq!(
            property(&dom, "Sword", "MeshContent"),
            &Variant::Content(Content::from_uri("kahua://assets/meshes/sword.glb"))
        );
    }

    #[test]
    fn unresolved_references_stay_local_and_are_reported() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();
        sync(dir.path(), &dom, &mut uploader).unwrap();
        let lock = AssetLock::read(dir.path()).unwrap();

        // Changing a file after the upload means its lock entry no longer applies.
        write(dir.path(), "assets/meshes/sword.glb", b"sword v2");
        let (resolved, warnings) = resolve_for_roblox(&dom, &lock, dir.path()).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("kahua://assets/meshes/sword.glb") && warnings[0].contains("changed"));
        assert_eq!(
            property(&resolved, "Sword", "MeshContent"),
            &Variant::Content(Content::from_uri("kahua://assets/meshes/sword.glb"))
        );

        // Nothing in the lock at all: every reference stays local.
        let (_, warnings) = resolve_for_roblox(&dom, &AssetLock::default(), dir.path()).unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn publish_sends_a_place_with_asset_uris() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader::default();
        let report = publish(dir.path(), &dom, &mut uploader, &mut |_| {}).unwrap();
        assert_eq!(report.version, 1);
        assert_eq!(report.target.creator, Creator::User(33));
        assert!(report.warnings.is_empty());
        assert_eq!(uploader.calls.len(), 3);

        let (universe, place, rbxl) = &uploader.places[0];
        assert_eq!((*universe, *place), (11, 22));
        let decoded = place::decode(rbxl).unwrap();
        let lock = AssetLock::read(dir.path()).unwrap();
        let sword = lock.get("assets/meshes/sword.glb").unwrap().asset_id;
        assert_eq!(
            property(&decoded, "Sword", "MeshContent"),
            &Variant::Content(Content::from_uri(format!("rbxassetid://{sword}")))
        );

        // The project files keep their kahua:// URIs.
        let text = fs::read_to_string(dir.path().join("kahua.toml")).unwrap();
        assert_eq!(text, CONFIG);
    }

    #[test]
    fn publish_stops_when_an_upload_fails() {
        let (dir, dom) = project();
        let mut uploader = FakeUploader { fail_on: Some(1), ..Default::default() };
        let err = publish(dir.path(), &dom, &mut uploader, &mut |_| {}).unwrap_err().to_string();
        assert!(err.contains("failed to upload") && err.contains("shield.obj"), "{err}");
        assert!(uploader.places.is_empty());
    }

    #[test]
    fn publish_checks_the_config_before_uploading() {
        let (dir, dom) = project();
        fs::write(dir.path().join("kahua.toml"), "[project]\nname = \"test\"\n").unwrap();
        let mut uploader = FakeUploader::default();
        let err = publish(dir.path(), &dom, &mut uploader, &mut |_| {}).unwrap_err().to_string();
        assert!(err.contains("universe_id") && err.contains("[publish]"), "{err}");
        assert!(uploader.calls.is_empty() && uploader.places.is_empty());

        let no_config = tempfile::tempdir().unwrap();
        assert!(publish(no_config.path(), &dom, &mut uploader, &mut |_| {}).is_err());
    }
}
