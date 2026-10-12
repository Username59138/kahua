use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use kahua_io::rbx_dom_weak::types::{CFrame, Color3, Matrix3, Ref, Variant, Vector3};
use kahua_io::rbx_dom_weak::{InstanceBuilder, WeakDom};
use kahua_io::{CreateOptions, Project, place, reflect, template};

/// A comparable description of a subtree: names, classes, non-default
/// properties (references as target paths), children sorted.
#[derive(Debug, PartialEq)]
struct Node {
    name: String,
    class: String,
    props: BTreeMap<String, String>,
    children: Vec<Node>,
}

fn describe(dom: &WeakDom, r: Ref) -> Node {
    let inst = dom.get_by_ref(r).unwrap();
    let mut props = BTreeMap::new();
    for (k, v) in &inst.properties {
        // Binary files store colors as 8-bit, like Roblox itself.
        let v = &normalize(v);
        let is_default = reflect::default_value(&inst.class, k).is_some_and(|d| normalize(d) == *v)
            || reflect::is_default(&inst.class, k, v);
        if is_default {
            continue;
        }
        let text = match v {
            Variant::Ref(target) if target.is_none() => continue,
            Variant::Ref(target) => format!("ref:{}", dom.full_path_of(*target, ".")),
            other => format!("{other:?}"),
        };
        props.insert(k.to_string(), text);
    }
    let mut children: Vec<Node> = inst.children().iter().map(|&c| describe(dom, c)).collect();
    children.sort_by(|a, b| (&a.name, &a.class).cmp(&(&b.name, &b.class)));
    Node { name: inst.name.clone(), class: inst.class.to_string(), props, children }
}

fn normalize(v: &Variant) -> Variant {
    match v {
        Variant::Color3(c) => {
            let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            Variant::Color3uint8(kahua_io::rbx_types::Color3uint8::new(q(c.r), q(c.g), q(c.b)))
        }
        other => other.clone(),
    }
}

fn root(dom: &WeakDom) -> Node {
    let mut node = describe(dom, dom.root_ref());
    node.name = String::new();
    node
}

fn find(dom: &WeakDom, path: &[&str]) -> Ref {
    let mut current = dom.root_ref();
    for name in path {
        current = *dom
            .get_by_ref(current)
            .unwrap()
            .children()
            .iter()
            .find(|&&c| dom.get_by_ref(c).unwrap().name == *name)
            .unwrap_or_else(|| panic!("{name} not found"));
    }
    current
}

fn no_git() -> CreateOptions {
    CreateOptions { git: false }
}

fn rich_place() -> WeakDom {
    let mut dom = template::baseplate();
    let ws = template::workspace(&dom);
    let sss = find(&dom, &["ServerScriptService"]);
    let rs = find(&dom, &["ReplicatedStorage"]);

    dom.insert(
        sss,
        InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('hello from Kahua')\n"),
    );
    // Two siblings with the same name, plus a name that is not a valid file name.
    for _ in 0..2 {
        dom.insert(
            ws,
            template::part(
                "Part",
                "Part",
                Vector3::new(1.0, 2.0, 3.0),
                Vector3::new(4.0, 1.0, 2.0),
                Color3::new(1.0, 0.0, 0.0),
            ),
        );
    }
    dom.insert(ws, InstanceBuilder::new("Folder").with_name("a/b: c?"));

    // A model with a script inside and a property on the model itself.
    let car = dom.insert(ws, InstanceBuilder::new("Model").with_name("Car"));
    dom.insert(car, InstanceBuilder::new("LocalScript").with_name("Drive").with_property("Source", "-- drive"));
    dom.insert(
        car,
        template::part(
            "Body",
            "Part",
            Vector3::new(0.0, 5.0, 0.0),
            Vector3::new(6.0, 2.0, 10.0),
            Color3::new(0.0, 0.0, 1.0),
        ),
    );

    // A script with children and a disabled flag.
    let lib =
        dom.insert(rs, InstanceBuilder::new("ModuleScript").with_name("Lib").with_property("Source", "return {}"));
    dom.insert(lib, InstanceBuilder::new("ModuleScript").with_name("Util").with_property("Source", "return 1"));
    dom.insert(lib, InstanceBuilder::new("StringValue").with_name("Version").with_property("Value", "1.0"));

    // A welded assembly: references must survive (stored as .rbxm).
    let door = dom.insert(ws, InstanceBuilder::new("Model").with_name("Door"));
    let a = dom.insert(
        door,
        template::part(
            "A",
            "Part",
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 1.0, 1.0),
            Color3::new(0.5, 0.5, 0.5),
        ),
    );
    let b = dom.insert(
        door,
        template::part(
            "B",
            "WedgePart",
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 1.0),
            Color3::new(0.5, 0.5, 0.5),
        ),
    );
    dom.insert(
        door,
        InstanceBuilder::new("WeldConstraint")
            .with_name("Weld")
            .with_property("Part0", Variant::Ref(a))
            .with_property("Part1", Variant::Ref(b)),
    );
    dom.get_by_ref_mut(door).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(a));
    let _ = CFrame::new(Vector3::new(0.0, 0.0, 0.0), Matrix3::identity());
    dom
}

#[test]
fn place_file_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dom = rich_place();
    for ext in ["rbxl", "rbxlx"] {
        let path = tmp.path().join(format!("test.{ext}"));
        place::write_file(&dom, &path).unwrap();
        let back = place::read_file(&path).unwrap();
        assert_eq!(root(&dom), root(&back), "{ext} roundtrip differs");
    }
}

#[test]
fn project_roundtrip_is_lossless() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game");
    let dom = rich_place();
    let (_, warnings) = kahua_io::project::create_from_dom(&dir, "game", &dom, &no_git()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");

    // Expected layout.
    for file in [
        "default.project.json",
        "kahua.toml",
        ".gitignore",
        "src/ServerScriptService/Main.server.luau",
        "src/Workspace/Baseplate.model.json",
        "src/Workspace/Part.model.json",
        "src/Workspace/Part~2.model.json",
        "src/Workspace/Car/init.meta.json",
        "src/Workspace/Car/Drive.client.luau",
        "src/Workspace/Car/Body.model.json",
        "src/Workspace/Door.rbxm",
        "src/ReplicatedStorage/Lib/init.luau",
        "src/ReplicatedStorage/Lib/Util.luau",
    ] {
        assert!(dir.join(file).is_file(), "missing {file}");
    }
    assert_eq!(
        fs::read_to_string(dir.join("src/ServerScriptService/Main.server.luau")).unwrap(),
        "print('hello from Kahua')\n"
    );

    let loaded = Project::open(&dir).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(root(&dom), root(&loaded.dom));
}

#[test]
fn saving_again_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game");
    kahua_io::project::create_from_dom(&dir, "game", &rich_place(), &no_git()).unwrap();
    let before = snapshot(&dir);
    let mut loaded = Project::open(&dir).unwrap();
    loaded.project.save(&loaded.dom).unwrap();
    assert_eq!(before, snapshot(&dir));
}

#[test]
fn edits_are_written_back_and_stale_files_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("game");
    kahua_io::project::create_from_dom(&dir, "game", &rich_place(), &no_git()).unwrap();
    let mut loaded = Project::open(&dir).unwrap();
    let dom = &mut loaded.dom;

    let main = find(dom, &["ServerScriptService", "Main"]);
    dom.get_by_ref_mut(main).unwrap().name = "Server".into();
    let car = find(dom, &["Workspace", "Car"]);
    dom.destroy(car);
    let rs = find(dom, &["ReplicatedStorage"]);
    dom.get_by_ref_mut(rs).unwrap().name = "Shared".into();

    loaded.project.save(dom).unwrap();
    assert!(!dir.join("src/ServerScriptService/Main.server.luau").exists());
    assert!(dir.join("src/ServerScriptService/Server.server.luau").is_file());
    assert!(!dir.join("src/Workspace/Car").exists());

    let reopened = Project::open(&dir).unwrap();
    assert_eq!(root(&loaded.dom), root(&reopened.dom));
}

#[test]
fn custom_rojo_layout_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::create_dir_all(dir.join("src/shared")).unwrap();
    fs::create_dir_all(dir.join("src/server")).unwrap();
    fs::write(
        dir.join("default.project.json"),
        r#"{
  "name": "rojo-game",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "$className": "ReplicatedStorage",
      "Shared": { "$path": "src/shared" }
    },
    "ServerScriptService": {
      "$className": "ServerScriptService",
      "Server": { "$path": "src/server" }
    },
    "Workspace": {
      "$className": "Workspace",
      "$properties": { "Gravity": 50 },
      "Sign": {
        "$className": "Part",
        "$properties": { "Anchored": true, "Material": "Neon", "Size": [4, 1, 2] }
      }
    }
  }
}
"#,
    )
    .unwrap();
    fs::write(dir.join("src/shared/Config.luau"), "return { speed = 10 }\n").unwrap();
    fs::write(dir.join("src/shared/notes.json"), "{}").unwrap();
    fs::write(dir.join("src/server/init.server.luau"), "print('server')\n").unwrap();
    fs::write(dir.join("src/server/Greeting.txt"), "hi").unwrap();

    let mut loaded = Project::open(dir).unwrap();
    assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings); // notes.json is unsupported
    let sign = find(&loaded.dom, &["Workspace", "Sign"]);
    let sign_inst = loaded.dom.get_by_ref(sign).unwrap();
    assert_eq!(sign_inst.class, "Part");
    assert!(matches!(sign_inst.properties.get(&"Material".into()), Some(Variant::Enum(_))));
    let server = find(&loaded.dom, &["ServerScriptService", "Server"]);
    assert_eq!(loaded.dom.get_by_ref(server).unwrap().class, "Script");

    let before = root(&loaded.dom);
    loaded.project.save(&loaded.dom).unwrap();

    // The user's files are untouched and nothing was reorganized.
    assert_eq!(fs::read_to_string(dir.join("src/shared/Config.luau")).unwrap(), "return { speed = 10 }\n");
    assert_eq!(fs::read_to_string(dir.join("src/server/Greeting.txt")).unwrap(), "hi");
    assert!(dir.join("src/shared/notes.json").is_file());
    assert!(!dir.join("src/Workspace").exists());
    let project: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("default.project.json")).unwrap()).unwrap();
    assert_eq!(project["tree"]["ReplicatedStorage"]["Shared"]["$path"], "src/shared");
    assert!(project["tree"]["Workspace"]["Sign"]["$properties"]["Material"].is_object());

    let reopened = Project::open(dir).unwrap();
    assert_eq!(before, root(&reopened.dom));
}

#[test]
fn import_place_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("in.rbxl");
    let dom = rich_place();
    place::write_file(&dom, &file).unwrap();
    let dir = tmp.path().join("imported");
    kahua_io::project::import(&file, &dir, "imported", &no_git()).unwrap();
    let loaded = Project::open(&dir).unwrap();
    assert_eq!(root(&place::read_file(&file).unwrap()), root(&loaded.dom));
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    out
}
