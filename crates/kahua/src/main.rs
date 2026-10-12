//! `kahua`: open source editor for Roblox places.
//!
//! Without a subcommand it opens the editor window. The subcommands work
//! without a display (CI, servers, scripts).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use kahua_core::Document;
use kahua_io::rbx_dom_weak::WeakDom;
use kahua_io::rbx_dom_weak::types::Ref;
use kahua_io::{CreateOptions, project};
use kahua_render::glam::Vec3;
use kahua_render::{Camera, Headless, Scene, scene};

#[derive(Parser)]
#[command(name = "kahua", version, about = "Open source editor for Roblox places")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the editor window, optionally with a project or place file.
    Edit { path: Option<PathBuf> },

    /// Create a new project (a Rojo-compatible directory with git).
    New {
        /// Directory to create.
        dir: PathBuf,
        /// Project name (defaults to the directory name).
        #[arg(long)]
        name: Option<String>,
        /// Start from an empty place instead of the baseplate template.
        #[arg(long)]
        empty: bool,
        /// Don't run `git init`.
        #[arg(long)]
        no_git: bool,
    },

    /// Turn a .rbxl/.rbxlx/.rbxm/.rbxmx file into a new project directory.
    Import {
        file: PathBuf,
        /// Directory to create (defaults to the file name without extension).
        dir: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        no_git: bool,
    },

    /// Write a project or file out as .rbxl, .rbxlx, .rbxm or .rbxmx.
    Build {
        /// Project directory or place file (defaults to the current directory).
        #[arg(default_value = ".")]
        input: PathBuf,
        /// Output file; the extension picks the format.
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Show the instance tree of a project or file.
    Info {
        #[arg(default_value = ".")]
        input: PathBuf,
        /// How many levels of the tree to print.
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
    },

    /// Render a screenshot without opening a window.
    Render {
        #[arg(default_value = ".")]
        input: PathBuf,
        /// Output image (.png).
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        /// Camera position, e.g. `--camera 40,30,40`.
        #[arg(long, value_parser = parse_vec3)]
        camera: Option<Vec3>,
        /// Point the camera looks at (default: the scene's center).
        #[arg(long, value_parser = parse_vec3)]
        look_at: Option<Vec3>,
        /// Frame an instance, e.g. `--focus Workspace.Car`.
        #[arg(long)]
        focus: Option<String>,
    },
}

fn main() {
    // Exit quietly when output is piped into something like `head`.
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload().downcast_ref::<String>().map(String::as_str).unwrap_or("");
        if msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        eprintln!("{info}");
    }));
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => open_gui(None),
        Some(Command::Edit { path }) => open_gui(path),
        Some(Command::New { dir, name, empty, no_git }) => {
            let name = name.unwrap_or_else(|| dir_name(&dir));
            let options = CreateOptions { git: !no_git };
            let dom = if empty { kahua_io::template::empty() } else { kahua_io::template::baseplate() };
            let (_, warnings) = project::create_from_dom(&dir, &name, &dom, &options)?;
            print_warnings(&warnings);
            println!("Created project {name} in {}", dir.display());
            Ok(())
        }
        Some(Command::Import { file, dir, name, no_git }) => {
            let dir = dir.unwrap_or_else(|| file.with_extension(""));
            let name = name.unwrap_or_else(|| dir_name(&dir));
            let warnings = project::import(&file, &dir, &name, &CreateOptions { git: !no_git })?;
            print_warnings(&warnings);
            println!("Imported {} into {}", file.display(), dir.display());
            Ok(())
        }
        Some(Command::Build { input, output }) => {
            let doc = open(&input)?;
            doc.export(&output)?;
            println!("Wrote {}", output.display());
            Ok(())
        }
        Some(Command::Info { input, depth }) => {
            let doc = open(&input)?;
            let dom = &doc.dom;
            let total = dom.descendants().count() - 1;
            let scripts = dom.descendants().filter(|i| kahua_io::reflect::is_script(&i.class)).count();
            let parts = dom.descendants().filter(|i| scene::is_renderable(i)).count();
            println!("{}: {total} instances, {parts} parts, {scripts} scripts", doc.title());
            for &child in dom.root().children() {
                print_tree(dom, child, 0, depth);
            }
            Ok(())
        }
        Some(Command::Render { input, output, width, height, camera, look_at, focus }) => {
            let doc = open(&input)?;
            let scene = Scene::extract(&doc.dom);
            let mut cam = kahua_render::default_camera(&scene);
            if let Some(path) = focus {
                let target = find_path(&doc.dom, &path).with_context(|| format!("{path} not found"))?;
                let corners =
                    scene.parts.iter().filter(|p| is_inside(&doc.dom, p.owner, target)).flat_map(|p| p.corners());
                let (min, max) = scene::bounds_of(corners).with_context(|| format!("{path} has no visible parts"))?;
                cam.frame(min, max);
            }
            if let Some(position) = camera {
                let target = look_at.unwrap_or(cam.position + cam.forward() * 50.0);
                cam = Camera::default();
                cam.look_at(position, target);
            } else if let Some(target) = look_at {
                let distance = cam.position.distance(target);
                let position = target - cam.forward() * distance;
                cam.look_at(position, target);
            }
            let mut headless = Headless::new()?;
            let image = headless.render(&scene, &cam, &Default::default(), width, height)?;
            image.save(&output).with_context(|| format!("failed to write {}", output.display()))?;
            println!(
                "Rendered {} parts to {} ({width}x{height}, {})",
                scene.parts.len(),
                output.display(),
                headless.adapter_name()
            );
            Ok(())
        }
    }
}

#[cfg(feature = "gui")]
fn open_gui(path: Option<PathBuf>) -> Result<()> {
    kahua_gui::run(path)
}

#[cfg(not(feature = "gui"))]
fn open_gui(_path: Option<PathBuf>) -> Result<()> {
    bail!("this build of kahua has no editor window (built without the `gui` feature); see `kahua --help`")
}

fn open(input: &Path) -> Result<Document> {
    if !input.exists() {
        bail!("{} does not exist", input.display());
    }
    if input.is_dir() && !kahua_io::is_project_dir(input) {
        bail!("{} is not a project (no default.project.json)", input.display());
    }
    let (doc, warnings) = Document::open(input)?;
    print_warnings(&warnings);
    Ok(doc)
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

fn dir_name(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
}

fn print_tree(dom: &WeakDom, r: Ref, level: usize, max: usize) {
    let inst = dom.get_by_ref(r).unwrap();
    let extra = if inst.children().is_empty() || level + 1 < max {
        String::new()
    } else {
        format!(" (+{} children)", inst.children().len())
    };
    println!("{}{} [{}]{extra}", "  ".repeat(level), inst.name, inst.class);
    if level + 1 < max {
        for &child in inst.children() {
            print_tree(dom, child, level + 1, max);
        }
    }
}

fn find_path(dom: &WeakDom, path: &str) -> Option<Ref> {
    let mut current = dom.root_ref();
    for name in path.split('.').filter(|s| !s.is_empty()) {
        current = *dom
            .get_by_ref(current)?
            .children()
            .iter()
            .find(|c| dom.get_by_ref(**c).is_some_and(|i| i.name == name))?;
    }
    Some(current)
}

fn is_inside(dom: &WeakDom, node: Ref, ancestor: Ref) -> bool {
    let mut current = node;
    while current.is_some() {
        if current == ancestor {
            return true;
        }
        current = dom.get_by_ref(current).map_or(Ref::none(), |i| i.parent());
    }
    false
}

fn parse_vec3(s: &str) -> Result<Vec3, String> {
    let parts: Vec<f32> =
        s.split(',').map(|p| p.trim().parse::<f32>().map_err(|e| format!("{p}: {e}"))).collect::<Result<_, _>>()?;
    match parts.as_slice() {
        [x, y, z] => Ok(Vec3::new(*x, *y, *z)),
        _ => Err("expected three numbers like 10,5,-3".into()),
    }
}
