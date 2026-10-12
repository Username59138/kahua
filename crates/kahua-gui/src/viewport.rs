//! The 3D viewport: camera controls, selection by clicking, and the move gizmo.

use std::collections::HashMap;

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Sense, Ui};
use eframe::egui_wgpu::wgpu;
use kahua_core::Document;
use kahua_core::kahua_io::rbx_dom_weak::types::{CFrame, Ref, Variant, Vector3};
use kahua_render::glam::{Vec2, Vec3};
use kahua_render::{Camera, Overlay, Renderer, Scene};

use crate::ops;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Move,
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

struct MoveDrag {
    axis: Vec3,
    /// Gizmo center when the drag started; offsets are measured from here.
    center: Vec3,
    start: f32,
    parts: Vec<(Ref, CFrame)>,
    pivots: Vec<(Ref, CFrame)>,
}

#[derive(Default)]
pub struct Viewport {
    pub camera: Camera,
    renderer: Option<Renderer>,
    texture: Option<egui::TextureId>,
    framed: bool,
    drag: Option<MoveDrag>,
    hovered_axis: Option<usize>,
    /// Tracks the scene uploaded to the renderer.
    uploaded: bool,
}

const AXES: [(Vec3, [f32; 4]); 3] =
    [(Vec3::X, [0.95, 0.25, 0.25, 1.0]), (Vec3::Y, [0.3, 0.9, 0.3, 1.0]), (Vec3::Z, [0.3, 0.5, 1.0, 1.0])];

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

        // Gizmo hover and drag.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let bounds = ops::selection_bounds(cx.doc, cx.scene, cx.index);
        let gizmo = (cx.tool == Tool::Move).then_some(bounds).flatten().map(|(min, max)| {
            let center = (min + max) * 0.5;
            let length = (self.camera.position.distance(center) * 0.12).max(1.0);
            Gizmo { center, half: (max - min) * 0.5, length }
        });
        if self.drag.is_none() {
            self.hovered_axis = None;
            if let (Some(g), Some(p)) = (&gizmo, pointer)
                && rect.contains(p)
            {
                self.hovered_axis = self.hit_axis(rect, g, p);
            }
        }

        if response.drag_started_by(PointerButton::Primary)
            && let (Some(axis), Some(g), Some(p)) = (self.hovered_axis, &gizmo, pointer)
        {
            let center = g.center;
            let axis_dir = AXES[axis].0;
            let (origin, dir) = self.camera.ray(to_ndc(rect, p), aspect);
            if let Some(start) = closest_on_axis(center, axis_dir, origin, dir) {
                let parts = ops::parts_to_transform(cx.doc)
                    .into_iter()
                    .filter_map(|r| ops::cframe_of(&cx.doc.dom, r, "CFrame").map(|cf| (r, cf)))
                    .collect();
                let pivots = ops::models_to_transform(cx.doc)
                    .into_iter()
                    .filter_map(|r| ops::cframe_of(&cx.doc.dom, r, "WorldPivot").map(|cf| (r, cf)))
                    .collect();
                self.drag = Some(MoveDrag { axis: axis_dir, center, start, parts, pivots });
            }
        }
        if let (Some(drag), Some(p)) = (&self.drag, pointer)
            && response.dragged_by(PointerButton::Primary)
        {
            let (origin, dir) = self.camera.ray(to_ndc(rect, p), aspect);
            if let Some(s) = closest_on_axis(drag.center, drag.axis, origin, dir) {
                let mut delta = s - drag.start;
                if let Some(step) = cx.snap {
                    delta = (delta / step).round() * step;
                }
                apply_offset(cx.doc, drag, delta);
            }
        }
        if response.drag_stopped_by(PointerButton::Primary)
            && let Some(drag) = self.drag.take()
        {
            let mut edits = Vec::new();
            for (r, old) in drag.parts.iter().chain(drag.pivots.iter()) {
                let key = if drag.pivots.iter().any(|(p, _)| p == r) { "WorldPivot" } else { "CFrame" };
                if let Some(now) = ops::cframe_of(&cx.doc.dom, *r, key)
                    && now != *old
                {
                    edits.push(kahua_core::Edit::SetProperty {
                        target: *r,
                        key: key.into(),
                        old: Some(Variant::CFrame(*old)),
                        new: Some(Variant::CFrame(now)),
                    });
                }
            }
            cx.doc.record("Move", edits);
        }

        // Click to select.
        let hovered_part =
            pointer.filter(|p| rect.contains(*p) && self.drag.is_none() && self.hovered_axis.is_none()).and_then(|p| {
                let (origin, dir) = self.camera.ray(to_ndc(rect, p), aspect);
                cx.scene.pick(origin, dir).map(|(i, _)| cx.scene.parts[i].owner)
            });
        let alt = ui.input(|i| i.modifiers.alt);
        let hovered_target = hovered_part.map(|r| ops::click_target(&cx.doc.dom, r, alt));
        if response.clicked_by(PointerButton::Primary) {
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

        // Overlay: selection boxes, hover box, gizmo.
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

        if let Some(g) = &gizmo {
            self.paint_gizmo(ui, rect, g);
        }

        // Hints.
        let hint = match cx.tool {
            Tool::Select => "Select · RMB look · WASD/QE fly · Wheel zoom · F focus",
            Tool::Move => "Move · drag the arrows · RMB look · WASD/QE fly · F focus",
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

    /// Screen-space segments of the six handles: (axis index, base, tip).
    fn handles(&self, rect: Rect, g: &Gizmo) -> Vec<(usize, Pos2, Pos2)> {
        let aspect = rect.width() / rect.height();
        let mut out = Vec::new();
        for (i, (axis, _)) in AXES.iter().enumerate() {
            let extent = g.half.dot(*axis).abs() + g.length * 0.15;
            for sign in [1.0f32, -1.0] {
                let dir = *axis * sign;
                let base = self.camera.project(g.center + dir * extent, aspect);
                let tip = self.camera.project(g.center + dir * (extent + g.length), aspect);
                if let (Some(base), Some(tip)) = (base, tip) {
                    out.push((i, from_ndc(rect, base), from_ndc(rect, tip)));
                }
            }
        }
        out
    }

    fn hit_axis(&self, rect: Rect, g: &Gizmo, p: Pos2) -> Option<usize> {
        let mut best = None;
        for (i, a, b) in self.handles(rect, g) {
            let d = distance_to_segment(p, a, b);
            if d < 10.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    fn paint_gizmo(&self, ui: &Ui, rect: Rect, g: &Gizmo) {
        let painter = ui.painter_at(rect);
        let active_axis = self.drag.as_ref().and_then(|d| AXES.iter().position(|(a, _)| *a == d.axis));
        for (i, base, tip) in self.handles(rect, g) {
            let [r, gr, b, _] = AXES[i].1;
            let mut color = Color32::from_rgb((r * 255.0) as u8, (gr * 255.0) as u8, (b * 255.0) as u8);
            if self.hovered_axis == Some(i) || active_axis == Some(i) {
                color = Color32::from_rgb(255, 225, 60);
            }
            let dir = (tip - base).normalized();
            if !dir.x.is_finite() {
                continue;
            }
            let normal = egui::vec2(-dir.y, dir.x);
            let head = 13.0;
            let neck = tip - dir * head;
            painter.line_segment([base, neck], egui::Stroke::new(3.5, color));
            painter.add(egui::Shape::convex_polygon(
                vec![tip, neck + normal * 6.5, neck - normal * 6.5],
                color,
                egui::Stroke::NONE,
            ));
        }
    }
}

fn apply_offset(doc: &mut Document, drag: &MoveDrag, delta: f32) {
    let offset = drag.axis * delta;
    let shift = |cf: &CFrame| {
        let mut cf = *cf;
        cf.position = Vector3::new(cf.position.x + offset.x, cf.position.y + offset.y, cf.position.z + offset.z);
        cf
    };
    let mut changed = false;
    for (r, old) in &drag.parts {
        if let Some(inst) = doc.dom.get_by_ref_mut(*r) {
            let new = Variant::CFrame(shift(old));
            if inst.properties.get(&"CFrame".into()) != Some(&new) {
                inst.properties.insert("CFrame".into(), new);
                changed = true;
            }
        }
    }
    for (r, old) in &drag.pivots {
        if let Some(inst) = doc.dom.get_by_ref_mut(*r) {
            inst.properties.insert("WorldPivot".into(), Variant::CFrame(shift(old)));
        }
    }
    if changed {
        doc.touch();
    }
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

struct Gizmo {
    center: Vec3,
    half: Vec3,
    length: f32,
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
