# Kahua

**An open source editor for Roblox places, native on Linux.**

Kahua opens and saves `.rbxl` places and Rojo projects, shows them in 3D, and lets you edit
instances, properties and scripts — without Wine, without Sober, and without an account.
It also runs headless: render screenshots of a place from the command line or in CI.

![The Kahua editor](docs/editor.png)

> **Status: early (v0.1).** Kahua is usable for building simple maps and editing scripts and
> properties, but it is not a full Roblox Studio replacement yet. See [what works](#what-works)
> and the [roadmap](#roadmap).

## What works

- **Projects like Godot's project manager:** create a project, open a folder, or import an
  existing `.rbxl` — every new project is a Rojo-compatible directory with git initialized.
- **Open and save place files directly** (`.rbxl`, `.rbxlx`, `.rbxm`, `.rbxmx`), lossless.
- **Explorer** with filtering, rename (F2), drag-and-drop to reparent, insert object, copy/paste,
  duplicate, group/ungroup.
- **Properties** for every property in the Roblox reflection database: checkboxes, numbers,
  vectors, colors, enums, text, plus Position and Orientation like Studio. Multi-selection edits.
- **3D viewport** for parts (block, ball, cylinder, wedge, corner wedge), colors and
  transparency, Studio-style camera (right mouse to look, WASD/QE to fly, wheel to zoom, F to focus),
  click or box select, and Move, Scale and Rotate tools with grid and angle snapping, world or
  local axes, and dragging parts across surfaces.
- **MeshParts from files in your project** (`.glb`, `.gltf`, `.obj`) with their textures, no
  account needed: Insert → Mesh from File, or `kahua add-mesh`.
- **Script editor** with Luau syntax highlighting, find and replace, go to line, auto-indent,
  block indent/unindent, comment toggling and bracket matching.
- **Publishing to Roblox** through Open Cloud: changed meshes and textures are uploaded
  (meshes are converted to Roblox's mesh format), their IDs are kept in `assets.lock`, then the
  place is published. File → Publish to Roblox, or `kahua publish`.
- **Meshes and unions of existing places** are downloaded and shown when you log in
  (File → Roblox Account), and cached in `.kahua/cache`.
- **Undo/redo** for everything.
- **CLI and headless rendering** that need no display or GPU.

![Headless render](docs/render.png)

## Install

You need [Rust](https://rustup.rs) 1.95 or newer.

```sh
git clone https://github.com/Username59138/kahua
cd kahua
cargo install --path crates/kahua
```

On a server without a GPU, install a software Vulkan driver for rendering
(`sudo apt install mesa-vulkan-drivers` on Debian/Ubuntu). For a CLI-only build without the
editor window: `cargo install --path crates/kahua --no-default-features`.

## Usage

Run `kahua` to open the project manager, or open something directly:

```sh
kahua edit my-game/          # a project folder
kahua edit place.rbxl        # a place file
```

Command line, no window needed:

```sh
kahua new my-game                       # new project (baseplate template, git init)
kahua import place.rbxl my-game         # turn a place file into a project
kahua build my-game -o my-game.rbxl     # project -> place file (also .rbxlx/.rbxm)
kahua info place.rbxl --depth 3         # print the instance tree
kahua render my-game -o shot.png        # screenshot without a window
kahua render place.rbxl --focus Workspace.Car --width 1920 --height 1080
kahua add-mesh my-game sword.glb --position 0,5,0   # copy a mesh into the project
kahua login --api-key                   # Open Cloud key, asked for interactively
kahua login --from-studio               # reuse Roblox Studio's login (under Vinegar)
kahua publish my-game                   # upload changed assets and publish
kahua publish my-game --dry-run         # show what would be uploaded
```

### Editor shortcuts

| Action | Keys |
| --- | --- |
| Look around / fly | Right mouse drag, WASD, Q/E (Shift = slower) |
| Zoom / focus selection | Mouse wheel / F |
| Select / Move / Scale / Rotate | Ctrl+1 / Ctrl+2 / Ctrl+3 / Ctrl+4 |
| World or local axes | Ctrl+L |
| Select all | Ctrl+A |
| Select a part inside a model | Alt+click |
| Undo / Redo | Ctrl+Z / Ctrl+Y |
| Duplicate / Delete / Rename | Ctrl+D / Del / F2 |
| Copy / Paste / Paste into | Ctrl+C / Ctrl+V / Ctrl+Shift+V |
| Group / Ungroup | Ctrl+G / Ctrl+U |
| Rotate / Tilt 90° | Ctrl+R / Ctrl+T |
| Save | Ctrl+S |
| Script: find / replace / go to line | Ctrl+F / Ctrl+H / Ctrl+G |
| Script: indent / unindent / comment | Tab / Shift+Tab / Ctrl+/ |

## Project format

A Kahua project is a [Rojo](https://rojo.space) project, so you can keep using `rojo serve`
with Roblox Studio and Kahua side by side:

```
my-game/
├── default.project.json    # Rojo project file
├── kahua.toml              # Kahua settings (safe to commit)
└── src/
    ├── Workspace/
    │   ├── Baseplate.model.json
    │   └── Car/
    │       ├── init.meta.json
    │       ├── Body.model.json
    │       └── Drive.client.luau
    └── ServerScriptService/
        └── Main.server.luau
```

- Scripts are plain `.luau` files. Instances that contain scripts become folders.
- Everything else is a readable `.model.json`, so map changes show up as small git diffs.
- Meshes and textures live in `assets/` and are referenced as `kahua://assets/meshes/sword.glb`.
  Publishing uploads them and records the Roblox asset IDs in `assets.lock` (commit it, so
  nobody uploads the same file twice). The project keeps the `kahua://` references.
- Publishing settings are in `kahua.toml`:
  ```toml
  [publish]
  universe_id = 1234567890
  place_id = 1234567890
  creator_user_id = 123     # or creator_group_id
  ```
- Instances that reference each other (welds, `PrimaryPart`) or hold binary data are stored as
  `.rbxm` so nothing is lost.
- Existing Rojo projects keep their own layout: Kahua writes changes back where they came from
  and only deletes files it loaded itself.

## How it's built

| Crate | What it does |
| --- | --- |
| `kahua-io` | Place files and Rojo projects, on top of [rbx-dom](https://github.com/rojo-rbx/rbx-dom) |
| `kahua-core` | The document, selection and undo/redo |
| `kahua-render` | wgpu renderer used by both the window and `kahua render` |
| `kahua-gui` | The editor window (egui) |
| `kahua-net` | Roblox Open Cloud client, login session, credential storage |
| `kahua` | The `kahua` command |

## Roadmap

- Decals and textures on part faces
- Properties grouped by category, Attributes and Tags, dockable panels
- Luau autocompletion and type checking via luau-lsp
- Optional login to publish places (internal Studio endpoints or Open Cloud)
- Terrain, GUI editing, a minimal playtest mode

Contributions are welcome — open an issue to talk about bigger changes first.

## Accounts and security

- The Open Cloud API key and the login cookie are stored in your system keyring (Secret Service),
  or in `~/.config/kahua/credentials.toml` with `0600` permissions when no keyring is available.
  They are never written into projects or logs.
- The cookie is only used to download assets of existing places. Never share it: it gives full
  access to your account.

## License

Kahua is free software under the [GNU GPL v3 or later](LICENSE). The license covers Kahua's
code only: places and projects you make with Kahua are yours.

Icons are from [Lucide](https://lucide.dev) (ISC license, see `crates/kahua-gui/assets/icons/LICENSE`).

Kahua is not affiliated with, endorsed by, or connected to Roblox Corporation. "Roblox" and
"Roblox Studio" are trademarks of Roblox Corporation.
