//! Built-in place templates.

use rbx_dom_weak::types::{CFrame, Color3, Content, Matrix3, Vector3};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

const SERVICES: &[&str] = &[
    "Workspace",
    "Players",
    "Lighting",
    "ReplicatedFirst",
    "ReplicatedStorage",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "Teams",
    "SoundService",
];

/// A place with the standard services and nothing else.
pub fn empty() -> WeakDom {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    for &service in SERVICES {
        let mut builder = InstanceBuilder::new(service).with_name(service);
        if service == "StarterPlayer" {
            builder = builder
                .with_child(InstanceBuilder::new("StarterPlayerScripts").with_name("StarterPlayerScripts"))
                .with_child(InstanceBuilder::new("StarterCharacterScripts").with_name("StarterCharacterScripts"));
        }
        dom.insert(root, builder);
    }
    dom
}

pub fn part(name: &str, class: &str, position: Vector3, size: Vector3, color: Color3) -> InstanceBuilder {
    InstanceBuilder::new(class)
        .with_name(name)
        .with_property("Anchored", true)
        .with_property("Size", size)
        .with_property("CFrame", CFrame::new(position, Matrix3::identity()))
        .with_property("Color", color)
}

/// An anchored MeshPart showing the mesh at `mesh_uri` (a `kahua://` URI).
/// `initial_size` is the mesh's own bounding-box size; the part starts at that
/// size so the mesh appears unscaled.
pub fn mesh_part(name: &str, mesh_uri: &str, initial_size: Vector3, position: Vector3) -> InstanceBuilder {
    InstanceBuilder::new("MeshPart")
        .with_name(name)
        .with_property("Anchored", true)
        .with_property("MeshContent", Content::from_uri(mesh_uri))
        .with_property("InitialSize", initial_size)
        .with_property("Size", initial_size)
        .with_property("CFrame", CFrame::new(position, Matrix3::identity()))
}

/// The classic Baseplate template: a large grey plate and a spawn point.
pub fn baseplate() -> WeakDom {
    let mut dom = empty();
    let workspace = workspace(&dom);
    dom.insert(
        workspace,
        part(
            "Baseplate",
            "Part",
            Vector3::new(0.0, -8.0, 0.0),
            Vector3::new(2048.0, 16.0, 2048.0),
            Color3::new(0.357, 0.365, 0.412),
        )
        .with_property("Locked", true),
    );
    dom.insert(
        workspace,
        part(
            "SpawnLocation",
            "SpawnLocation",
            Vector3::new(0.0, 0.5, 0.0),
            Vector3::new(12.0, 1.0, 12.0),
            Color3::new(0.639, 0.635, 0.647),
        ),
    );
    dom
}

pub fn workspace(dom: &WeakDom) -> rbx_dom_weak::types::Ref {
    dom.root()
        .children()
        .iter()
        .copied()
        .find(|r| dom.get_by_ref(*r).is_some_and(|i| i.class == "Workspace"))
        .unwrap_or_else(|| dom.root_ref())
}
