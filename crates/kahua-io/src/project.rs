//! Rojo-compatible project directories.
//!
//! A project is a directory with a `default.project.json`. Kahua reads it into a
//! [`WeakDom`] and writes changes back while keeping the project's own layout:
//! files keep their format, `$path` mappings are respected, and only files that
//! Kahua itself loaded are ever deleted.
//!
//! Layout written for new content:
//! - scripts become `Name.server.luau`, `Name.client.luau` or `Name.luau`;
//! - instances that contain scripts become directories with `init.meta.json`;
//! - everything else becomes `Name.model.json`, or `Name.rbxm` when it holds
//!   data JSON can't represent (references between instances, binary data).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};
use serde_json::{Map, Value, json};

use crate::json_props::{self, JsonMeta, JsonModel};
use crate::names::StemAllocator;
use crate::place;
use crate::reflect;

pub const PROJECT_FILE: &str = "default.project.json";
pub const CONFIG_FILE: &str = "kahua.toml";
const GITIGNORE: &str = "# Kahua\n/build/\n/.kahua/\n*.kahua-tmp\n";

/// How an instance was stored on disk, so it is written back the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    ModelJson,
    Rbxm,
    Txt,
}

/// An open project directory and the bookkeeping needed to save it back.
#[derive(Debug)]
pub struct Project {
    dir: PathBuf,
    json: Value,
    /// Instances defined by nodes of the project tree, by key path from `tree`.
    nodes: Vec<(Vec<String>, Ref)>,
    origins: HashMap<Ref, Origin>,
    /// Files and directories that make up the project contents (absolute paths).
    files: BTreeSet<PathBuf>,
    dirs: BTreeSet<PathBuf>,
}

/// Result of opening a project.
pub struct Loaded {
    pub dom: WeakDom,
    pub project: Project,
    pub warnings: Vec<String>,
}

pub fn is_project_dir(dir: &Path) -> bool {
    dir.join(PROJECT_FILE).is_file()
}

impl Project {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn name(&self) -> &str {
        self.json.get("name").and_then(Value::as_str).unwrap_or("Untitled")
    }

    /// Opens a project directory.
    pub fn open(dir: &Path) -> Result<Loaded> {
        let dir = dir.canonicalize().with_context(|| format!("project directory {} not found", dir.display()))?;
        let project_path = dir.join(PROJECT_FILE);
        let text = fs::read_to_string(&project_path)
            .with_context(|| format!("{} not found in {}", PROJECT_FILE, dir.display()))?;
        let json: Value =
            serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", project_path.display()))?;
        let tree = json
            .get("tree")
            .and_then(Value::as_object)
            .with_context(|| format!("{PROJECT_FILE} has no \"tree\" object"))?
            .clone();

        let mut loader = Loader {
            dir: dir.clone(),
            dom: WeakDom::new(InstanceBuilder::new("DataModel")),
            nodes: Vec::new(),
            origins: HashMap::new(),
            files: BTreeSet::new(),
            dirs: BTreeSet::new(),
            warnings: Vec::new(),
        };

        let root = loader.dom.root_ref();
        if tree.contains_key("$path") {
            loader.warnings.push("a \"$path\" on the tree root is not supported yet and was ignored".into());
        }
        loader.nodes.push((Vec::new(), root));
        for (key, child) in &tree {
            if key.starts_with('$') {
                continue;
            }
            if let Some(child) = child.as_object() {
                loader.load_node(root, key, child, vec![key.clone()])?;
            }
        }

        let Loader { dom, nodes, origins, files, dirs, warnings, .. } = loader;
        Ok(Loaded { dom, project: Project { dir, json, nodes, origins, files, dirs }, warnings })
    }

    /// Creates the bookkeeping for a brand new project holding `dom`.
    /// Nothing is written until [`Project::save`].
    pub fn new_for_dom(dir: &Path, name: &str, dom: &WeakDom) -> Self {
        let mut tree = Map::new();
        tree.insert("$className".into(), json!("DataModel"));
        let root = dom.root_ref();
        let mut nodes = vec![(Vec::new(), root)];
        for &child in dom.root().children() {
            let inst = dom.get_by_ref(child).expect("child exists");
            if inst.name.starts_with('$') || tree.contains_key(&inst.name) {
                continue;
            }
            tree.insert(inst.name.clone(), json!({ "$className": inst.class.as_str() }));
            nodes.push((vec![inst.name.clone()], child));
        }
        Project {
            dir: dir.to_path_buf(),
            json: json!({ "name": name, "tree": Value::Object(tree) }),
            nodes,
            origins: HashMap::new(),
            files: BTreeSet::new(),
            dirs: BTreeSet::new(),
        }
    }

    /// Writes the DOM back to the project directory. Returns warnings about
    /// anything that could not be represented on disk.
    pub fn save(&mut self, dom: &WeakDom) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        let tree_nodes = self.reconcile_nodes(dom);
        let node_refs: HashSet<Ref> = tree_nodes.iter().map(|(_, r)| *r).collect();

        let mut emitter = Emitter {
            dom,
            origins: &self.origins,
            files: BTreeMap::new(),
            dirs: BTreeSet::new(),
            warnings: &mut warnings,
        };

        for (jpath, r) in &tree_nodes {
            let inst = dom.get_by_ref(*r).expect("reconciled nodes are alive");
            let children: Vec<Ref> = inst.children().iter().copied().filter(|c| !node_refs.contains(c)).collect();
            let node = node_at_mut(&mut self.json, jpath).expect("reconciled node exists");
            let is_root = jpath.is_empty();

            if !is_root {
                let has_class = node.contains_key("$className");
                let has_path = node.contains_key("$path");
                if has_class || !has_path {
                    node.insert("$className".into(), json!(inst.class.as_str()));
                }
                let skip: &[&str] = if reflect::is_script(&inst.class) { &["Source"] } else { &[] };
                let (props, lost) = json_props::encode_properties(inst, skip);
                emitter.report_lost(*r, &lost);
                if props.is_empty() {
                    node.shift_remove("$properties");
                } else {
                    node.insert("$properties".into(), Value::Object(props));
                }
            }

            let path = node.get("$path").and_then(Value::as_str).map(str::to_owned);
            match path {
                Some(rel) if looks_like_file(&rel) => {
                    if !children.is_empty() {
                        emitter.warnings.push(format!(
                            "{} is mapped to the file {rel}; its children can't be saved there",
                            dom.full_path_of(*r, ".")
                        ));
                    }
                    emitter.emit_node_file(*r, &self.dir.join(&rel));
                }
                Some(rel) => {
                    let abs = self.dir.join(&rel);
                    if reflect::is_script(&inst.class) {
                        let file = abs.join(format!("init.{}", script_extension(&inst.class)));
                        emitter.add_file(file, script_source(inst).into_bytes());
                    }
                    emitter.emit_children(&children, &abs);
                }
                None if !children.is_empty() => {
                    let rel = if is_root {
                        "src".to_owned()
                    } else {
                        let parts: Vec<String> = jpath.iter().map(|k| crate::names::sanitize(k)).collect();
                        format!("src/{}", parts.join("/"))
                    };
                    let abs = self.dir.join(&rel);
                    if is_root {
                        emitter.warnings.push(
                            "instances placed directly in the DataModel were saved under src/ \
                             and will load as children of the place"
                                .into(),
                        );
                    } else {
                        node.insert("$path".into(), json!(rel));
                    }
                    emitter.emit_children(&children, &abs);
                }
                None => {}
            }
        }

        let Emitter { files, dirs, .. } = emitter;

        // Write changed files.
        for (path, bytes) in &files {
            if fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
                continue;
            }
            place::write_atomic(path, bytes)?;
        }
        for dir in &dirs {
            fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        // Remove files and directories Kahua loaded that are no longer part of the place.
        for stale in self.files.iter().filter(|p| !files.contains_key(*p)) {
            if stale.is_file() {
                fs::remove_file(stale).with_context(|| format!("failed to remove {}", stale.display()))?;
            }
        }
        let mut stale_dirs: Vec<&PathBuf> = self.dirs.iter().filter(|d| !dirs.contains(*d)).collect();
        stale_dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for dir in stale_dirs {
            if fs::read_dir(dir).map(|mut it| it.next().is_none()).unwrap_or(false) {
                let _ = fs::remove_dir(dir);
            }
        }

        let text = crate::json_format::to_string(&self.json);
        let project_path = self.dir.join(PROJECT_FILE);
        if fs::read_to_string(&project_path).ok().as_deref() != Some(text.as_str()) {
            place::write_atomic(&project_path, text.as_bytes())?;
        }

        self.files = files.into_keys().collect();
        self.dirs = dirs;
        self.nodes = tree_nodes;
        Ok(warnings)
    }

    /// Drops project-tree nodes whose instance was deleted or moved, and
    /// renames node keys to follow renamed instances.
    fn reconcile_nodes(&mut self, dom: &WeakDom) -> Vec<(Vec<String>, Ref)> {
        let mut nodes = self.nodes.clone();
        nodes.sort_by_key(|(p, _)| p.len());
        let mut valid: Vec<(Vec<String>, Ref)> = Vec::new();
        // Old key path -> new key path, for nodes that were kept.
        let mut moved: HashMap<Vec<String>, Vec<String>> = HashMap::new();

        for (old_path, r) in nodes {
            if old_path.is_empty() {
                moved.insert(Vec::new(), Vec::new());
                valid.push((Vec::new(), dom.root_ref()));
                continue;
            }
            let parent_old = &old_path[..old_path.len() - 1];
            let Some(parent_new) = moved.get(parent_old).cloned() else {
                continue; // parent node was dropped, and with it this one
            };
            let parent_ref =
                valid.iter().find(|(p, _)| *p == parent_new).map(|(_, r)| *r).expect("kept parent is valid");
            let old_key = old_path.last().unwrap().clone();
            let mut current = parent_new.clone();
            current.push(old_key.clone());

            let keep = dom.get_by_ref(r).is_some_and(|inst| inst.parent() == parent_ref && !inst.name.starts_with('$'));
            if !keep {
                if let Some(parent) = node_at_mut(&mut self.json, &parent_new) {
                    parent.shift_remove(&old_key);
                }
                continue;
            }

            let name = dom.get_by_ref(r).unwrap().name.clone();
            if name != old_key {
                let parent = node_at_mut(&mut self.json, &parent_new).expect("parent exists");
                if parent.contains_key(&name) {
                    // Another node already uses that name: stop treating this
                    // instance as a project node; it is saved as a plain child.
                    parent.shift_remove(&old_key);
                    continue;
                }
                rename_key(parent, &old_key, &name);
                current.pop();
                current.push(name);
            }
            moved.insert(old_path.clone(), current.clone());
            valid.push((current, r));
        }
        valid
    }
}

fn node_at_mut<'a>(json: &'a mut Value, path: &[String]) -> Option<&'a mut Map<String, Value>> {
    let mut node = json.get_mut("tree")?.as_object_mut()?;
    for key in path {
        node = node.get_mut(key)?.as_object_mut()?;
    }
    Some(node)
}

fn rename_key(map: &mut Map<String, Value>, old: &str, new: &str) {
    let entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
    for (k, v) in entries {
        if k == old {
            map.insert(new.to_owned(), v);
        } else {
            map.insert(k, v);
        }
    }
}

fn looks_like_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    [".luau", ".lua", ".json", ".rbxm", ".rbxmx", ".txt"].iter().any(|ext| lower.ends_with(ext))
}

pub fn script_extension(class: &str) -> &'static str {
    match class {
        "Script" => "server.luau",
        "LocalScript" => "client.luau",
        _ => "luau",
    }
}

fn script_source(inst: &rbx_dom_weak::Instance) -> String {
    match inst.properties.get(&rbx_dom_weak::ustr("Source")) {
        Some(Variant::String(s)) => s.clone(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

struct Loader {
    dir: PathBuf,
    dom: WeakDom,
    nodes: Vec<(Vec<String>, Ref)>,
    origins: HashMap<Ref, Origin>,
    files: BTreeSet<PathBuf>,
    dirs: BTreeSet<PathBuf>,
    warnings: Vec<String>,
}

enum FileKind {
    Script(&'static str),
    ModelJson,
    Rbxm,
    Txt,
}

fn classify(file_name: &str) -> Option<(String, FileKind)> {
    let lower = file_name.to_ascii_lowercase();
    let strip = |n: usize| file_name[..file_name.len() - n].to_owned();
    for (suffix, class) in [
        (".server.luau", "Script"),
        (".server.lua", "Script"),
        (".client.luau", "LocalScript"),
        (".client.lua", "LocalScript"),
    ] {
        if lower.ends_with(suffix) {
            return Some((strip(suffix.len()), FileKind::Script(class)));
        }
    }
    if lower.ends_with(".meta.json") || lower.ends_with(".project.json") {
        return None;
    }
    for suffix in [".luau", ".lua"] {
        if lower.ends_with(suffix) {
            return Some((strip(suffix.len()), FileKind::Script("ModuleScript")));
        }
    }
    if lower.ends_with(".model.json") {
        return Some((strip(".model.json".len()), FileKind::ModelJson));
    }
    for suffix in [".rbxmx", ".rbxm"] {
        if lower.ends_with(suffix) {
            return Some((strip(suffix.len()), FileKind::Rbxm));
        }
    }
    if lower.ends_with(".txt") {
        return Some((strip(4), FileKind::Txt));
    }
    None
}

fn is_unsupported_data_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    !lower.ends_with(".meta.json")
        && !lower.ends_with(".project.json")
        && [".json", ".csv", ".toml", ".yaml", ".yml"].iter().any(|e| lower.ends_with(e))
}

impl Loader {
    fn rel<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, str> {
        path.strip_prefix(&self.dir).unwrap_or(path).to_string_lossy()
    }

    fn load_node(&mut self, parent: Ref, key: &str, node: &Map<String, Value>, jpath: Vec<String>) -> Result<()> {
        let class = node.get("$className").and_then(Value::as_str);
        let path = node.get("$path").and_then(Value::as_str);

        let r = match path {
            Some(rel) => {
                let abs = self.dir.join(rel);
                if abs.is_dir() {
                    self.load_dir(parent, &abs)?
                } else if abs.is_file() {
                    self.load_file(parent, &abs)?
                } else {
                    if class.is_none() {
                        self.warnings.push(format!("{key}: $path {rel} does not exist"));
                    }
                    class.map(|c| self.dom.insert(parent, InstanceBuilder::new(c)))
                }
            }
            None => Some(self.dom.insert(parent, InstanceBuilder::new(class.unwrap_or("Folder")))),
        };
        let Some(r) = r else { return Ok(()) };

        {
            let inst = self.dom.get_by_ref_mut(r).unwrap();
            inst.name = key.to_owned();
            if let Some(class) = class {
                inst.class = rbx_dom_weak::ustr(class);
            }
        }
        if let Some(props) = node.get("$properties").and_then(Value::as_object) {
            let class = self.dom.get_by_ref(r).unwrap().class;
            let decoded = json_props::decode_properties(&class, props, None, key, &mut self.warnings);
            let inst = self.dom.get_by_ref_mut(r).unwrap();
            for (k, v) in decoded {
                inst.properties.insert(k, v);
            }
        }
        self.nodes.push((jpath.clone(), r));

        for (child_key, child) in node {
            if child_key.starts_with('$') {
                continue;
            }
            if let Some(child) = child.as_object() {
                let mut child_path = jpath.clone();
                child_path.push(child_key.clone());
                self.load_node(r, child_key, child, child_path)?;
            }
        }
        Ok(())
    }

    fn read_meta(&mut self, path: &Path) -> Option<JsonMeta> {
        if !path.is_file() {
            return None;
        }
        self.files.insert(path.to_path_buf());
        let parsed = fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|t| serde_json::from_str::<JsonMeta>(&t).map_err(Into::into));
        match parsed {
            Ok(meta) => Some(meta),
            Err(e) => {
                self.warnings.push(format!("{}: {e}", self.rel(path)));
                None
            }
        }
    }

    fn apply_meta(&mut self, r: Ref, meta: &JsonMeta, context: &str, allow_class: bool) {
        if allow_class && let Some(class) = &meta.class_name {
            self.dom.get_by_ref_mut(r).unwrap().class = rbx_dom_weak::ustr(class);
        }
        let class = self.dom.get_by_ref(r).unwrap().class;
        let decoded = json_props::decode_properties(
            &class,
            &meta.properties,
            meta.attributes.as_ref(),
            context,
            &mut self.warnings,
        );
        let inst = self.dom.get_by_ref_mut(r).unwrap();
        if let Some(name) = &meta.name {
            inst.name = name.clone();
        }
        for (k, v) in decoded {
            inst.properties.insert(k, v);
        }
    }

    fn sorted_entries(&mut self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("failed to read {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.')))
            .collect();
        entries.sort();
        Ok(entries)
    }

    fn load_dir(&mut self, parent: Ref, dir: &Path) -> Result<Option<Ref>> {
        if dir != self.dir && dir.join(PROJECT_FILE).is_file() {
            self.warnings.push(format!("{}: nested projects are not supported yet; skipped", self.rel(dir)));
            return Ok(None);
        }
        let entries = self.sorted_entries(dir)?;
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

        let mut class = "Folder";
        let mut init_script = None;
        for entry in &entries {
            let file_name = entry.file_name().unwrap().to_string_lossy().to_ascii_lowercase();
            let kind = match file_name.as_str() {
                "init.server.luau" | "init.server.lua" => "Script",
                "init.client.luau" | "init.client.lua" => "LocalScript",
                "init.luau" | "init.lua" => "ModuleScript",
                _ => continue,
            };
            class = kind;
            init_script = Some(entry.clone());
            break;
        }

        self.dirs.insert(dir.to_path_buf());
        let r = self.dom.insert(parent, InstanceBuilder::new(class).with_name(name));
        if let Some(script) = &init_script {
            let source = self.read_source(script);
            self.dom
                .get_by_ref_mut(r)
                .unwrap()
                .properties
                .insert(rbx_dom_weak::ustr("Source"), Variant::String(source));
        }
        if let Some(meta) = self.read_meta(&dir.join("init.meta.json")) {
            let context = self.rel(dir).into_owned();
            self.apply_meta(r, &meta, &context, init_script.is_none());
        }

        for entry in entries {
            let file_name = entry.file_name().unwrap().to_string_lossy().to_ascii_lowercase();
            if file_name.starts_with("init.") && entry.is_file() {
                let is_init = file_name == "init.meta.json"
                    || classify(&file_name).is_some_and(|(stem, _)| stem.eq_ignore_ascii_case("init"));
                if is_init {
                    continue;
                }
            }
            if entry.is_dir() {
                self.load_dir(r, &entry)?;
            } else {
                self.load_file(r, &entry)?;
            }
        }
        Ok(Some(r))
    }

    fn read_source(&mut self, path: &Path) -> String {
        self.files.insert(path.to_path_buf());
        match fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(e) => {
                    self.warnings.push(format!("{}: not valid UTF-8, invalid bytes replaced", self.rel(path)));
                    String::from_utf8_lossy(e.as_bytes()).into_owned()
                }
            },
            Err(e) => {
                self.warnings.push(format!("{}: {e}", self.rel(path)));
                String::new()
            }
        }
    }

    fn load_file(&mut self, parent: Ref, path: &Path) -> Result<Option<Ref>> {
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some((stem, kind)) = classify(&file_name) else {
            if is_unsupported_data_file(&file_name) {
                self.warnings.push(format!("{}: this file type is not supported yet; left untouched", self.rel(path)));
            }
            return Ok(None);
        };
        let meta_path = path.with_file_name(format!("{stem}.meta.json"));
        let context = self.rel(path).into_owned();

        let r = match kind {
            FileKind::Script(class) => {
                let source = self.read_source(path);
                let r = self.dom.insert(
                    parent,
                    InstanceBuilder::new(class).with_name(stem).with_property("Source", Variant::String(source)),
                );
                if let Some(meta) = self.read_meta(&meta_path) {
                    self.apply_meta(r, &meta, &context, false);
                }
                r
            }
            FileKind::Txt => {
                let value = self.read_source(path);
                let r = self.dom.insert(
                    parent,
                    InstanceBuilder::new("StringValue").with_name(stem).with_property("Value", Variant::String(value)),
                );
                if let Some(meta) = self.read_meta(&meta_path) {
                    self.apply_meta(r, &meta, &context, false);
                }
                self.origins.insert(r, Origin::Txt);
                r
            }
            FileKind::ModelJson => {
                self.files.insert(path.to_path_buf());
                let text = fs::read_to_string(path)?;
                let model: JsonModel = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        self.warnings.push(format!("{context}: {e}"));
                        return Ok(None);
                    }
                };
                let r = self.insert_json_model(parent, &model, &stem, &context);
                self.origins.insert(r, Origin::ModelJson);
                r
            }
            FileKind::Rbxm => {
                self.files.insert(path.to_path_buf());
                let mut model = match place::read_file(path) {
                    Ok(m) => m,
                    Err(e) => {
                        self.warnings.push(format!("{context}: {e:#}"));
                        return Ok(None);
                    }
                };
                let roots = model.root().children().to_vec();
                if roots.len() != 1 {
                    self.warnings.push(format!("{context}: expected one instance, found {}", roots.len()));
                }
                let mut first = None;
                for root in roots {
                    model.transfer(root, &mut self.dom, parent);
                    self.origins.insert(root, Origin::Rbxm);
                    first.get_or_insert(root);
                }
                let Some(first) = first else { return Ok(None) };
                first
            }
        };
        Ok(Some(r))
    }

    fn insert_json_model(&mut self, parent: Ref, model: &JsonModel, name: &str, context: &str) -> Ref {
        let name = model.name.clone().unwrap_or_else(|| name.to_owned());
        let props = json_props::decode_properties(
            &model.class_name,
            &model.properties,
            model.attributes.as_ref(),
            context,
            &mut self.warnings,
        );
        let r = self
            .dom
            .insert(parent, InstanceBuilder::new(model.class_name.as_str()).with_name(name).with_properties(props));
        for child in &model.children {
            let child_name = child.name.clone().unwrap_or_else(|| child.class_name.clone());
            self.insert_json_model(r, child, &child_name, context);
        }
        r
    }
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

struct Emitter<'a> {
    dom: &'a WeakDom,
    origins: &'a HashMap<Ref, Origin>,
    files: BTreeMap<PathBuf, Vec<u8>>,
    dirs: BTreeSet<PathBuf>,
    warnings: &'a mut Vec<String>,
}

impl Emitter<'_> {
    fn add_file(&mut self, path: PathBuf, bytes: Vec<u8>) {
        self.files.insert(path, bytes);
    }

    fn add_json<T: serde::Serialize>(&mut self, path: PathBuf, value: &T) {
        self.add_file(path, crate::json_format::to_string(value).into_bytes());
    }

    fn report_lost(&mut self, r: Ref, lost: &[String]) {
        if !lost.is_empty() {
            self.warnings.push(format!(
                "{}: properties not saved ({}); references and binary data need a .rbxm file",
                self.dom.full_path_of(r, "."),
                lost.join(", ")
            ));
        }
    }

    fn emit_children(&mut self, children: &[Ref], dir: &Path) {
        self.dirs.insert(dir.to_path_buf());
        let mut stems = StemAllocator::default();
        for &child in children {
            let name = &self.dom.get_by_ref(child).unwrap().name;
            let stem = stems.allocate(name);
            self.emit_child(child, dir, &stem);
        }
    }

    fn has_script(&self, r: Ref) -> bool {
        self.dom.descendants_of(r).any(|inst| reflect::is_script(&inst.class))
    }

    fn needs_binary(&self, r: Ref) -> bool {
        self.dom.descendants_of(r).any(|inst| {
            inst.properties.iter().any(|(key, value)| {
                !json_props::is_json_representable(value) && !reflect::is_default(&inst.class, key, value)
            })
        })
    }

    fn meta_for(&mut self, r: Ref, stem: &str, class_name: Option<String>) -> JsonMeta {
        let inst = self.dom.get_by_ref(r).unwrap();
        let skip: &[&str] = if reflect::is_script(&inst.class) {
            &["Source"]
        } else if self.origins.get(&r) == Some(&Origin::Txt) {
            &["Value"]
        } else {
            &[]
        };
        let (properties, lost) = json_props::encode_properties(inst, skip);
        self.report_lost(r, &lost);
        JsonMeta { name: (stem != inst.name).then(|| inst.name.clone()), class_name, properties, ..Default::default() }
    }

    fn emit_child(&mut self, r: Ref, dir: &Path, stem: &str) {
        let inst = self.dom.get_by_ref(r).unwrap();
        let class = inst.class;

        if reflect::is_script(&class) {
            let file_name = format!("{stem}.{}", script_extension(&class));
            let meta = self.meta_for(r, stem, None);
            if inst.children().is_empty() {
                self.add_file(dir.join(file_name), script_source(inst).into_bytes());
                if !meta.is_empty() {
                    self.add_json(dir.join(format!("{stem}.meta.json")), &meta);
                }
            } else {
                let sub = dir.join(stem);
                self.add_file(sub.join(format!("init.{}", script_extension(&class))), script_source(inst).into_bytes());
                if !meta.is_empty() {
                    self.add_json(sub.join("init.meta.json"), &meta);
                }
                let children = inst.children().to_vec();
                self.emit_children(&children, &sub);
            }
        } else if self.has_script(r) {
            let class_name = (class != "Folder").then(|| class.to_string());
            let meta = self.meta_for(r, stem, class_name);
            let sub = dir.join(stem);
            if !meta.is_empty() {
                self.add_json(sub.join("init.meta.json"), &meta);
            }
            let children = inst.children().to_vec();
            self.emit_children(&children, &sub);
        } else if class == "StringValue" && inst.children().is_empty() && self.origins.get(&r) == Some(&Origin::Txt) {
            let value = match inst.properties.get(&rbx_dom_weak::ustr("Value")) {
                Some(Variant::String(s)) => s.clone(),
                _ => String::new(),
            };
            self.add_file(dir.join(format!("{stem}.txt")), value.into_bytes());
            let meta = self.meta_for(r, stem, None);
            if !meta.is_empty() {
                self.add_json(dir.join(format!("{stem}.meta.json")), &meta);
            }
        } else if self.origins.get(&r) == Some(&Origin::Rbxm) || self.needs_binary(r) {
            match place::encode_refs(self.dom, &[r], false) {
                Ok(bytes) => self.add_file(dir.join(format!("{stem}.rbxm")), bytes),
                Err(e) => self.warnings.push(format!("{}: failed to encode: {e:#}", self.dom.full_path_of(r, "."))),
            }
        } else {
            let mut model = self.json_model(r);
            if stem == inst.name {
                model.name = None;
            }
            self.add_json(dir.join(format!("{stem}.model.json")), &model);
        }
    }

    fn json_model(&mut self, r: Ref) -> JsonModel {
        let inst = self.dom.get_by_ref(r).unwrap();
        let (properties, lost) = json_props::encode_properties(inst, &[]);
        self.report_lost(r, &lost);
        let children = inst.children().iter().map(|&c| self.json_model(c)).collect();
        JsonModel {
            name: Some(inst.name.clone()),
            class_name: inst.class.to_string(),
            properties,
            attributes: None,
            children,
        }
    }

    /// Writes a project-tree node whose `$path` points at a single file.
    fn emit_node_file(&mut self, r: Ref, path: &Path) {
        let inst = self.dom.get_by_ref(r).unwrap();
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if lower.ends_with(".luau") || lower.ends_with(".lua") || lower.ends_with(".txt") {
            let text = if reflect::is_script(&inst.class) {
                script_source(inst)
            } else {
                match inst.properties.get(&rbx_dom_weak::ustr("Value")) {
                    Some(Variant::String(s)) => s.clone(),
                    _ => String::new(),
                }
            };
            self.add_file(path.to_path_buf(), text.into_bytes());
        } else if lower.ends_with(".model.json") {
            let mut model = self.json_model(r);
            model.name = None;
            self.add_json(path.to_path_buf(), &model);
        } else if lower.ends_with(".rbxm") || lower.ends_with(".rbxmx") {
            match place::encode_refs(self.dom, &[r], lower.ends_with(".rbxmx")) {
                Ok(bytes) => self.add_file(path.to_path_buf(), bytes),
                Err(e) => self.warnings.push(format!("{}: {e:#}", path.display())),
            }
        } else {
            self.warnings.push(format!("{}: saving this kind of mapped file is not supported yet", path.display()));
        }
    }
}

// ---------------------------------------------------------------------------
// Creating projects
// ---------------------------------------------------------------------------

/// Options for creating a project.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Run `git init` in the new project.
    pub git: bool,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self { git: true }
    }
}

/// Creates a new project from a DOM (a fresh template or an imported place).
pub fn create_from_dom(
    dir: &Path,
    name: &str,
    dom: &WeakDom,
    options: &CreateOptions,
) -> Result<(Project, Vec<String>)> {
    if dir.exists() && fs::read_dir(dir)?.next().is_some() {
        bail!("{} already exists and is not empty", dir.display());
    }
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let dir = dir.canonicalize()?;

    let mut project = Project::new_for_dom(&dir, name, dom);
    let mut warnings = project.save(dom)?;

    let config = crate::config::KahuaConfig::new(name);
    config.write(&dir.join(CONFIG_FILE))?;
    fs::write(dir.join(".gitignore"), GITIGNORE)?;

    if options.git {
        match Command::new("git").args(["init", "-q"]).current_dir(&dir).status() {
            Ok(status) if status.success() => {
                // Best effort: this fails quietly when git has no user configured.
                let added = Command::new("git").args(["add", "-A"]).current_dir(&dir).output();
                if added.is_ok_and(|o| o.status.success()) {
                    let _ = Command::new("git")
                        .args(["commit", "-q", "-m", "Create project with Kahua"])
                        .current_dir(&dir)
                        .output();
                }
            }
            Ok(_) => warnings.push("git init failed; the project was created without git".into()),
            Err(_) => warnings.push("git is not installed; the project was created without git".into()),
        }
    }
    Ok((project, warnings))
}

/// Creates a new project from the default template (baseplate + spawn).
pub fn create(dir: &Path, name: &str, options: &CreateOptions) -> Result<Vec<String>> {
    create_from_dom(dir, name, &crate::template::baseplate(), options).map(|(_, w)| w)
}

/// Imports a place (or model) file into a new project directory.
pub fn import(file: &Path, dir: &Path, name: &str, options: &CreateOptions) -> Result<Vec<String>> {
    let format = place::FileFormat::from_path(file)
        .with_context(|| format!("{} is not a .rbxl, .rbxlx, .rbxm or .rbxmx file", file.display()))?;
    let mut dom = place::read_file(file)?;
    if !format.is_place() {
        // Models go into the Workspace of an empty place.
        let mut place = crate::template::empty();
        let workspace = place
            .root()
            .children()
            .iter()
            .copied()
            .find(|r| place.get_by_ref(*r).unwrap().class == "Workspace")
            .expect("template has a Workspace");
        for child in dom.root().children().to_vec() {
            dom.transfer(child, &mut place, workspace);
        }
        dom = place;
    }
    create_from_dom(dir, name, &dom, options).map(|(_, w)| w)
}
