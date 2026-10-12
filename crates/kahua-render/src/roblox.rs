//! Roblox's own mesh and union formats, and the URIs that name Roblox assets.
//!
//! Meshes are read into `LoadedMesh` (LOD 0 only; bones and facial data are
//! ignored) and written back as version 2.00 binary meshes for upload. The
//! geometry of a UnionOperation is read with `load_union_mesh`.
//!
//! Conventions, shared with the other loaders in `assets`: positions are in
//! studs, normals are unit length, and UVs have V = 0 at the top of the image.
//! A mesh read from a file is centered on its bounding box (see
//! `LoadedMesh`), and `to_roblox_mesh` writes the centered positions, so a
//! written mesh's origin is the middle of its box and its InitialSize is the
//! mesh's `native_size`. Union geometry is not centered: it is stored relative
//! to the part's CFrame and used as it is.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use binrw::BinWriterExt;
use glam::Vec3;
use rbx_mesh::mesh::{Face2, Lod3, Mesh, Mesh2, Revision2, Vertex2, Vertex2Truncated, Vertices2};
use rbx_mesh::union_graphics::{self, UnionGraphics};

use crate::assets::{LoadedMesh, MeshPrimitive, build_primitive, load_mesh_file};
use crate::mesh::Vertex;

/// Whether bytes look like a Roblox mesh: a `version 1.00` text header, or the
/// binary header of versions 2 to 5 (`version N.00`).
pub fn is_mesh(bytes: &[u8]) -> bool {
    bytes.starts_with(b"version ")
}

/// What a downloaded file holds, judged from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Content {
    Mesh,
    Image,
    Other,
}

pub(crate) fn sniff(bytes: &[u8]) -> Content {
    if is_mesh(bytes) {
        return Content::Mesh;
    }
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg) => Content::Image,
        _ => Content::Other,
    }
}

/// Vertex streams and triangle indices read from a file, before they become a `LoadedMesh`.
#[derive(Default)]
struct Geometry {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

/// Reads a Roblox mesh of any version (1.00 to 5.00). Only level of detail 0
/// is kept. Positions come out in studs; version 1.00 files, which store
/// double-size geometry, are halved by the parser. Like every mesh the
/// renderer loads, the result is centered on its bounding box.
pub fn load_roblox_mesh(bytes: &[u8]) -> Result<LoadedMesh> {
    let mesh =
        rbx_mesh::read_mesh_versioned(Cursor::new(bytes)).map_err(|e| anyhow!("not a readable Roblox mesh: {e}"))?;
    let geometry = match mesh {
        Mesh::V1(m) => Geometry {
            positions: m.vertices.iter().map(|v| v.pos).collect(),
            normals: m.vertices.iter().map(|v| v.norm).collect(),
            // The parser already flipped V to the top-down convention used here.
            uvs: m.vertices.iter().map(|v| [v.tex[0], v.tex[1]]).collect(),
            indices: (0..m.vertices.len() as u32).collect(),
        },
        Mesh::V2(m) => geometry_from_vertices2(&m.vertices, face_indices(&m.faces)),
        Mesh::V3(m) => geometry_from_vertices2(&m.vertices, lod0_indices(&m.faces, &m.lods)),
        Mesh::V4(m) => geometry_from_vertices(&m.vertices, lod0_indices(&m.faces, &m.lods)),
        Mesh::V5(m) => geometry_from_vertices(&m.vertices, lod0_indices(&m.faces, &m.lods)),
    };
    finish(geometry, true)
}

/// Writes a mesh as a Roblox version 2.00 binary mesh (full 40-byte vertices).
///
/// The positions, normals and UVs are written as they are, so the mesh's
/// origin is the center of its bounding box. Every primitive is merged into
/// one mesh and its texture is not written; a TextureID is uploaded separately.
/// Tangents are arbitrary directions perpendicular to each normal, and vertex
/// color is white.
pub fn to_roblox_mesh(mesh: &LoadedMesh) -> Result<Vec<u8>> {
    let mut vertices: Vec<Vertex2> = Vec::new();
    let mut faces: Vec<Face2> = Vec::new();
    for primitive in &mesh.primitives {
        let base = vertices.len();
        vertices.extend(primitive.vertices.iter().map(encode_vertex));
        for tri in primitive.indices.chunks_exact(3) {
            if tri.iter().any(|&i| i as usize >= primitive.vertices.len()) {
                bail!("a triangle refers to a vertex that does not exist");
            }
            let index = |i: u32| -> Result<rbx_mesh::mesh::VertexId2> {
                Ok(rbx_mesh::mesh::VertexId2(u32::try_from(base + i as usize).context("too many vertices")?))
            };
            faces.push(Face2([index(tri[0])?, index(tri[1])?, index(tri[2])?]));
        }
    }
    if faces.is_empty() {
        bail!("the mesh has no triangles");
    }
    u32::try_from(vertices.len()).context("too many vertices for a Roblox mesh")?;
    let mesh = Mesh2 { revision: Revision2::Version200, vertices: Vertices2::Full(vertices), faces };
    let mut out = Cursor::new(Vec::new());
    out.write_le(&mesh).map_err(|e| anyhow!("failed to write the mesh: {e}"))?;
    Ok(out.into_inner())
}

/// Reads a .glb, .gltf, .obj or .mesh file and returns it as a Roblox version 2.00 mesh.
pub fn convert_mesh_file(path: &Path) -> Result<Vec<u8>> {
    let mesh = load_mesh_file(path)?;
    to_roblox_mesh(&mesh)
}

/// Reads the geometry of a UnionOperation from its union graphics data (the
/// MeshData property, or the file behind its AssetId). The vertices are kept
/// where they are stored, relative to the part's CFrame. CSGK data holds only
/// an identifier, so it has no geometry and is an error.
pub fn load_union_mesh(bytes: &[u8]) -> Result<LoadedMesh> {
    let graphics = rbx_mesh::read_union_graphics_versioned(Cursor::new(bytes))
        .map_err(|e| anyhow!("not readable union data: {e}"))?;
    let geometry = match graphics {
        UnionGraphics::CSGK(_) => bail!("CSGK union data holds no geometry"),
        UnionGraphics::V2(m) => geometry_from_union(&m.mesh),
        UnionGraphics::V4(m) => geometry_from_union(&m.mesh),
        UnionGraphics::V5(m) => Geometry {
            positions: m.positions,
            normals: m.normals.iter().map(|n| n.0).collect(),
            uvs: m.tex,
            indices: m.faces.indices,
        },
    };
    finish(geometry, false)
}

/// The asset ID that a Roblox asset URI names: `rbxassetid://<id>`, or a
/// roblox.com asset link such as `https://www.roblox.com/asset/?id=<id>`
/// (also `roblox.com`, `assetdelivery.roblox.com`, `http://` and the
/// `/v1/asset/` path). `None` for other URIs.
pub fn asset_id(uri: &str) -> Option<u64> {
    let uri = uri.trim();
    if let Some(id) = uri.strip_prefix("rbxassetid://") {
        return parse_id(id.trim_end_matches('/'));
    }
    let rest = uri.strip_prefix("https://").or_else(|| uri.strip_prefix("http://")).unwrap_or(uri);
    let (host_path, query) = rest.split_once('?')?;
    let (host, path) = host_path.split_once('/')?;
    if !matches!(host.to_ascii_lowercase().as_str(), "roblox.com" | "www.roblox.com" | "assetdelivery.roblox.com") {
        return None;
    }
    let path = path.trim_end_matches('/');
    if path != "asset" && !path.ends_with("/asset") {
        return None;
    }
    query.split('&').find_map(|pair| pair.strip_prefix("id=")).and_then(parse_id)
}

fn parse_id(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|&id| id > 0)
}

fn finish(mut g: Geometry, recenter: bool) -> Result<LoadedMesh> {
    g.indices.truncate(g.indices.len() / 3 * 3);
    let (vertices, indices) = build_primitive(&g.positions, Some(&g.normals), Some(&g.uvs), &g.indices)?;
    LoadedMesh::from_primitives(vec![MeshPrimitive { vertices, indices, texture: None }], recenter)
}

fn face_indices(faces: &[Face2]) -> Vec<u32> {
    faces.iter().flat_map(|f| f.0.iter().map(|v| v.0)).collect()
}

/// The triangles of level of detail 0. The LOD list holds the first face of
/// each level, so LOD 0 runs from the first entry up to the second. Without
/// LODs every face is used.
fn lod0_indices(faces: &[Face2], lods: &[Lod3]) -> Vec<u32> {
    let start = lods.first().map_or(0, |l| l.0 as usize).min(faces.len());
    let end = lods.get(1).map_or(faces.len(), |l| l.0 as usize).clamp(start, faces.len());
    face_indices(&faces[start..end])
}

fn geometry_from_vertices2(vertices: &Vertices2, indices: Vec<u32>) -> Geometry {
    match vertices {
        Vertices2::Full(v) => Geometry {
            positions: v.iter().map(|v| v.pos).collect(),
            normals: v.iter().map(|v| v.norm).collect(),
            uvs: v.iter().map(|v| v.tex).collect(),
            indices,
        },
        Vertices2::Truncated(v) => Geometry {
            positions: v.iter().map(|v: &Vertex2Truncated| v.pos).collect(),
            normals: v.iter().map(|v| v.norm).collect(),
            uvs: v.iter().map(|v| v.tex).collect(),
            indices,
        },
    }
}

fn geometry_from_vertices(vertices: &[Vertex2], indices: Vec<u32>) -> Geometry {
    Geometry {
        positions: vertices.iter().map(|v| v.pos).collect(),
        normals: vertices.iter().map(|v| v.norm).collect(),
        uvs: vertices.iter().map(|v| v.tex).collect(),
        indices,
    }
}

fn geometry_from_union(mesh: &union_graphics::Mesh2) -> Geometry {
    Geometry {
        positions: mesh.vertices.iter().map(|v| v.pos).collect(),
        normals: mesh.vertices.iter().map(|v| v.norm).collect(),
        uvs: mesh.vertices.iter().map(|v| v.tex).collect(),
        indices: mesh.faces.iter().flat_map(|f| f.iter().map(|v| v.0)).collect(),
    }
}

/// A vertex in Roblox's full layout. The tangent is an arbitrary direction
/// perpendicular to the normal, with a bitangent sign of +1.
fn encode_vertex(v: &Vertex) -> Vertex2 {
    let normal = Vec3::from(v.normal).try_normalize().unwrap_or(Vec3::Y);
    let tangent = normal.any_orthonormal_vector();
    let signed = |x: f32| (x * 127.0).round() as i8;
    Vertex2 {
        pos: v.position,
        norm: normal.to_array(),
        tex: v.uv,
        tangent: [signed(tangent.x), signed(tangent.y), signed(tangent.z), 127],
        color: [255; 4],
    }
}

#[cfg(test)]
pub(crate) mod test_data {
    //! Roblox files built by the tests, so no binary files are needed.

    use std::io::Cursor;

    use binrw::BinWriterExt;
    use rbx_mesh::mesh::{
        Face2, Facs5, Lod3, Mesh2, Mesh3, Mesh4, Mesh5, QuantizedMatrix5, QuantizedTransforms5, Revision2, Revision3,
        Revision4, Revision5, Vertex2, VertexId2, Vertices2,
    };
    use rbx_mesh::mesh::{FacsFormat5, LodType4};
    use rbx_mesh::union_graphics::{
        CSGMDL2, Hash, Mesh2 as UnionMesh2, NormalId, NormalId2, Vertex as UnionVertex, VertexId,
    };

    use crate::assets::LoadedMesh;
    use crate::roblox::to_roblox_mesh;

    /// A vertex with an upward normal, a tangent along X and the given UV.
    pub fn vertex2(pos: [f32; 3], uv: [f32; 2]) -> Vertex2 {
        Vertex2 { pos, norm: [0.0, 0.0, 1.0], tex: uv, tangent: [127, 0, 0, 127], color: [255; 4] }
    }

    /// Two triangles forming a 2x2 square in the XY plane, centered on the origin.
    pub fn square_vertices() -> Vec<Vertex2> {
        vec![
            vertex2([-1.0, -1.0, 0.0], [0.0, 1.0]),
            vertex2([1.0, -1.0, 0.0], [1.0, 1.0]),
            vertex2([1.0, 1.0, 0.0], [1.0, 0.0]),
            vertex2([-1.0, 1.0, 0.0], [0.0, 0.0]),
        ]
    }

    pub fn square_faces() -> Vec<Face2> {
        vec![Face2([VertexId2(0), VertexId2(1), VertexId2(2)]), Face2([VertexId2(0), VertexId2(2), VertexId2(3)])]
    }

    /// Writes a value with the default (no) arguments, as all the test formats need.
    fn encode<T: for<'a> binrw::BinWrite<Args<'a> = ()>>(value: &T) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        out.write_le(value).unwrap();
        out.into_inner()
    }

    pub fn mesh2(vertices: Vertices2) -> Vec<u8> {
        encode(&Mesh2 { revision: Revision2::Version200, vertices, faces: square_faces() })
    }

    /// Two triangles, the second one is LOD 1 (LOD list: 0, 1).
    pub fn mesh3_with_two_lods() -> Vec<u8> {
        encode(&Mesh3 {
            revision: Revision3::Version300,
            vertices: Vertices2::Full(square_vertices()),
            faces: square_faces(),
            lods: vec![Lod3(0), Lod3(1)],
        })
    }

    pub fn mesh4_with_two_lods() -> Vec<u8> {
        encode(&Mesh4 {
            revision: Revision4::Version401,
            lod_type: LodType4::None,
            vertices: square_vertices(),
            envelopes: vec![],
            faces: square_faces(),
            lods: vec![Lod3(0), Lod3(1)],
            bones: vec![],
            bone_names: vec![],
            subsets: vec![],
            lod_hq_count: 0,
            _padding: 0,
        })
    }

    pub fn mesh5_with_two_lods() -> Vec<u8> {
        let empty = || QuantizedMatrix5::Raw { x: 0, y: 0, matrix: vec![] };
        encode(&Mesh5 {
            revision: Revision5::Version500,
            lod_type: LodType4::None,
            vertices: square_vertices(),
            envelopes: vec![],
            faces: square_faces(),
            lods: vec![Lod3(0), Lod3(1)],
            bones: vec![],
            bone_names: vec![],
            subsets: vec![],
            lod_hq_count: 0,
            facs_format: FacsFormat5::Format1,
            sizeof_facs: 0,
            facs: Facs5 {
                face_bone_names: vec![],
                face_control_names: vec![],
                quantized_transforms: QuantizedTransforms5 {
                    px: empty(),
                    py: empty(),
                    pz: empty(),
                    rx: empty(),
                    ry: empty(),
                    rz: empty(),
                },
                two_pose_correctives: vec![],
                three_pose_correctives: vec![],
            },
        })
    }

    /// A version 2.00 mesh of a square, written by `to_roblox_mesh`.
    pub fn square_mesh_bytes() -> Vec<u8> {
        let mesh = LoadedMesh::from_primitives(
            vec![crate::assets::MeshPrimitive {
                vertices: square_vertices()
                    .iter()
                    .map(|v| crate::mesh::Vertex { position: v.pos, normal: v.norm, uv: v.tex })
                    .collect(),
                indices: vec![0, 1, 2, 0, 2, 3],
                texture: None,
            }],
            true,
        )
        .unwrap();
        to_roblox_mesh(&mesh).unwrap()
    }

    /// Union graphics (CSGMDL version 2) holding a 4x4 square at x = 8..12,
    /// so tests can check that union geometry is not centered.
    pub fn union_square_bytes() -> Vec<u8> {
        let corner = |x: f32, y: f32, uv: [f32; 2]| UnionVertex {
            pos: [x, y, 0.0],
            norm: [0.0, 0.0, 1.0],
            color: [255; 4],
            normal_id: NormalId2(NormalId::Front),
            tex: uv,
            tangent: [0.0; 3],
        };
        let vertices = vec![
            corner(8.0, -2.0, [0.0, 1.0]),
            corner(12.0, -2.0, [1.0, 1.0]),
            corner(12.0, 2.0, [1.0, 0.0]),
            corner(8.0, 2.0, [0.0, 0.0]),
        ];
        let faces = vec![[VertexId(0), VertexId(1), VertexId(2)], [VertexId(0), VertexId(2), VertexId(3)]];
        let union = CSGMDL2 { hash: Hash { hash: [0; 16], _unknown: [0; 16] }, mesh: UnionMesh2 { vertices, faces } };
        encode(&union)
    }
}

#[cfg(test)]
mod tests {
    use super::test_data::*;
    use super::*;
    use rbx_mesh::mesh::Vertex2Truncated;

    /// The min and max corners of every vertex in a mesh.
    fn bounds(mesh: &LoadedMesh) -> (Vec3, Vec3) {
        let points = mesh.primitives.iter().flat_map(|p| p.vertices.iter().map(|v| Vec3::from(v.position)));
        points.fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)))
    }

    /// A 2x2 square, centered on its bounding box.
    fn assert_square(mesh: &LoadedMesh, expected_triangles: usize) {
        let prim = &mesh.primitives[0];
        assert_eq!(prim.indices.len(), expected_triangles * 3);
        assert!(mesh.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5), "{:?}", mesh.native_size);
        let (lo, hi) = bounds(mesh);
        assert!(((lo + hi) * 0.5).length() < 1e-5, "centered on the box: {lo:?} {hi:?}");
    }

    #[test]
    fn version_2_full_vertices_are_read_and_centered() {
        let mesh = load_roblox_mesh(&mesh2(Vertices2::Full(square_vertices()))).unwrap();
        assert_square(&mesh, 2);
        assert!(mesh.primitives[0].vertices.iter().all(|v| Vec3::from(v.normal).abs_diff_eq(Vec3::Z, 1e-5)));
    }

    #[test]
    fn version_2_truncated_vertices_are_read() {
        let truncated = square_vertices()
            .into_iter()
            .map(|v| Vertex2Truncated { pos: v.pos, norm: v.norm, tex: v.tex, tangent: v.tangent })
            .collect();
        let mesh = load_roblox_mesh(&mesh2(Vertices2::Truncated(truncated))).unwrap();
        assert_square(&mesh, 2);
    }

    #[test]
    fn versions_3_to_5_keep_only_lod_0() {
        for (name, bytes) in
            [("3.00", mesh3_with_two_lods()), ("4.01", mesh4_with_two_lods()), ("5.00", mesh5_with_two_lods())]
        {
            let mesh = load_roblox_mesh(&bytes).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            // The second face starts LOD 1, so LOD 0 is only the first triangle.
            assert_eq!(mesh.primitives[0].indices, vec![0, 1, 2], "{name}: LOD 0 is one triangle");
        }
    }

    #[test]
    fn version_1_text_meshes_are_read_and_halved_only_in_1_00() {
        let text = |version: &str| {
            format!("version {version}\n1\n[0,0,0][0,0,1][0,0,0][2,0,0][0,0,1][1,0,0][0,2,0][0,0,1][0,1,0]\n")
        };
        // 1.00 stores double-size geometry, so the 2x2 triangle is 1x1 once read.
        let v100 = load_roblox_mesh(text("1.00").as_bytes()).unwrap();
        assert!(v100.native_size.abs_diff_eq(Vec3::new(1.0, 1.0, 0.0), 1e-5), "{:?}", v100.native_size);
        let v101 = load_roblox_mesh(text("1.01").as_bytes()).unwrap();
        assert!(v101.native_size.abs_diff_eq(Vec3::new(2.0, 2.0, 0.0), 1e-5), "{:?}", v101.native_size);

        // The V of each UV is flipped by the parser: the second vertex's (1, 0) becomes (1, 1).
        let uvs: Vec<[f32; 2]> = v101.primitives[0].vertices.iter().map(|v| v.uv).collect();
        assert_eq!(uvs[1], [1.0, 1.0]);
    }

    #[test]
    fn written_meshes_read_back_with_the_same_geometry() {
        let original = load_roblox_mesh(&square_mesh_bytes()).unwrap();
        assert_square(&original, 2);

        // Write what we read, and read it back.
        let bytes = to_roblox_mesh(&original).unwrap();
        assert!(bytes.starts_with(b"version 2.00\n"));
        let again = load_roblox_mesh(&bytes).unwrap();
        assert_eq!(again.primitives[0].vertices.len(), original.primitives[0].vertices.len());
        assert_eq!(again.primitives[0].indices.len(), original.primitives[0].indices.len());
        assert!(again.native_size.abs_diff_eq(original.native_size, 1e-5));
        assert!(again.primitives[0].vertices[0].uv == original.primitives[0].vertices[0].uv, "UVs are kept");
    }

    #[test]
    fn to_roblox_mesh_counts_and_bounds_match_the_source() {
        // An L-shaped mesh: two primitives, the second offset from the first.
        let quad = |x: f32| {
            let v = |px: f32, py: f32| crate::mesh::Vertex {
                position: [x + px, py, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
            };
            crate::assets::MeshPrimitive {
                vertices: vec![v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(0.0, 1.0)],
                indices: vec![0, 1, 2, 0, 2, 3],
                texture: None,
            }
        };
        let source = LoadedMesh::from_primitives(vec![quad(0.0), quad(5.0)], true).unwrap();
        let bytes = to_roblox_mesh(&source).unwrap();
        let back = load_roblox_mesh(&bytes).unwrap();
        // Both primitives are merged into one mesh of four triangles.
        assert_eq!(back.primitives.len(), 1);
        assert_eq!(back.primitives[0].indices.len(), 12);
        assert_eq!(back.primitives[0].vertices.len(), 8);
        assert!(back.native_size.abs_diff_eq(source.native_size, 1e-5));
        // The written positions are centered, so the read-back origin is the box middle.
        let (lo, hi) = bounds(&back);
        assert!(((lo + hi) * 0.5).length() < 1e-5);
        assert!((hi - lo).abs_diff_eq(source.native_size, 1e-5));
    }

    #[test]
    fn converts_obj_and_glb_files() {
        let dir = tempfile::tempdir().unwrap();
        let obj = dir.path().join("quad.obj");
        std::fs::write(&obj, crate::assets::fixtures::QUAD_OBJ).unwrap();
        let bytes = convert_mesh_file(&obj).unwrap();
        assert_square(&load_roblox_mesh(&bytes).unwrap(), 2);

        let gltf = crate::assets::fixtures::quad_gltf(dir.path(), "quad.gltf", [255, 255, 255, 255]);
        let bytes = convert_mesh_file(&gltf).unwrap();
        assert_square(&load_roblox_mesh(&bytes).unwrap(), 2);

        assert!(convert_mesh_file(&dir.path().join("nope.txt")).is_err());
    }

    #[test]
    fn union_geometry_is_read_without_recentering() {
        let mesh = load_union_mesh(&union_square_bytes()).unwrap();
        assert!(mesh.native_size.abs_diff_eq(Vec3::new(4.0, 4.0, 0.0), 1e-5));
        let xs: Vec<f32> = mesh.primitives[0].vertices.iter().map(|v| v.position[0]).collect();
        assert!(xs.iter().all(|&x| (8.0..=12.0).contains(&x)), "positions are kept: {xs:?}");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(load_roblox_mesh(b"version 9.99\nnope").is_err());
        assert!(load_roblox_mesh(b"not a mesh at all").is_err());
        assert!(load_union_mesh(b"CSGK").is_err());
        assert!(!is_mesh(b"\x89PNG"));
        assert!(is_mesh(b"version 2.00\n"));
    }

    #[test]
    fn asset_ids_are_parsed_from_every_form() {
        assert_eq!(asset_id("rbxassetid://123"), Some(123));
        assert_eq!(asset_id("https://www.roblox.com/asset/?id=456"), Some(456));
        assert_eq!(asset_id("http://roblox.com/asset/?id=7"), Some(7));
        assert_eq!(asset_id("https://www.roblox.com/asset/?version=1&id=8"), Some(8));
        assert_eq!(asset_id("https://assetdelivery.roblox.com/v1/asset/?id=9"), Some(9));
        assert_eq!(asset_id("www.roblox.com/asset?id=10"), Some(10));
        assert_eq!(asset_id("https://www.roblox.com/library/11"), None);
        assert_eq!(asset_id("https://example.com/asset/?id=12"), None);
        assert_eq!(asset_id("rbxassetid://abc"), None);
        assert_eq!(asset_id("rbxassetid://0"), None);
        assert_eq!(asset_id("kahua://assets/meshes/a.obj"), None);
    }
}
