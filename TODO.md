# TODO

What is left to do, roughly in planned order.

## Panels (next)
- [ ] Properties grouped by category (Appearance, Behavior, Part, Transform…), collapsible like Studio
- [ ] Edit Attributes and Tags
- [ ] Material picker with previews, BrickColor palette
- [ ] "+" button in Explorer with class search instead of the long Insert Object menu
- [ ] Dockable, rearrangeable panels with tabs

## Assets and build
- [ ] `assets.lock`: file hash → Roblox asset ID, committed to git
- [ ] `kahua build` replaces `kahua://` URIs with `rbxassetid://` from the lock file
- [ ] Decals and Textures on part faces (render + insert)
- [ ] Read OBJ materials (`.mtl`, `map_Kd`) and texture alpha (cutouts)
- [ ] Extract textures embedded in `.glb` into `assets/textures/` on import

## Account and publishing
- [ ] First verify on a real account: how meshes are uploaded (Studio's internal endpoints vs Open Cloud "Model" assets) and how to get an image ID (not a decal ID) for textures
- [ ] Check whether Roblox computes MeshPart collision (`PhysicsData`) itself when it is missing
- [ ] Login (session stored in the system keyring, never in the project), optional Open Cloud API key
- [ ] Download meshes/textures of existing places into `.kahua/cache` so imported places render properly
- [ ] Upload new assets and publish the place

## Viewport and tools
- [ ] Pass rotate snap through `ViewCtx` instead of copying it from the toolbar every frame
- [ ] Surface drag: snap the object's corner, not the grabbed point
- [ ] Scale: snap face positions, not the size
- [ ] Unions (`UnionOperation`) still render as boxes
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
