//! Meshes and textures stored in a project, loaded from glTF/GLB, OBJ, Roblox
//! mesh and image files. Parts refer to project files with `kahua://` URIs (see
//! `kahua_io::assets`) and to Roblox assets with `rbxassetid://` URIs or
//! roblox.com asset links. Roblox assets are downloaded in the background
//! through a downloader the caller provides, and cached on disk.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use glam::{Mat3, Mat4, Vec3};
use image::RgbaImage;
use image::imageops::{self, FilterType};
use kahua_io::assets as uri;

use crate::mesh::Vertex;
use crate::roblox::{self, Content};

/// Largest width or height of a texture that is uploaded to the GPU. Bigger
/// images are scaled down when they are loaded.
pub const MAX_TEXTURE_SIZE: u32 = 2048;

/// A decoded image with its mip chain, ready to upload. Pixels are RGBA8,
/// sRGB-encoded.
#[derive(Debug)]
pub struct LoadedImage {
    pub width: u32,
    pub height: u32,
    /// Level 0 is the full image; each further level is half the size.
    pub levels: Vec<ImageLevel>,
}

#[derive(Debug)]
pub struct ImageLevel {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl LoadedImage {
    /// Builds an image and its mip chain, scaling it down to `MAX_TEXTURE_SIZE` first.
    pub fn from_rgba(image: RgbaImage) -> Self {
        let (w, h) = image.dimensions();
        let image = if w.max(h) > MAX_TEXTURE_SIZE {
            let scale = MAX_TEXTURE_SIZE as f32 / w.max(h) as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            imageops::resize(&image, nw, nh, FilterType::Triangle)
        } else {
            image
        };
        let (width, height) = image.dimensions();
        let mut levels = vec![ImageLevel { width, height, rgba: image.as_raw().clone() }];
        let mut current = image;
        while current.width() > 1 || current.height() > 1 {
            let nw = (current.width() / 2).max(1);
            let nh = (current.height() / 2).max(1);
            current = imageops::resize(&current, nw, nh, FilterType::Triangle);
            levels.push(ImageLevel { width: nw, height: nh, rgba: current.as_raw().clone() });
        }
        Self { width, height, levels }
    }
}

/// One draw call's worth of geometry, with the texture it was authored with.
#[derive(Debug)]
pub struct MeshPrimitive {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// The base color texture from the file, if any.
    pub texture: Option<Arc<LoadedImage>>,
}

/// A mesh file's geometry. It is centered on its bounding box, so a part's
/// CFrame position is the center of the mesh.
#[derive(Debug)]
pub struct LoadedMesh {
    pub primitives: Vec<MeshPrimitive>,
    /// Size of the bounding box in the file's own units (before any scaling).
    pub native_size: Vec3,
}

impl LoadedMesh {
    /// Builds a mesh from its primitives. With `recenter`, positions are moved so
    /// the bounding box is centered on the origin.
    pub(crate) fn from_primitives(primitives: Vec<MeshPrimitive>, recenter: bool) -> Result<Self> {
        let primitives: Vec<MeshPrimitive> = primitives.into_iter().filter(|p| !p.indices.is_empty()).collect();
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for v in primitives.iter().flat_map(|p| &p.vertices) {
            let p = Vec3::from(v.position);
            min = min.min(p);
            max = max.max(p);
        }
        if !min.is_finite() || !max.is_finite() {
            bail!("the mesh has no triangles");
        }
        let mut primitives = primitives;
        if recenter {
            let center = (min + max) * 0.5;
            for v in primitives.iter_mut().flat_map(|p| p.vertices.iter_mut()) {
                v.position = (Vec3::from(v.position) - center).to_array();
            }
        }
        Ok(Self { primitives, native_size: max - min })
    }
}

/// Largest number of Roblox asset downloads running at once.
pub const MAX_DOWNLOADS: usize = 4;

/// The function that fetches a Roblox asset's bytes by its ID. It runs on a
/// background thread, so it may block.
pub type Downloader = Arc<dyn Fn(u64) -> Result<Vec<u8>> + Send + Sync>;

type DownloadFn = dyn Fn(u64) -> Result<Vec<u8>> + Send + Sync;

/// Whether an asset can be drawn now.
#[derive(Debug)]
pub enum AssetState<T> {
    Ready(Arc<T>),
    /// Being downloaded in the background. `AssetStore::poll` returns true when it finishes.
    Pending,
    /// Not cached and no downloader is set, so it will never load. Parts using
    /// it are drawn as boxes without a warning.
    Unavailable,
    /// Could not be loaded. The message says why.
    Failed(String),
}

/// Loads and caches meshes and images from a project folder. Each file is read
/// again when its modification time changes. Roblox assets are downloaded in the
/// background (see `set_downloader`) and cached on disk.
pub struct AssetStore {
    root: PathBuf,
    meshes: HashMap<PathBuf, Cached<LoadedMesh>>,
    images: HashMap<PathBuf, Cached<LoadedImage>>,
    cache_dir: PathBuf,
    downloader: Option<Downloader>,
    remote: HashMap<(u64, RemoteKind), Slot>,
    queue: VecDeque<(u64, RemoteKind)>,
    running: usize,
    /// Union geometry stored inline in a part, decoded once per distinct data.
    inline_unions: HashMap<u64, Arc<LoadedMesh>>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
}

struct Cached<T> {
    modified: Option<SystemTime>,
    value: Arc<T>,
}

/// What a Roblox asset is used as. The same asset ID can be read as different kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RemoteKind {
    Mesh,
    Image,
    Union,
}

#[derive(Clone)]
enum Decoded {
    Mesh(Arc<LoadedMesh>),
    Image(Arc<LoadedImage>),
}

#[derive(Clone)]
enum Slot {
    Pending,
    Ready(Decoded),
    Failed(String),
}

/// Outcome of a remote lookup, before it becomes an `AssetState`.
enum Status {
    Ready(Decoded),
    Pending,
    Unavailable,
    Failed(String),
}

struct Done {
    key: (u64, RemoteKind),
    result: std::result::Result<Decoded, String>,
}

impl AssetStore {
    /// A store that resolves `kahua://` URIs against `root`, the project directory.
    /// Downloaded Roblox assets are cached in `<root>/.kahua/cache/assets`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let cache_dir = root.join(".kahua").join("cache").join("assets");
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            root,
            meshes: HashMap::new(),
            images: HashMap::new(),
            cache_dir,
            downloader: None,
            remote: HashMap::new(),
            queue: VecDeque::new(),
            running: 0,
            inline_unions: HashMap::new(),
            done_tx,
            done_rx,
        }
    }

    /// Stores downloaded Roblox assets in `dir` instead of the default cache folder.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = dir.into();
        self
    }

    /// Sets the function that downloads Roblox assets. Without one, only assets
    /// already in the cache can load.
    pub fn set_downloader(&mut self, downloader: Arc<dyn Fn(u64) -> anyhow::Result<Vec<u8>> + Send + Sync>) {
        self.downloader = Some(downloader);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The mesh at a URI. A Roblox asset that is still downloading gives an error
    /// until `poll` reports it finished. Never waits for the network.
    pub fn mesh(&mut self, mesh_uri: &str) -> Result<Arc<LoadedMesh>> {
        match self.mesh_state(mesh_uri) {
            AssetState::Ready(mesh) => Ok(mesh),
            AssetState::Pending => bail!("{mesh_uri} is still downloading"),
            AssetState::Unavailable => bail!("{mesh_uri} is not cached and no downloader is set"),
            AssetState::Failed(e) => bail!("{e}"),
        }
    }

    /// The image at a URI, as for `mesh`.
    pub fn image(&mut self, image_uri: &str) -> Result<Arc<LoadedImage>> {
        match self.image_state(image_uri) {
            AssetState::Ready(image) => Ok(image),
            AssetState::Pending => bail!("{image_uri} is still downloading"),
            AssetState::Unavailable => bail!("{image_uri} is not cached and no downloader is set"),
            AssetState::Failed(e) => bail!("{e}"),
        }
    }

    /// Whether the mesh at a URI can be drawn now. The first request for a
    /// Roblox asset starts its download.
    pub fn mesh_state(&mut self, mesh_uri: &str) -> AssetState<LoadedMesh> {
        match roblox::asset_id(mesh_uri) {
            Some(id) => match self.remote_status(id, RemoteKind::Mesh) {
                Status::Ready(Decoded::Mesh(mesh)) => AssetState::Ready(mesh),
                Status::Ready(Decoded::Image(_)) => AssetState::Failed(format!("{mesh_uri} is not a mesh")),
                other => other.into_state(),
            },
            None => match self.load_local_mesh(mesh_uri) {
                Ok(mesh) => AssetState::Ready(mesh),
                Err(e) => AssetState::Failed(format!("{e:#}")),
            },
        }
    }

    /// Whether the image at a URI can be drawn now, as for `mesh_state`.
    pub fn image_state(&mut self, image_uri: &str) -> AssetState<LoadedImage> {
        match roblox::asset_id(image_uri) {
            Some(id) => match self.remote_status(id, RemoteKind::Image) {
                Status::Ready(Decoded::Image(image)) => AssetState::Ready(image),
                Status::Ready(Decoded::Mesh(_)) => AssetState::Failed(format!("{image_uri} is not an image")),
                other => other.into_state(),
            },
            None => match self.load_local_image(image_uri) {
                Ok(image) => AssetState::Ready(image),
                Err(e) => AssetState::Failed(format!("{e:#}")),
            },
        }
    }

    /// The geometry of union data downloaded from a UnionOperation's AssetId.
    /// The first request starts the download, as for `mesh_state`.
    pub fn union_state(&mut self, union_uri: &str) -> AssetState<LoadedMesh> {
        match roblox::asset_id(union_uri) {
            Some(id) => match self.remote_status(id, RemoteKind::Union) {
                Status::Ready(Decoded::Mesh(mesh)) => AssetState::Ready(mesh),
                Status::Ready(Decoded::Image(_)) => AssetState::Failed(format!("{union_uri} is not union data")),
                other => other.into_state(),
            },
            None => AssetState::Failed(format!("{union_uri} is not a Roblox asset URI")),
        }
    }

    /// Downloads that are running or waiting to start.
    pub fn pending(&self) -> usize {
        self.running + self.queue.len()
    }

    /// Collects the downloads that finished since the last call and starts queued
    /// ones. Returns true if any asset finished, loaded or failed, so the caller
    /// should extract the scene again.
    pub fn poll(&mut self) -> bool {
        let mut finished = false;
        while let Ok(done) = self.done_rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            let slot = match done.result {
                Ok(decoded) => Slot::Ready(decoded),
                Err(message) => Slot::Failed(message),
            };
            self.remote.insert(done.key, slot);
            finished = true;
        }
        self.start_jobs();
        finished
    }

    /// Union geometry stored inline in a part (its MeshData). Identical data is
    /// decoded once and shared.
    pub(crate) fn inline_union(&mut self, bytes: &[u8]) -> Result<Arc<LoadedMesh>> {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(mesh) = self.inline_unions.get(&key) {
            return Ok(mesh.clone());
        }
        let mesh = Arc::new(roblox::load_union_mesh(bytes)?);
        self.inline_unions.insert(key, mesh.clone());
        Ok(mesh)
    }

    fn load_local_mesh(&mut self, mesh_uri: &str) -> Result<Arc<LoadedMesh>> {
        let path = uri::resolve(&self.root, mesh_uri)?;
        let modified = modified_time(&path)?;
        if let Some(cached) = self.meshes.get(&path)
            && cached.modified == modified
        {
            return Ok(cached.value.clone());
        }
        let mesh = Arc::new(load_mesh_file(&path).with_context(|| format!("failed to load {mesh_uri}"))?);
        self.meshes.insert(path, Cached { modified, value: mesh.clone() });
        Ok(mesh)
    }

    fn load_local_image(&mut self, image_uri: &str) -> Result<Arc<LoadedImage>> {
        let path = uri::resolve(&self.root, image_uri)?;
        let modified = modified_time(&path)?;
        if let Some(cached) = self.images.get(&path)
            && cached.modified == modified
        {
            return Ok(cached.value.clone());
        }
        let image = Arc::new(load_image_file(&path).with_context(|| format!("failed to load {image_uri}"))?);
        self.images.insert(path, Cached { modified, value: image.clone() });
        Ok(image)
    }

    /// The state of a Roblox asset used as `kind`. The first request queues a
    /// background job, unless the asset is neither cached nor downloadable.
    fn remote_status(&mut self, id: u64, kind: RemoteKind) -> Status {
        match self.remote.get(&(id, kind)) {
            Some(Slot::Ready(decoded)) => return Status::Ready(decoded.clone()),
            Some(Slot::Failed(message)) => return Status::Failed(message.clone()),
            Some(Slot::Pending) => return Status::Pending,
            None => {}
        }
        if self.downloader.is_none() && !self.cache_path(id).is_file() {
            return Status::Unavailable;
        }
        self.remote.insert((id, kind), Slot::Pending);
        self.queue.push_back((id, kind));
        self.start_jobs();
        Status::Pending
    }

    fn cache_path(&self, id: u64) -> PathBuf {
        self.cache_dir.join(id.to_string())
    }

    fn start_jobs(&mut self) {
        while self.running < MAX_DOWNLOADS {
            let Some(key) = self.queue.pop_front() else { break };
            self.running += 1;
            let job = Job {
                key,
                cache: self.cache_path(key.0),
                downloader: self.downloader.clone(),
                done: self.done_tx.clone(),
            };
            std::thread::spawn(move || job.run());
        }
    }
}

impl Status {
    fn into_state<T>(self) -> AssetState<T> {
        match self {
            // Callers match Ready themselves; the kind of a job always matches its request.
            Status::Ready(_) => AssetState::Failed("internal error: asset kind mismatch".to_owned()),
            Status::Pending => AssetState::Pending,
            Status::Unavailable => AssetState::Unavailable,
            Status::Failed(message) => AssetState::Failed(message),
        }
    }
}

/// One background download and decode.
struct Job {
    key: (u64, RemoteKind),
    cache: PathBuf,
    downloader: Option<Downloader>,
    done: Sender<Done>,
}

impl Job {
    fn run(self) {
        let (id, kind) = self.key;
        let result =
            fetch(id, kind, &self.cache, self.downloader.as_deref()).map_err(|e| format!("rbxassetid://{id}: {e:#}"));
        // The store may have been dropped; then nobody wants the result.
        let _ = self.done.send(Done { key: self.key, result });
    }
}

/// Reads an asset from the cache, or else downloads it. Only bytes that decode
/// are cached, so a cached file is always a valid mesh, image or union, and a
/// kind mismatch is reported without downloading the asset again.
fn fetch(id: u64, kind: RemoteKind, cache: &Path, downloader: Option<&DownloadFn>) -> Result<Decoded> {
    if let Ok(bytes) = fs::read(cache) {
        return decode(kind, &bytes);
    }
    let Some(download) = downloader else {
        bail!("not cached and no downloader is set");
    };
    let bytes = download(id).context("download failed")?;
    let decoded = decode(kind, &bytes)?;
    write_cache(cache, &bytes);
    Ok(decoded)
}

/// Decodes asset bytes as `kind`. The content decides what the bytes are, so a
/// PNG requested as a mesh is reported as an image.
fn decode(kind: RemoteKind, bytes: &[u8]) -> Result<Decoded> {
    match (kind, roblox::sniff(bytes)) {
        (RemoteKind::Mesh, Content::Mesh) => Ok(Decoded::Mesh(Arc::new(roblox::load_roblox_mesh(bytes)?))),
        (RemoteKind::Mesh, Content::Image) => bail!("it is an image, not a mesh"),
        (RemoteKind::Image, Content::Image) => {
            let image = image::load_from_memory(bytes).context("failed to decode the image")?;
            Ok(Decoded::Image(Arc::new(LoadedImage::from_rgba(image.to_rgba8()))))
        }
        (RemoteKind::Image, Content::Mesh) => bail!("it is a mesh, not an image"),
        (RemoteKind::Union, _) => Ok(Decoded::Mesh(Arc::new(roblox::load_union_mesh(bytes)?))),
        (_, Content::Other) => bail!("unrecognized content (not a Roblox mesh, PNG or JPEG)"),
    }
}

/// Writes a cached asset through a temporary file, so a reader never sees half
/// of it. A cache that cannot be written only costs a download next time.
fn write_cache(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// A mesh copied into a project and the size it was read with.
#[derive(Debug, Clone)]
pub struct ImportedMesh {
    pub uri: String,
    /// The mesh's bounding-box size, for a MeshPart's InitialSize.
    pub size: Vec3,
}

/// Checks a .glb, .gltf or .obj file, copies it into the project's meshes
/// folder under a unique name, and returns its URI and size.
pub fn import_mesh(project_dir: &Path, file: &Path) -> Result<ImportedMesh> {
    let mesh = load_mesh_file(file)?;
    let uri = uri::import_file(project_dir, file, uri::MESH_FOLDER)?;
    Ok(ImportedMesh { uri, size: mesh.native_size })
}

/// Loads a mesh file by its extension.
pub fn load_mesh_file(path: &Path) -> Result<LoadedMesh> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "glb" | "gltf" => load_gltf(path),
        "obj" => load_obj(path),
        "mesh" => {
            let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
            roblox::load_roblox_mesh(&bytes).with_context(|| format!("failed to read {}", path.display()))
        }
        other => bail!("unsupported mesh format {:?} (use .glb, .gltf, .obj or .mesh)", other),
    }
}

/// Loads a PNG or JPEG image.
pub fn load_image_file(path: &Path) -> Result<LoadedImage> {
    let image = image::open(path).with_context(|| format!("failed to decode {}", path.display()))?;
    Ok(LoadedImage::from_rgba(image.to_rgba8()))
}

fn modified_time(path: &Path) -> Result<Option<SystemTime>> {
    let meta = fs::metadata(path).with_context(|| format!("{} not found", path.display()))?;
    Ok(meta.modified().ok())
}

/// The mesh scale that stretches the file's geometry from `initial` (its
/// original size) to the part's `size`. Axes with no extent in the file are
/// left unscaled.
pub fn mesh_scale(size: Vec3, initial: Vec3) -> Vec3 {
    let axis = |s: f32, i: f32| if i.abs() > 1e-6 { s / i } else { 1.0 };
    Vec3::new(axis(size.x, initial.x), axis(size.y, initial.y), axis(size.z, initial.z))
}

// -- glTF ---------------------------------------------------------------------

fn load_gltf(path: &Path) -> Result<LoadedMesh> {
    let (document, buffers, images) =
        gltf::import(path).with_context(|| format!("failed to read glTF {}", path.display()))?;

    // Node world transforms: walk down from the nodes nobody is a child of.
    let mut is_child = vec![false; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            is_child[child.index()] = true;
        }
    }
    let mut placed = Vec::new();
    let mut referenced = vec![false; document.meshes().len()];
    let mut stack: Vec<(gltf::Node, Mat4)> =
        document.nodes().filter(|n| !is_child[n.index()]).map(|n| (n, Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            referenced[mesh.index()] = true;
            placed.push((mesh, world));
        }
        for child in node.children() {
            stack.push((child, world));
        }
    }
    // Meshes that no node uses are still included, as they are.
    for mesh in document.meshes() {
        if !referenced[mesh.index()] {
            placed.push((mesh, Mat4::IDENTITY));
        }
    }

    let mut image_cache: HashMap<usize, Arc<LoadedImage>> = HashMap::new();
    let mut primitives = Vec::new();
    for (mesh, transform) in placed {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
            let Some(positions) = reader.read_positions() else { continue };
            let mut positions: Vec<[f32; 3]> = positions.collect();

            let normal_matrix = {
                let m3 = Mat3::from_mat4(transform);
                if m3.determinant().abs() > 1e-12 { m3.inverse().transpose() } else { m3 }
            };
            for p in &mut positions {
                *p = transform.transform_point3(Vec3::from(*p)).to_array();
            }
            let normals = reader.read_normals().map(|normals| {
                normals
                    .map(|n| (normal_matrix * Vec3::from(n)).normalize_or_zero().to_array())
                    .collect::<Vec<[f32; 3]>>()
            });

            let base_color = primitive.material().pbr_metallic_roughness().base_color_texture();
            let uv_set = base_color.as_ref().map_or(0, |info| info.tex_coord());
            let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(uv_set).map(|t| t.into_f32().collect());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };

            let texture = match base_color {
                Some(info) => {
                    let index = info.texture().source().index();
                    if let Some(cached) = image_cache.get(&index) {
                        Some(cached.clone())
                    } else {
                        match images.get(index).map(rgba_from_gltf) {
                            Some(Ok(rgba)) => {
                                let loaded = Arc::new(LoadedImage::from_rgba(rgba));
                                image_cache.insert(index, loaded.clone());
                                Some(loaded)
                            }
                            Some(Err(e)) => {
                                eprintln!("warning: {}: texture {index}: {e:#}", path.display());
                                None
                            }
                            None => None,
                        }
                    }
                }
                None => None,
            };

            let (vertices, indices) = build_primitive(&positions, normals.as_deref(), uvs.as_deref(), &indices)?;
            primitives.push(MeshPrimitive { vertices, indices, texture });
        }
    }
    LoadedMesh::from_primitives(primitives, true)
}

/// Converts a decoded glTF image to RGBA8. Grayscale and 16-bit images are
/// reduced to 8 bits per channel.
fn rgba_from_gltf(data: &gltf::image::Data) -> Result<RgbaImage> {
    use gltf::image::Format;
    let (channels, bytes) = match data.format {
        Format::R8 => (1, 1),
        Format::R8G8 => (2, 1),
        Format::R8G8B8 => (3, 1),
        Format::R8G8B8A8 => (4, 1),
        Format::R16 => (1, 2),
        Format::R16G16 => (2, 2),
        Format::R16G16B16 => (3, 2),
        Format::R16G16B16A16 => (4, 2),
        other => bail!("unsupported glTF image format {other:?}"),
    };
    let pixel_count = data.width as usize * data.height as usize;
    if data.pixels.len() < pixel_count * channels * bytes {
        bail!("glTF image data is too short");
    }
    // Little-endian 16-bit values: the high byte is the one that matters.
    let high = bytes - 1;
    let sample = |p: usize, c: usize| data.pixels[(p * channels + c) * bytes + high];
    let mut out = Vec::with_capacity(pixel_count * 4);
    for p in 0..pixel_count {
        let px = match channels {
            1 => [sample(p, 0), sample(p, 0), sample(p, 0), 255],
            2 => [sample(p, 0), sample(p, 0), sample(p, 0), sample(p, 1)],
            3 => [sample(p, 0), sample(p, 1), sample(p, 2), 255],
            _ => [sample(p, 0), sample(p, 1), sample(p, 2), sample(p, 3)],
        };
        out.extend_from_slice(&px);
    }
    RgbaImage::from_raw(data.width, data.height, out).context("bad glTF image size")
}

// -- OBJ ----------------------------------------------------------------------

fn load_obj(path: &Path) -> Result<LoadedMesh> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let options = tobj::LoadOptions { triangulate: true, single_index: true, ..Default::default() };
    // Materials are not read: an OBJ's .mtl is not part of the project yet.
    let (models, _) = tobj::load_obj_buf(&mut reader, &options, |_| Ok((Vec::new(), Default::default())))
        .with_context(|| format!("failed to read OBJ {}", path.display()))?;

    let mut primitives = Vec::new();
    for model in models {
        let mesh = model.mesh;
        let positions: Vec<[f32; 3]> = mesh.positions.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
        let normals: Option<Vec<[f32; 3]>> =
            (!mesh.normals.is_empty()).then(|| mesh.normals.chunks_exact(3).map(|n| [n[0], n[1], n[2]]).collect());
        // OBJ puts V = 0 at the bottom of the image; glTF and Roblox put it at the top.
        let uvs: Option<Vec<[f32; 2]>> =
            (!mesh.texcoords.is_empty()).then(|| mesh.texcoords.chunks_exact(2).map(|t| [t[0], 1.0 - t[1]]).collect());
        let (vertices, indices) = build_primitive(&positions, normals.as_deref(), uvs.as_deref(), &mesh.indices)?;
        primitives.push(MeshPrimitive { vertices, indices, texture: None });
    }
    LoadedMesh::from_primitives(primitives, true)
}

// -- shared -------------------------------------------------------------------

/// Turns positions, optional normals and UVs with a triangle index list into
/// renderable vertices. Without usable normals, every triangle gets its face
/// normal, so its vertices are not shared.
pub(crate) fn build_primitive(
    positions: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    uvs: Option<&[[f32; 2]]>,
    indices: &[u32],
) -> Result<(Vec<Vertex>, Vec<u32>)> {
    if let Some(&bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
        bail!("index {bad} is out of range for {} vertices", positions.len());
    }
    let uv = |i: usize| uvs.and_then(|u| u.get(i)).copied().unwrap_or([0.0, 0.0]);
    match normals {
        Some(normals) if normals.len() == positions.len() => {
            let vertices = positions
                .iter()
                .enumerate()
                .map(|(i, &position)| Vertex { position, normal: normals[i], uv: uv(i) })
                .collect();
            Ok((vertices, indices.to_vec()))
        }
        _ => {
            let mut vertices = Vec::with_capacity(indices.len());
            for tri in indices.chunks_exact(3) {
                let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                let (pa, pb, pc) = (Vec3::from(positions[a]), Vec3::from(positions[b]), Vec3::from(positions[c]));
                let n = (pb - pa).cross(pc - pa).normalize_or(Vec3::Y).to_array();
                for i in [a, b, c] {
                    vertices.push(Vertex { position: positions[i], normal: n, uv: uv(i) });
                }
            }
            let indices = (0..vertices.len() as u32).collect();
            Ok((vertices, indices))
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Small meshes and textures written by the tests, so no binary files are needed.

    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use base64::Engine;
    use image::{ImageFormat, Rgba, RgbaImage};

    use super::AssetStore;

    /// A 2x2 quad in the XY plane facing +Z, as OBJ text without normals.
    pub const QUAD_OBJ: &str =
        "v -1 -1 0\nv 1 -1 0\nv 1 1 0\nv -1 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4\n";

    /// Writes a 2x2 quad as a glTF file with an embedded base64 buffer and an
    /// embedded 2x2 PNG of `color` as its base color texture.
    pub fn quad_gltf(dir: &Path, name: &str, color: [u8; 4]) -> PathBuf {
        let positions: [[f32; 3]; 4] = [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]];
        let normals = [[0., 0., 1.]; 4];
        let uvs: [[f32; 2]; 4] = [[0., 1.], [1., 1.], [1., 0.], [0., 0.]];
        let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

        let mut bin = Vec::new();
        for p in positions.iter().flatten().chain(normals.iter().flatten()).chain(uvs.iter().flatten()) {
            bin.extend(p.to_le_bytes());
        }
        for i in indices {
            bin.extend(i.to_le_bytes());
        }

        let mut png = Vec::new();
        RgbaImage::from_pixel(2, 2, Rgba(color)).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let json = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": [{
                "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 },
                "indices": 3,
                "material": 0
            }] }],
            "materials": [{ "pbrMetallicRoughness": { "baseColorTexture": { "index": 0 } } }],
            "textures": [{ "source": 0 }],
            "images": [{ "uri": format!("data:image/png;base64,{}", b64.encode(&png)) }],
            "buffers": [{ "byteLength": bin.len(), "uri": format!("data:application/octet-stream;base64,{}", b64.encode(&bin)) }],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 48 },
                { "buffer": 0, "byteOffset": 48, "byteLength": 48 },
                { "buffer": 0, "byteOffset": 96, "byteLength": 32 },
                { "buffer": 0, "byteOffset": 128, "byteLength": 12 }
            ],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-1, -1, 0], "max": [1, 1, 0] },
                { "bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3" },
                { "bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2" },
                { "bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR" }
            ]
        });
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        path
    }

    /// A 4x4 square in the XY plane as a version 2.00 Roblox mesh.
    pub fn quad_mesh_bytes() -> Vec<u8> {
        crate::roblox::test_data::square_mesh_bytes()
    }

    /// A downloader that counts its calls and returns `bytes` for any ID.
    pub fn counting_downloader(
        bytes: Vec<u8>,
        calls: Arc<AtomicUsize>,
    ) -> Arc<dyn Fn(u64) -> anyhow::Result<Vec<u8>> + Send + Sync> {
        Arc::new(move |_id| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(bytes.clone())
        })
    }

    /// Polls until a download finishes, so the test can read its result.
    pub fn wait_for_poll(store: &mut AssetStore) {
        let start = std::time::Instant::now();
        while !store.poll() {
            assert!(start.elapsed() < std::time::Duration::from_secs(20), "download never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use image::Rgba;

    use super::fixtures::{QUAD_OBJ, quad_gltf};
    use super::*;

    fn project_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        dir
    }

    #[test]
    fn obj_is_centered_and_gets_flat_normals() {
        let dir = project_with(&[("assets/meshes/quad.obj", QUAD_OBJ.as_bytes())]);
        let mut store = AssetStore::new(dir.path());
        let mesh = store.mesh("kahua://assets/meshes/quad.obj").unwrap();
        assert_eq!(mesh.primitives.len(), 1);
        assert!(mesh.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5));
        let prim = &mesh.primitives[0];
        assert_eq!(prim.indices.len(), 6);
        assert!(prim.texture.is_none());
        for v in &prim.vertices {
            assert!(v.position[0].abs() <= 1.0 + 1e-5 && v.position[1].abs() <= 1.0 + 1e-5);
            assert!(Vec3::from(v.normal).abs_diff_eq(Vec3::Z, 1e-5));
        }
        // The UVs are flipped vertically: OBJ v = 0 is the bottom edge.
        let bottom_left = prim.vertices.iter().find(|v| v.position == [-1.0, -1.0, 0.0]).unwrap();
        assert_eq!(bottom_left.uv, [0.0, 1.0]);
    }

    #[test]
    fn gltf_loads_geometry_and_embedded_texture() {
        let dir = project_with(&[]);
        quad_gltf(dir.path(), "quad.gltf", [0, 255, 0, 255]);
        let mut store = AssetStore::new(dir.path());
        let mesh = store.mesh("kahua://quad.gltf").unwrap();
        assert!(mesh.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5));
        let prim = &mesh.primitives[0];
        assert_eq!(prim.vertices.len(), 4);
        assert!(prim.vertices.iter().all(|v| Vec3::from(v.normal).abs_diff_eq(Vec3::Z, 1e-5)));
        let texture = prim.texture.as_ref().expect("embedded base color texture");
        assert_eq!((texture.width, texture.height), (2, 2));
        assert_eq!(texture.levels.len(), 2, "2x2 has a 1x1 mip level");
        assert_eq!(&texture.levels[0].rgba[0..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn glb_is_read_by_the_same_loader() {
        // A GLB is a glTF JSON chunk followed by a binary chunk. Build one from
        // the embedded-buffer file by moving its buffer into the binary chunk.
        let dir = project_with(&[]);
        let gltf_path = quad_gltf(dir.path(), "quad.gltf", [255, 0, 0, 255]);
        let doc: serde_json::Value = serde_json::from_slice(&fs::read(&gltf_path).unwrap()).unwrap();
        let b64 = doc["buffers"][0]["uri"].as_str().unwrap().split(',').nth(1).unwrap();
        let bin = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let mut doc = doc;
        doc["buffers"][0].as_object_mut().unwrap().remove("uri");
        // The image stays embedded; GLB allows data URIs in images.
        let mut json = serde_json::to_vec(&doc).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin_chunk = bin.clone();
        while !bin_chunk.len().is_multiple_of(4) {
            bin_chunk.push(0);
        }
        let total = 12 + 8 + json.len() + 8 + bin_chunk.len();
        let mut glb = Vec::new();
        glb.extend(b"glTF");
        glb.extend(2u32.to_le_bytes());
        glb.extend((total as u32).to_le_bytes());
        glb.extend((json.len() as u32).to_le_bytes());
        glb.extend(b"JSON");
        glb.extend(&json);
        glb.extend((bin_chunk.len() as u32).to_le_bytes());
        glb.extend(b"BIN\0");
        glb.extend(&bin_chunk);
        fs::write(dir.path().join("quad.glb"), glb).unwrap();

        let mut store = AssetStore::new(dir.path());
        let mesh = store.mesh("kahua://quad.glb").unwrap();
        assert_eq!(mesh.primitives[0].vertices.len(), 4);
        assert!(mesh.primitives[0].texture.is_some());
    }

    #[test]
    fn store_reloads_when_the_file_changes() {
        let dir = project_with(&[("quad.obj", QUAD_OBJ.as_bytes())]);
        let mut store = AssetStore::new(dir.path());
        let first = store.mesh("kahua://quad.obj").unwrap();
        let again = store.mesh("kahua://quad.obj").unwrap();
        assert!(Arc::ptr_eq(&first, &again), "unchanged files come from the cache");

        let triangle = "v 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n";
        let path = dir.path().join("quad.obj");
        fs::write(&path, triangle).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        fs::OpenOptions::new().write(true).open(&path).unwrap().set_modified(later).unwrap();

        let changed = store.mesh("kahua://quad.obj").unwrap();
        assert_eq!(changed.primitives[0].indices.len(), 3);
        assert!(changed.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5));
    }

    #[test]
    fn missing_and_escaping_paths_are_errors() {
        let dir = project_with(&[]);
        let mut store = AssetStore::new(dir.path());
        assert!(store.mesh("kahua://assets/meshes/nope.obj").is_err());
        assert!(store.mesh("kahua://../outside.obj").is_err());
        assert!(store.image("rbxassetid://1234").is_err());
    }

    #[test]
    fn texture_images_are_loaded_and_downscaled() {
        let dir = project_with(&[]);
        RgbaImage::from_pixel(4096, 16, Rgba([10, 20, 30, 255])).save(dir.path().join("big.png")).unwrap();
        let mut store = AssetStore::new(dir.path());
        let image = store.image("kahua://big.png").unwrap();
        assert_eq!(image.width.max(image.height), MAX_TEXTURE_SIZE);
        assert_eq!(image.levels[0].rgba[0..4], [10, 20, 30, 255]);
    }

    #[test]
    fn import_copies_the_mesh_into_the_project() {
        let project = project_with(&[]);
        let src = project_with(&[("sword.obj", QUAD_OBJ.as_bytes())]);
        let imported = import_mesh(project.path(), &src.path().join("sword.obj")).unwrap();
        assert_eq!(imported.uri, "kahua://assets/meshes/sword.obj");
        assert!(imported.size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5));
        assert!(project.path().join("assets/meshes/sword.obj").is_file());
        assert!(import_mesh(project.path(), &src.path().join("sword.txt")).is_err());
    }

    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use image::ImageFormat;

    use super::fixtures::{counting_downloader, quad_mesh_bytes, wait_for_poll};

    fn downloader_of(bytes: Vec<u8>) -> (Arc<AtomicUsize>, Downloader) {
        let calls = Arc::new(AtomicUsize::new(0));
        let downloader = counting_downloader(bytes, calls.clone());
        (calls, downloader)
    }

    #[test]
    fn roblox_meshes_download_in_the_background_and_then_load() {
        let project = project_with(&[]);
        let cache = tempfile::tempdir().unwrap();
        let (calls, downloader) = downloader_of(quad_mesh_bytes());
        let mut store = AssetStore::new(project.path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);

        assert!(!store.poll(), "nothing has finished yet");
        assert!(matches!(store.mesh_state("rbxassetid://123"), AssetState::Pending));
        // Asking again does not start a second download.
        assert!(matches!(store.mesh_state("rbxassetid://123"), AssetState::Pending));
        assert!(store.mesh("rbxassetid://123").is_err(), "still downloading");

        wait_for_poll(&mut store);
        let AssetState::Ready(mesh) = store.mesh_state("rbxassetid://123") else { panic!("expected a loaded mesh") };
        assert!(mesh.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.path().join("123").is_file(), "the download is cached");
        assert!(store.mesh("https://www.roblox.com/asset/?id=123").is_ok(), "URL forms share the cache entry");
    }

    #[test]
    fn cached_downloads_are_not_downloaded_again() {
        let cache = tempfile::tempdir().unwrap();
        let (calls, downloader) = downloader_of(quad_mesh_bytes());
        let mut first = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        first.set_downloader(downloader);
        assert!(matches!(first.mesh_state("rbxassetid://55"), AssetState::Pending));
        wait_for_poll(&mut first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A new store on the same cache loads it with no downloader at all.
        let mut second = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        assert!(matches!(second.mesh_state("rbxassetid://55"), AssetState::Pending));
        wait_for_poll(&mut second);
        assert!(matches!(second.mesh_state("rbxassetid://55"), AssetState::Ready(_)));

        // And with a downloader set, the cache still wins.
        let (calls_again, downloader) = downloader_of(quad_mesh_bytes());
        second.set_downloader(downloader);
        assert!(matches!(second.mesh_state("rbxassetid://55"), AssetState::Ready(_)));
        assert_eq!(calls_again.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn without_a_downloader_or_cache_roblox_assets_are_unavailable() {
        let project = project_with(&[]);
        let cache = tempfile::tempdir().unwrap();
        let mut store = AssetStore::new(project.path()).with_cache_dir(cache.path());
        assert!(matches!(store.mesh_state("rbxassetid://9"), AssetState::Unavailable));
        assert!(matches!(store.image_state("rbxassetid://9"), AssetState::Unavailable));
        assert!(store.mesh("rbxassetid://9").is_err());
        assert!(!store.poll());
    }

    #[test]
    fn at_most_four_downloads_run_at_once() {
        let cache = tempfile::tempdir().unwrap();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (r, p) = (running.clone(), peak.clone());
        let bytes = quad_mesh_bytes();
        let downloader: Arc<dyn Fn(u64) -> Result<Vec<u8>> + Send + Sync> = Arc::new(move |_id| {
            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(60));
            r.fetch_sub(1, Ordering::SeqCst);
            Ok(bytes.clone())
        });
        let mut store = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);
        for id in 1..=10 {
            assert!(matches!(store.mesh_state(&format!("rbxassetid://{id}")), AssetState::Pending));
        }
        let all_ready = |store: &mut AssetStore| {
            (1..=10).all(|id| matches!(store.mesh_state(&format!("rbxassetid://{id}")), AssetState::Ready(_)))
        };
        let start = std::time::Instant::now();
        while !all_ready(&mut store) {
            store.poll();
            assert!(start.elapsed() < std::time::Duration::from_secs(30), "downloads did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(peak.load(Ordering::SeqCst) <= MAX_DOWNLOADS, "peak {}", peak.load(Ordering::SeqCst));
        assert!(peak.load(Ordering::SeqCst) >= 2, "downloads do run in parallel");
    }

    #[test]
    fn a_failed_download_is_reported_and_not_retried() {
        let cache = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let downloader: Arc<dyn Fn(u64) -> Result<Vec<u8>> + Send + Sync> = Arc::new(move |_id| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(b"<html>Not Found</html>".to_vec())
        });
        let mut store = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);
        assert!(matches!(store.mesh_state("rbxassetid://77"), AssetState::Pending));
        wait_for_poll(&mut store);
        let AssetState::Failed(message) = store.mesh_state("rbxassetid://77") else { panic!("expected a failure") };
        assert!(message.contains("rbxassetid://77") && message.contains("unrecognized"), "{message}");
        assert!(matches!(store.mesh_state("rbxassetid://77"), AssetState::Failed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!cache.path().join("77").exists(), "bad data is not cached");
    }

    #[test]
    fn downloaded_images_load_and_kinds_are_checked_by_content() {
        let cache = tempfile::tempdir().unwrap();
        let mut png = Vec::new();
        RgbaImage::from_pixel(3, 2, Rgba([9, 8, 7, 255]))
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let (_, downloader) = downloader_of(png);
        let mut store = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);
        assert!(matches!(store.image_state("rbxassetid://31"), AssetState::Pending));
        wait_for_poll(&mut store);
        let AssetState::Ready(image) = store.image_state("rbxassetid://31") else { panic!("expected an image") };
        assert_eq!((image.width, image.height), (3, 2));
        // The same asset read as a mesh fails by its content, without downloading again.
        assert!(matches!(store.mesh_state("rbxassetid://31"), AssetState::Pending));
        wait_for_poll(&mut store);
        let AssetState::Failed(message) = store.mesh_state("rbxassetid://31") else { panic!("a PNG is not a mesh") };
        assert!(message.contains("image"), "{message}");
    }

    #[test]
    fn remote_meshes_are_checked_by_content() {
        let cache = tempfile::tempdir().unwrap();
        let (_, downloader) = downloader_of(quad_mesh_bytes());
        let mut store = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);
        assert!(matches!(store.image_state("rbxassetid://41"), AssetState::Pending));
        wait_for_poll(&mut store);
        let AssetState::Failed(message) = store.image_state("rbxassetid://41") else {
            panic!("a mesh is not an image")
        };
        assert!(message.contains("mesh"), "{message}");
    }

    #[test]
    fn poll_reports_only_new_results() {
        let cache = tempfile::tempdir().unwrap();
        let (_, downloader) = downloader_of(quad_mesh_bytes());
        let mut store = AssetStore::new(project_with(&[]).path()).with_cache_dir(cache.path());
        store.set_downloader(downloader);
        assert!(!store.poll());
        store.mesh_state("rbxassetid://5");
        wait_for_poll(&mut store);
        assert!(!store.poll(), "the finished download was reported once");
    }

    #[test]
    fn roblox_uris_are_not_files() {
        let project = project_with(&[]);
        let mut store = AssetStore::new(project.path());
        assert!(matches!(store.union_state("kahua://x.obj"), AssetState::Failed(_)));
    }
}
