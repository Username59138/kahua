# TODO

What is left to do, roughly in planned order.

## Panels (next)
- [ ] Properties grouped by category (Appearance, Behavior, Part, Transform…), collapsible like Studio
- [ ] Edit Attributes and Tags
- [ ] Material picker with previews, BrickColor palette
- [ ] "+" button in Explorer with class search instead of the long Insert Object menu
- [ ] Dockable, rearrangeable panels with tabs

## Assets and build
- [x] `assets.lock`: file hash → Roblox asset ID, committed to git
- [x] `kahua build` replaces `kahua://` URIs with `rbxassetid://` from the lock file
- [ ] Decals and Textures on part faces (render + insert)
- [ ] Textures embedded in `.glb` are shown in Kahua but not uploaded: extract them into `assets/textures/` and set `TextureContent` on import
- [ ] Read OBJ materials (`.mtl`, `map_Kd`) and texture alpha (cutouts)

## Account and publishing
- [x] Open Cloud client: image and mesh upload, operation polling, rate limits, place publishing
- [x] Login cookie and API key in the system keyring (file fallback with 0600), `kahua login/logout/whoami`, File > Roblox Account
- [x] Download meshes/textures/unions of existing places into `.kahua/cache`
- [x] `kahua publish` and File > Publish to Roblox
- [ ] **Verify on a real account** (nothing has touched live Roblox yet): the `"Mesh"` assetType string (in `kahua-net/src/open_cloud.rs`), that our version 2.00 `.mesh` files are accepted, image uploads, place publishing, assetdelivery downloads
- [ ] Check whether Roblox computes MeshPart collision (`PhysicsData`) itself when it is missing
- [ ] Union `MeshData` handling is tested only with generated data; check with a real place
- [ ] Windows Studio cookie discovery (only Vinegar's Wine registry is read)
- [ ] OAuth "Sign in with Roblox" instead of pasting an API key (needs a registered OAuth app)

## Viewport and tools
- [ ] Pass rotate snap through `ViewCtx` instead of copying it from the toolbar every frame
- [ ] Surface drag: snap the object's corner, not the grabbed point
- [ ] Scale: snap face positions, not the size
- [ ] Unions render from their data, but clicking still uses the bounding box
- [ ] Grid and orientation cube, camera speed setting

## Script editor
- [ ] Ctrl+Z inside the editor uses the text field's own undo, not the project history
- [ ] Dedent `end`/`else`/`until` to the opening line's indentation, not just one level
- [ ] Whole-word toggle looks enabled even when it is off
- [ ] Unicode-aware case-insensitive search (now ASCII only)
- [ ] Autocomplete and type errors via luau-lsp

## General
- [ ] Ctrl+P quick search (instances, scripts, commands)
- [ ] Settings window (shortcuts, camera, theme)
- [ ] Autosave backup copy
- [ ] Rojo nested projects and `.json`/`.csv`/`.toml` files (now skipped with a warning)
- [ ] References between instances in different files (welds, PrimaryPart across files) are lost unless stored as `.rbxm`
- [ ] Packaging: Flathub, AUR, release binaries
