//! Meshes and textures stored in a project, loaded from glTF/GLB, OBJ and
//! image files. Parts refer to them with `kahua://` URIs (see `kahua_io::assets`).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use glam::{Mat3, Mat4, Vec3};
use image::RgbaImage;
use image::imageops::{self, FilterType};
use kahua_io::assets as uri;

use crate::mesh::Vertex;

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
    fn from_primitives(primitives: Vec<MeshPrimitive>) -> Result<Self> {
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
        let center = (min + max) * 0.5;
        let mut primitives = primitives;
        for v in primitives.iter_mut().flat_map(|p| p.vertices.iter_mut()) {
            v.position = (Vec3::from(v.position) - center).to_array();
        }
        Ok(Self { primitives, native_size: max - min })
    }
}

/// Loads and caches meshes and images from a project folder. Each file is read
/// again when its modification time changes.
pub struct AssetStore {
    root: PathBuf,
    meshes: HashMap<PathBuf, Cached<LoadedMesh>>,
    images: HashMap<PathBuf, Cached<LoadedImage>>,
}

struct Cached<T> {
    modified: Option<SystemTime>,
    value: Arc<T>,
}

impl AssetStore {
    /// A store that resolves `kahua://` URIs against `root`, the project directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), meshes: HashMap::new(), images: HashMap::new() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The mesh at a `kahua://` URI.
    pub fn mesh(&mut self, mesh_uri: &str) -> Result<Arc<LoadedMesh>> {
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

    /// The image at a `kahua://` URI.
    pub fn image(&mut self, image_uri: &str) -> Result<Arc<LoadedImage>> {
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
        other => bail!("unsupported mesh format {:?} (use .glb, .gltf or .obj)", other),
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
    LoadedMesh::from_primitives(primitives)
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
    LoadedMesh::from_primitives(primitives)
}

// -- shared -------------------------------------------------------------------

/// Turns positions, optional normals and UVs with a triangle index list into
/// renderable vertices. Without usable normals, every triangle gets its face
/// normal, so its vertices are not shared.
fn build_primitive(
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

    use base64::Engine;
    use image::{ImageFormat, Rgba, RgbaImage};

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
}
