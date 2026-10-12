//! The 3D viewport: camera controls, selection (click and box), and the
//! Move, Scale and Rotate gizmos.

use std::collections::HashMap;
use std::f32::consts::TAU;

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};
use eframe::egui_wgpu::wgpu;
use kahua_core::kahua_io::rbx_dom_weak::Ustr;
use kahua_core::kahua_io::rbx_dom_weak::types::{CFrame, Ref, Variant, Vector3};
use kahua_core::kahua_io::reflect;
use kahua_core::{Document, Edit};
use kahua_render::glam::{Vec2, Vec3};
use kahua_render::scene::{self, cframe_to_mat4};
use kahua_render::{Camera, Overlay, Renderer, Scene};

use crate::ops;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Move,
    Scale,
    Rotate,
}

pub struct ViewCtx<'a> {
    pub doc: &'a mut Document,
    pub scene: &'a Scene,
    pub index: &'a HashMap<Ref, usize>,
    pub scene_changed: bool,
    pub tool: Tool,
    pub snap: Option<f32>,
}

#[derive(Default)]
pub struct ViewOutput {
    /// Instance selected by clicking, to reveal in the Explorer.
    pub picked: Option<Ref>,
}

/// Original CFrames (and model pivots) of the things a drag moves, for undo.
type Originals = Vec<(Ref, &'static str, Variant)>;

/// One grabbable part of a gizmo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    /// Move arrow along gizmo axis `i`.
    Arrow(usize),
    /// Rotation ring around gizmo axis `i`.
    Ring(usize),
    /// Scale handle on the face at +/- the part's axis `i`.
    Face(usize, bool),
}

enum Drag {
    /// Slides the selection along one gizmo axis.
    Arrow { handle: Handle, axis: Vec3, center: Vec3, start: f32, originals: Originals },
    /// Turns the selection around one gizmo axis. `start` is the grab point relative to the center.
    Ring { handle: Handle, axis: Vec3, center: Vec3, start: Vec3, originals: Originals },
    /// Resizes one part along one of its own axes.
    Face {
        handle: Handle,
        part: Ref,
        /// Unit vector pointing out of the dragged face.
        outward: Vec3,
        axis: usize,
        face: Vec3,
        start: f32,
        cf: CFrame,
        size: Vec3,
        originals: Originals,
    },
    /// Drags the selection over surfaces, keeping its bottom on the surface under the pointer.
    Surface { grab: Vec3, bottom: f32, others: Scene, originals: Originals },
    /// Rubber-band selection on empty space.
    Box { start: Pos2, current: Pos2 },
}

impl Drag {
    fn handle(&self) -> Option<Handle> {
        match self {
            Drag::Arrow { handle, .. } | Drag::Ring { handle, .. } | Drag::Face { handle, .. } => Some(*handle),
            Drag::Surface { .. } | Drag::Box { .. } => None,
        }
    }
}

/// What a gizmo is drawn around for the current tool.
struct Gizmo {
    center: Vec3,
    axes: [Vec3; 3],
    /// Distance from the center to the edge of the selection along each axis.
    extent: [f32; 3],
    /// Arrow length for Move, ring radius for Rotate.
    length: f32,
    /// The part being resized (Scale only).
    part: Option<Ref>,
}

#[derive(Default)]
pub struct Viewport {
    pub camera: Camera,
    /// Move and Rotate use the axes of the first selected part instead of the world's.
    pub local_axes: bool,
    /// Rotation snap in degrees, 0 for none. The toolbar keeps this equal to the config.
    pub rotate_snap: f32,
    renderer: Option<Renderer>,
    texture: Option<egui::TextureId>,
    framed: bool,
    drag: Option<Drag>,
    hovered: Option<Handle>,
    /// Tracks the scene uploaded to the renderer.
    uploaded: bool,
}

const AXIS_COLORS: [Color32; 3] =
    [Color32::from_rgb(242, 64, 64), Color32::from_rgb(77, 230, 77), Color32::from_rgb(77, 128, 255)];
const HIGHLIGHT: Color32 = Color32::from_rgb(255, 225, 60);
const RING_SEGMENTS: usize = 64;
const ARROW_REACH: f32 = 10.0;
const RING_REACH: f32 = 8.0;
const FACE_REACH: f32 = 10.0;

impl Viewport {
    pub fn focus(&mut self, bounds: Option<(Vec3, Vec3)>) {
        if let Some((min, max)) = bounds {
            self.camera.frame(min, max);
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, frame: &eframe::Frame, cx: ViewCtx<'_>) -> ViewOutput {
        let mut out = ViewOutput::default();
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        if rect.width() < 2.0 || rect.height() < 2.0 {
            return out;
        }
        let aspect = rect.width() / rect.height();

        if !self.framed {
            self.camera = kahua_render::default_camera(cx.scene);
            self.framed = true;
        }

        self.camera_controls(ui, &response, rect);

        let pointer = ui.input(|i| i.pointer.latest_pos());
        let (press, modifiers) = ui.input(|i| (i.pointer.press_origin(), i.modifiers));

        // Gizmo hover.
        let gizmo = self.gizmo(cx.doc, cx.scene, cx.index, cx.tool);
        self.hovered = None;
        if let (Some(g), Some(p)) = (&gizmo, pointer)
            && self.drag.is_none()
            && rect.contains(p)
        {
            self.hovered = self.hit_handle(rect, aspect, g, cx.tool, p);
        }

        // Drags. Decisions use where the press started, not where the pointer is after the threshold.
        if response.drag_started_by(PointerButton::Primary)
            && let Some(press) = press
        {
            self.drag = self.begin_drag(&cx, rect, aspect, gizmo.as_ref(), press);
        }
        if let Some(p) = pointer
            && response.dragged_by(PointerButton::Primary)
        {
            let snap = cx.snap;
            self.update_drag(cx.doc, rect, aspect, p, snap, modifiers.shift);
        }
        if response.drag_stopped_by(PointerButton::Primary)
            && let Some(drag) = self.drag.take()
        {
            let additive = modifiers.command || modifiers.shift;
            self.finish_drag(cx.doc, cx.scene, rect, aspect, drag, additive);
        }

        // Click to select.
        let hovered_part =
            pointer.filter(|p| rect.contains(*p) && self.drag.is_none() && self.hovered.is_none()).and_then(|p| {
                let (origin, dir) = self.camera.ray(to_ndc(rect, p), aspect);
                cx.scene.pick(origin, dir).map(|(i, _)| cx.scene.parts[i].owner)
            });
        let alt = ui.input(|i| i.modifiers.alt);
        let hovered_target = hovered_part.map(|r| ops::click_target(&cx.doc.dom, r, alt));
        if response.clicked_by(PointerButton::Primary) && self.hovered.is_none() {
            let toggle = ui.input(|i| i.modifiers.command || i.modifiers.shift);
            match hovered_target {
                Some(target) if toggle => {
                    if let Some(i) = cx.doc.selection.iter().position(|r| *r == target) {
                        cx.doc.selection.remove(i);
                    } else {
                        cx.doc.selection.push(target);
                    }
                    out.picked = Some(target);
                }
                Some(target) => {
                    cx.doc.selection = vec![target];
                    out.picked = Some(target);
                }
                None if !toggle => cx.doc.selection.clear(),
                None => {}
            }
        }

        // Overlay: selection boxes, hover box.
        let mut overlay = Overlay::default();
        for &r in &cx.doc.selection {
            add_bounds(&mut overlay, cx.doc, cx.scene, cx.index, r, theme::SELECTION_3D);
        }
        if let Some(target) = hovered_target
            && !cx.doc.selection.contains(&target)
        {
            add_bounds(&mut overlay, cx.doc, cx.scene, cx.index, target, theme::HOVER_3D);
        }

        // Render.
        let Some(rs) = frame.wgpu_render_state() else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "3D view needs the wgpu backend",
                egui::FontId::proportional(14.0),
                Color32::WHITE,
            );
            return out;
        };
        let renderer = self.renderer.get_or_insert_with(|| {
            let msaa =
                rs.adapter.get_texture_format_features(kahua_render::COLOR_FORMAT).flags.sample_count_supported(4);
            Renderer::new(&rs.device, if msaa { 4 } else { 1 })
        });
        if cx.scene_changed || !self.uploaded {
            renderer.set_scene(&rs.device, &rs.queue, cx.scene);
            self.uploaded = true;
        }
        let ppp = ui.ctx().pixels_per_point();
        let size = ((rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32);
        let view = renderer.render(&rs.device, &rs.queue, size, &self.camera, &overlay);
        {
            let mut egui_renderer = rs.renderer.write();
            match self.texture {
                None => {
                    self.texture =
                        Some(egui_renderer.register_native_texture(&rs.device, view, wgpu::FilterMode::Linear));
                }
                Some(id) => {
                    egui_renderer.update_egui_texture_from_wgpu_texture(&rs.device, view, wgpu::FilterMode::Linear, id)
                }
            }
        }
        if let Some(id) = self.texture {
            ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }

        // The gizmo is drawn after this frame's drag so it does not lag behind the pointer.
        let gizmo = self.gizmo(cx.doc, cx.scene, cx.index, cx.tool);
        if let Some(g) = &gizmo {
            self.paint_gizmo(ui, rect, aspect, g, cx.tool);
        }
        if let Some(Drag::Box { start, current }) = &self.drag {
            paint_box(&ui.painter_at(rect), *start, *current);
        }

        // Hints.
        let hint = match (cx.tool, gizmo.is_some()) {
            (Tool::Select, _) => "Select · drag to box select · RMB look · WASD/QE fly · Wheel zoom · F focus",
            (Tool::Move, _) => "Move · drag the arrows · RMB look · WASD/QE fly · F focus",
            (Tool::Scale, true) => "Scale · drag a face · Shift scales both sides · F focus",
            (Tool::Scale, false) => "Scale · select a single part",
            (Tool::Rotate, _) => "Rotate · drag a ring · RMB look · WASD/QE fly · F focus",
        };
        ui.painter().text(
            rect.left_bottom() + egui::vec2(10.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(12.0),
            Color32::from_white_alpha(170),
        );
        if let Some(r) = hovered_target {
            let name = cx.doc.dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default();
            if let Some(p) = pointer {
                ui.painter().text(
                    p + egui::vec2(14.0, 14.0),
                    egui::Align2::LEFT_TOP,
                    name,
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        }
        out
    }

    fn camera_controls(&mut self, ui: &mut Ui, response: &egui::Response, rect: Rect) {
        let looking = response.dragged_by(PointerButton::Secondary);
        if looking {
            let d = response.drag_delta();
            self.camera.rotate(-d.x * 0.006, -d.y * 0.006);
        }
        if response.dragged_by(PointerButton::Middle) {
            let d = response.drag_delta();
            let speed = 0.05;
            self.camera.position += -self.camera.right() * d.x * speed + self.camera.up() * d.y * speed;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                // Zoom toward the cursor, like Studio.
                let dir = ui
                    .input(|i| i.pointer.hover_pos())
                    .map(|p| self.camera.ray(to_ndc(rect, p), rect.width() / rect.height()).1)
                    .unwrap_or(self.camera.forward());
                self.camera.position += dir * scroll * 0.08;
            }
        }
        let keyboard = (response.hovered() || looking) && !ui.ctx().egui_wants_keyboard_input();
        if keyboard {
            let (dt, shift, command, keys) = ui.input(|i| {
                let k = |key| i.key_down(key);
                (
                    i.stable_dt.min(0.1),
                    i.modifiers.shift,
                    i.modifiers.command,
                    [
                        k(egui::Key::W),
                        k(egui::Key::S),
                        k(egui::Key::A),
                        k(egui::Key::D),
                        k(egui::Key::E),
                        k(egui::Key::Q),
                    ],
                )
            });
            if !command {
                let mut v = Vec3::ZERO;
                let flat_forward = self.camera.forward();
                if keys[0] {
                    v += flat_forward;
                }
                if keys[1] {
                    v -= flat_forward;
                }
                if keys[2] {
                    v -= self.camera.right();
                }
                if keys[3] {
                    v += self.camera.right();
                }
                if keys[4] {
                    v += Vec3::Y;
                }
                if keys[5] {
                    v -= Vec3::Y;
                }
                if v != Vec3::ZERO {
                    let speed = if shift { 10.0 } else { 40.0 };
                    self.camera.position += v.normalize() * speed * dt;
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    /// The gizmo for the tool and selection, or `None` when the tool has nothing to show.
    fn gizmo(&self, doc: &Document, scene: &Scene, index: &HashMap<Ref, usize>, tool: Tool) -> Option<Gizmo> {
        match tool {
            Tool::Select => None,
            Tool::Move | Tool::Rotate => {
                let (min, max) = ops::selection_bounds(doc, scene, index)?;
                let center = (min + max) * 0.5;
                let half = (max - min) * 0.5;
                let axes = frame_axes(doc, self.local_axes);
                let arrow = (self.camera.position.distance(center) * 0.12).max(1.0);
                let extent = axes.map(|a| half.dot(a.abs()));
                let length = if tool == Tool::Rotate { half.length().max(arrow) } else { arrow };
                Some(Gizmo { center, axes, extent, length, part: None })
            }
            Tool::Scale => {
                let part = single_part(doc)?;
                let cf = ops::cframe_of(&doc.dom, part, "CFrame")?;
                let half = scene::part_size(doc.dom.get_by_ref(part)?) * 0.5;
                let m = cframe_to_mat4(&cf);
                Some(Gizmo {
                    center: vec3(cf.position),
                    axes: [m.x_axis.truncate(), m.y_axis.truncate(), m.z_axis.truncate()],
                    extent: half.to_array(),
                    length: 0.0,
                    part: Some(part),
                })
            }
        }
    }

    fn hit_handle(&self, rect: Rect, aspect: f32, g: &Gizmo, tool: Tool, p: Pos2) -> Option<Handle> {
        let cam = &self.camera;
        let (candidates, reach): (Vec<(Handle, f32)>, f32) = match tool {
            Tool::Select => return None,
            Tool::Move => (
                arrows(cam, rect, aspect, g).into_iter().map(|(h, a, b)| (h, distance_to_segment(p, a, b))).collect(),
                ARROW_REACH,
            ),
            Tool::Rotate => {
                ((0..3).map(|i| (Handle::Ring(i), ring_distance(cam, rect, aspect, g, i, p))).collect(), RING_REACH)
            }
            Tool::Scale => {
                (faces(cam, rect, aspect, g).into_iter().map(|(h, q)| (h, q.distance(p))).collect(), FACE_REACH)
            }
        };
        candidates.into_iter().filter(|(_, d)| *d < reach).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(h, _)| h)
    }

    fn begin_drag(
        &self,
        cx: &ViewCtx<'_>,
        rect: Rect,
        aspect: f32,
        gizmo: Option<&Gizmo>,
        press: Pos2,
    ) -> Option<Drag> {
        if cx.tool == Tool::Select {
            return self.begin_select_drag(cx, rect, aspect, press);
        }
        let g = gizmo?;
        let handle = self.hit_handle(rect, aspect, g, cx.tool, press)?;
        let (origin, ray) = self.camera.ray(to_ndc(rect, press), aspect);
        match handle {
            Handle::Arrow(i) => {
                let axis = g.axes[i];
                let start = closest_on_axis(g.center, axis, origin, ray)?;
                let originals = capture(cx.doc);
                Some(Drag::Arrow { handle, axis, center: g.center, start, originals })
            }
            Handle::Ring(i) => {
                let axis = g.axes[i];
                let hit = plane_hit(origin, ray, g.center, axis)?;
                let originals = capture(cx.doc);
                Some(Drag::Ring { handle, axis, center: g.center, start: hit - g.center, originals })
            }
            Handle::Face(i, positive) => {
                let part = g.part?;
                let cf = ops::cframe_of(&cx.doc.dom, part, "CFrame")?;
                let size = scene::part_size(cx.doc.dom.get_by_ref(part)?);
                let outward = if positive { g.axes[i] } else { -g.axes[i] };
                let face = g.center + outward * g.extent[i];
                let start = closest_on_axis(face, outward, origin, ray)?;
                let originals =
                    vec![(part, "CFrame", Variant::CFrame(cf)), (part, "Size", Variant::Vector3(to_vector3(size)))];
                Some(Drag::Face { handle, part, outward, axis: i, face, start, cf, size, originals })
            }
        }
    }

    /// Select tool: a press on a selected part drags the selection, a press on empty space box selects.
    fn begin_select_drag(&self, cx: &ViewCtx<'_>, rect: Rect, aspect: f32, press: Pos2) -> Option<Drag> {
        let (origin, ray) = self.camera.ray(to_ndc(rect, press), aspect);
        let Some((hit, _)) = cx.scene.pick(origin, ray) else {
            return Some(Drag::Box { start: press, current: press });
        };
        let owner = cx.scene.parts[hit].owner;
        let roots = cx.doc.selection_roots();
        let selected = |r: Ref| roots.iter().any(|&root| cx.doc.is_ancestor_or_self(root, r));
        if !selected(owner) {
            return None;
        }
        let (mine, others) = split_scene(cx.scene, selected);
        let (_, t) = mine.pick(origin, ray)?;
        let grab = origin + ray * t;
        let (min, _) = ops::selection_bounds(cx.doc, cx.scene, cx.index)?;
        Some(Drag::Surface { grab, bottom: min.y, others, originals: capture(cx.doc) })
    }

    fn update_drag(
        &mut self,
        doc: &mut Document,
        rect: Rect,
        aspect: f32,
        p: Pos2,
        snap: Option<f32>,
        symmetric: bool,
    ) {
        let camera = self.camera;
        let rotate_snap = self.rotate_snap;
        let (origin, ray) = camera.ray(to_ndc(rect, p), aspect);
        let Some(drag) = self.drag.as_mut() else { return };
        match drag {
            Drag::Arrow { axis, center, start, originals, .. } => {
                let Some(s) = closest_on_axis(*center, *axis, origin, ray) else { return };
                let mut delta = s - *start;
                if let Some(step) = snap {
                    delta = (delta / step).round() * step;
                }
                let offset = *axis * delta;
                apply_originals(doc, originals, |cf| translate(cf, offset));
            }
            Drag::Ring { axis, center, start, originals, .. } => {
                let Some(hit) = plane_hit(origin, ray, *center, *axis) else { return };
                let mut angle = signed_angle(*start, hit - *center, *axis);
                if rotate_snap > 0.0 {
                    let step = rotate_snap.to_radians();
                    angle = (angle / step).round() * step;
                }
                let (axis, center) = (*axis, *center);
                apply_originals(doc, originals, |cf| ops::rotate_cframe_about(cf, center, axis, angle));
            }
            Drag::Face { part, outward, axis, face, start, cf, size, .. } => {
                let Some(s) = closest_on_axis(*face, *outward, origin, ray) else { return };
                let (len, new_center) =
                    ops::resize_along(vec3(cf.position), *outward, size[*axis], s - *start, symmetric, snap);
                let mut new_size = *size;
                new_size[*axis] = len;
                let mut new_cf = *cf;
                new_cf.position = to_vector3(new_center);
                let changed = set_live(doc, *part, "Size", Variant::Vector3(to_vector3(new_size)))
                    | set_live(doc, *part, "CFrame", Variant::CFrame(new_cf));
                if changed {
                    doc.touch();
                }
            }
            Drag::Surface { grab, bottom, others, originals } => {
                let Some((_, t)) = others.pick(origin, ray) else { return };
                let hit = origin + ray * t;
                // The grabbed point follows the pointer on X/Z; the bottom rests on the surface.
                let mut d = Vec3::new(hit.x - grab.x, hit.y - *bottom, hit.z - grab.z);
                if let Some(step) = snap {
                    d.x = (hit.x / step).round() * step - grab.x;
                    d.z = (hit.z / step).round() * step - grab.z;
                }
                apply_originals(doc, originals, |cf| translate(cf, d));
            }
            Drag::Box { current, .. } => *current = p,
        }
    }

    fn finish_drag(&self, doc: &mut Document, scene: &Scene, rect: Rect, aspect: f32, drag: Drag, additive: bool) {
        match drag {
            Drag::Arrow { originals, .. } | Drag::Surface { originals, .. } => record_live(doc, "Move", &originals),
            Drag::Ring { originals, .. } => record_live(doc, "Rotate", &originals),
            Drag::Face { originals, .. } => record_live(doc, "Scale", &originals),
            Drag::Box { start, current } => self.select_box(doc, scene, rect, aspect, start, current, additive),
        }
    }

    /// Selects the click targets of every part whose center projects inside the rectangle.
    #[allow(clippy::too_many_arguments)]
    fn select_box(
        &self,
        doc: &mut Document,
        scene: &Scene,
        rect: Rect,
        aspect: f32,
        start: Pos2,
        end: Pos2,
        additive: bool,
    ) {
        if start.distance(end) < 3.0 {
            return;
        }
        let mut picked: Vec<Ref> = Vec::new();
        for part in &scene.parts {
            let Some(p) = screen(&self.camera, rect, aspect, part.position()) else { continue };
            if !inside_box(start, end, p) {
                continue;
            }
            let target = ops::click_target(&doc.dom, part.owner, false);
            if !picked.contains(&target) {
                picked.push(target);
            }
        }
        if additive {
            for r in picked {
                if !doc.selection.contains(&r) {
                    doc.selection.push(r);
                }
            }
        } else {
            doc.selection = picked;
        }
    }

    fn paint_gizmo(&self, ui: &Ui, rect: Rect, aspect: f32, g: &Gizmo, tool: Tool) {
        let painter = ui.painter_at(rect);
        let active = self.drag.as_ref().and_then(Drag::handle);
        let cam = &self.camera;
        let color = |h: Handle, i: usize| {
            if self.hovered == Some(h) || active == Some(h) { HIGHLIGHT } else { AXIS_COLORS[i] }
        };
        match tool {
            Tool::Select => {}
            Tool::Move => {
                for (h, base, tip) in arrows(cam, rect, aspect, g) {
                    let Handle::Arrow(i) = h else { continue };
                    let color = color(h, i);
                    let dir = (tip - base).normalized();
                    if !dir.x.is_finite() {
                        continue;
                    }
                    let normal = egui::vec2(-dir.y, dir.x);
                    let neck = tip - dir * 13.0;
                    painter.line_segment([base, neck], Stroke::new(3.5, color));
                    painter.add(egui::Shape::convex_polygon(
                        vec![tip, neck + normal * 6.5, neck - normal * 6.5],
                        color,
                        Stroke::NONE,
                    ));
                }
            }
            Tool::Rotate => {
                for i in 0..3 {
                    let color = color(Handle::Ring(i), i);
                    for run in ring_runs(cam, rect, aspect, g, i) {
                        painter.add(egui::Shape::line(run, Stroke::new(3.0, color)));
                    }
                }
            }
            Tool::Scale => {
                for (h, p) in faces(cam, rect, aspect, g) {
                    let Handle::Face(i, _) = h else { continue };
                    painter.circle(p, 5.5, color(h, i), Stroke::new(1.0, Color32::BLACK));
                }
            }
        }
    }
}

/// Applies `f` to the CFrame of every original (parts and model pivots) without touching undo.
fn apply_originals(doc: &mut Document, originals: &Originals, f: impl Fn(CFrame) -> CFrame) {
    let mut changed = false;
    for (r, key, old) in originals {
        if let Variant::CFrame(cf) = old {
            changed |= set_live(doc, *r, key, Variant::CFrame(f(*cf)));
        }
    }
    if changed {
        doc.touch();
    }
}

/// Sets a property on an instance during a live drag. Returns whether it changed.
fn set_live(doc: &mut Document, r: Ref, key: &str, value: Variant) -> bool {
    let Some(inst) = doc.dom.get_by_ref_mut(r) else { return false };
    let key = Ustr::from(key);
    if inst.properties.get(&key) == Some(&value) {
        return false;
    }
    inst.properties.insert(key, value);
    true
}

/// Records a finished drag as one undo step, from the originals to the current values.
fn record_live(doc: &mut Document, label: &str, originals: &Originals) {
    let mut edits = Vec::new();
    for (r, key, old) in originals {
        let now = doc.dom.get_by_ref(*r).and_then(|i| i.properties.get(&Ustr::from(key))).cloned();
        if now.as_ref() != Some(old) {
            edits.push(Edit::SetProperty { target: *r, key: (*key).into(), old: Some(old.clone()), new: now });
        }
    }
    doc.record(label, edits);
}

/// The CFrames a transform drag moves: selected parts and model pivots.
fn capture(doc: &Document) -> Originals {
    let parts = ops::parts_to_transform(doc)
        .into_iter()
        .filter_map(|r| ops::cframe_of(&doc.dom, r, "CFrame").map(|cf| (r, "CFrame", Variant::CFrame(cf))));
    let pivots = ops::models_to_transform(doc)
        .into_iter()
        .filter_map(|r| ops::cframe_of(&doc.dom, r, "WorldPivot").map(|cf| (r, "WorldPivot", Variant::CFrame(cf))));
    parts.chain(pivots).collect()
}

/// The part Scale works on: exactly one BasePart selected.
fn single_part(doc: &Document) -> Option<Ref> {
    let [r] = doc.selection.as_slice() else { return None };
    let inst = doc.dom.get_by_ref(*r)?;
    reflect::is_base_part(&inst.class).then_some(*r)
}

/// Axes for Move and Rotate: the world's, or the first selected part's when `local`.
fn frame_axes(doc: &Document, local: bool) -> [Vec3; 3] {
    if local
        && let Some(&r) = ops::parts_to_transform(doc).first()
        && let Some(cf) = ops::cframe_of(&doc.dom, r, "CFrame")
    {
        let m = cframe_to_mat4(&cf);
        return [m.x_axis.truncate(), m.y_axis.truncate(), m.z_axis.truncate()];
    }
    [Vec3::X, Vec3::Y, Vec3::Z]
}

/// Splits a scene into the parts `mine` accepts and the rest.
fn split_scene(scene: &Scene, mut mine: impl FnMut(Ref) -> bool) -> (Scene, Scene) {
    let (a, b): (Vec<_>, Vec<_>) = scene.parts.iter().cloned().partition(|p| mine(p.owner));
    (Scene { parts: a }, Scene { parts: b })
}

fn translate(mut cf: CFrame, offset: Vec3) -> CFrame {
    cf.position = to_vector3(vec3(cf.position) + offset);
    cf
}

fn vec3(v: Vector3) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

fn to_vector3(v: Vec3) -> Vector3 {
    Vector3::new(v.x, v.y, v.z)
}

/// Screen-space move arrows: (handle, base, tip), both directions of every axis.
fn arrows(cam: &Camera, rect: Rect, aspect: f32, g: &Gizmo) -> Vec<(Handle, Pos2, Pos2)> {
    let mut out = Vec::new();
    for (i, axis) in g.axes.iter().enumerate() {
        for sign in [1.0f32, -1.0] {
            let dir = *axis * sign;
            let base = g.extent[i] + g.length * 0.15;
            let tip = base + g.length * 0.85;
            if let (Some(a), Some(b)) =
                (screen(cam, rect, aspect, g.center + dir * base), screen(cam, rect, aspect, g.center + dir * tip))
            {
                out.push((Handle::Arrow(i), a, b));
            }
        }
    }
    out
}

/// Screen-space scale handles on the six faces of the part.
fn faces(cam: &Camera, rect: Rect, aspect: f32, g: &Gizmo) -> Vec<(Handle, Pos2)> {
    let mut out = Vec::new();
    for (i, axis) in g.axes.iter().enumerate() {
        for positive in [true, false] {
            let sign = if positive { 1.0 } else { -1.0 };
            if let Some(p) = screen(cam, rect, aspect, g.center + *axis * sign * g.extent[i]) {
                out.push((Handle::Face(i, positive), p));
            }
        }
    }
    out
}

/// The visible pieces of the rotation ring around gizmo axis `i`, as screen polylines.
fn ring_runs(cam: &Camera, rect: Rect, aspect: f32, g: &Gizmo, i: usize) -> Vec<Vec<Pos2>> {
    let n = g.axes[i];
    let u = n.cross(if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y }).normalize();
    let v = n.cross(u);
    let mut runs = Vec::new();
    let mut run = Vec::new();
    for k in 0..=RING_SEGMENTS {
        let a = k as f32 / RING_SEGMENTS as f32 * TAU;
        let point = g.center + (u * a.cos() + v * a.sin()) * g.length;
        match screen(cam, rect, aspect, point) {
            Some(p) => run.push(p),
            None => {
                if run.len() > 1 {
                    runs.push(std::mem::take(&mut run));
                }
                run.clear();
            }
        }
    }
    if run.len() > 1 {
        runs.push(run);
    }
    runs
}

fn ring_distance(cam: &Camera, rect: Rect, aspect: f32, g: &Gizmo, i: usize, p: Pos2) -> f32 {
    ring_runs(cam, rect, aspect, g, i)
        .iter()
        .flat_map(|run| run.windows(2).map(|w| distance_to_segment(p, w[0], w[1])))
        .fold(f32::INFINITY, f32::min)
}

fn screen(cam: &Camera, rect: Rect, aspect: f32, p: Vec3) -> Option<Pos2> {
    cam.project(p, aspect).map(|ndc| from_ndc(rect, ndc))
}

fn paint_box(painter: &egui::Painter, start: Pos2, current: Pos2) {
    let r = Rect::from_two_pos(start, current);
    painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(90, 160, 255, 40));
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(120, 180, 255)), StrokeKind::Inside);
}

fn inside_box(a: Pos2, b: Pos2, p: Pos2) -> bool {
    Rect::from_two_pos(a, b).contains(p)
}

fn add_bounds(
    overlay: &mut Overlay,
    doc: &Document,
    scene: &Scene,
    index: &HashMap<Ref, usize>,
    r: Ref,
    color: [f32; 4],
) {
    if let Some(&i) = index.get(&r) {
        overlay.box_edges(&scene.parts[i].corners(), color);
        return;
    }
    let corners = doc
        .dom
        .descendants_of(r)
        .filter_map(|inst| index.get(&inst.referent()))
        .flat_map(|&i| scene.parts[i].corners());
    if let Some((min, max)) = kahua_render::scene::bounds_of(corners) {
        let pad = Vec3::splat(0.05);
        let (min, max) = (min - pad, max + pad);
        let mut corners = [Vec3::ZERO; 8];
        for (i, c) in corners.iter_mut().enumerate() {
            *c = Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
        }
        overlay.box_edges(&corners, color);
    }
}

fn to_ndc(rect: Rect, p: Pos2) -> Vec2 {
    Vec2::new((p.x - rect.left()) / rect.width() * 2.0 - 1.0, 1.0 - (p.y - rect.top()) / rect.height() * 2.0)
}

fn from_ndc(rect: Rect, ndc: Vec2) -> Pos2 {
    Pos2::new(
        rect.left() + (ndc.x * 0.5 + 0.5) * rect.width(),
        rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * rect.height(),
    )
}

fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

/// Parameter along the axis line (from `point`) closest to the ray.
fn closest_on_axis(point: Vec3, axis: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    let r = point - origin;
    let b = axis.dot(dir);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-4 {
        return None;
    }
    Some((b * r.dot(dir) - r.dot(axis)) / denom)
}

/// Where the ray meets the plane through `point` with unit `normal`, if it is ahead of the origin.
fn plane_hit(origin: Vec3, dir: Vec3, point: Vec3, normal: Vec3) -> Option<Vec3> {
    let denom = dir.dot(normal);
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (point - origin).dot(normal) / denom;
    (t > 0.0).then(|| origin + dir * t)
}

/// Signed angle about the unit `normal` that turns `from` onto `to`, both projected onto the plane.
fn signed_angle(from: Vec3, to: Vec3, normal: Vec3) -> f32 {
    let a = from - normal * from.dot(normal);
    let b = to - normal * to.dot(normal);
    normal.dot(a.cross(b)).atan2(a.dot(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn signed_angle_follows_the_right_hand_rule() {
        // Turning +X by a quarter turn about +Y gives -Z, so the angle from +X to -Z is +90 degrees.
        let angle = signed_angle(Vec3::X, Vec3::NEG_Z, Vec3::Y);
        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        let back = signed_angle(Vec3::X, Vec3::Z, Vec3::Y);
        assert!((back + std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        // Components along the normal do not matter.
        let lifted = signed_angle(Vec3::X + Vec3::Y * 3.0, Vec3::NEG_Z + Vec3::Y, Vec3::Y);
        assert!((lifted - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn ray_plane_hits_ahead_only() {
        let hit = plane_hit(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, Vec3::ZERO, Vec3::Y).unwrap();
        assert!(hit.abs_diff_eq(Vec3::ZERO, 1e-6));
        // Looking away from the plane, or parallel to it, is no hit.
        assert!(plane_hit(Vec3::new(0.0, 5.0, 0.0), Vec3::Y, Vec3::ZERO, Vec3::Y).is_none());
        assert!(plane_hit(Vec3::new(0.0, 5.0, 0.0), Vec3::X, Vec3::ZERO, Vec3::Y).is_none());
    }

    #[test]
    fn box_contains_points_inside_either_corner_order() {
        let (a, b) = (pos2(0.0, 0.0), pos2(10.0, 10.0));
        assert!(inside_box(a, b, pos2(5.0, 5.0)));
        assert!(inside_box(b, a, pos2(5.0, 5.0)));
        assert!(!inside_box(a, b, pos2(11.0, 5.0)));
        assert!(!inside_box(b, a, pos2(-1.0, 5.0)));
    }

    #[test]
    fn ndc_round_trip() {
        let rect = Rect::from_min_size(pos2(100.0, 50.0), egui::vec2(200.0, 100.0));
        let p = pos2(150.0, 80.0);
        let back = from_ndc(rect, to_ndc(rect, p));
        assert!(back.distance(p) < 1e-3);
    }
}
