//! Model (`.ydr`, `.yft`, `.ydd`) preview: a small software renderer with
//! flat shading, drawn into a texture. Drag to turn the model, scroll to zoom.

use eframe::egui::{self, Color32, RichText};
use paleto_formats::drawable::{Drawable, Mesh, Picture};

pub struct ModelView {
    drawables: Vec<Drawable>,
    selected: usize,
    lod: usize,
    yaw: f32,
    pitch: f32,
    zoom: f32,
    /// Show diffuse textures (when the model has any).
    textured: bool,
    /// The rendered image and what it shows.
    shown: Option<(Key, egui::TextureHandle)>,
}

#[derive(PartialEq, Clone, Copy)]
struct Key {
    selected: usize,
    lod: usize,
    camera: [u32; 3],
    size: [usize; 2],
    dark: bool,
    textured: bool,
}

const START_YAW: f32 = -0.8;
const START_PITCH: f32 = 0.35;

impl ModelView {
    pub fn new(drawables: Vec<Drawable>) -> Self {
        ModelView { drawables, selected: 0, lod: 0, yaw: START_YAW, pitch: START_PITCH, zoom: 1.0, textured: true, shown: None }
    }

    pub fn summary(&self) -> String {
        let n = self.drawables.len();
        let mesh = self.mesh();
        let tris = mesh.map_or(0, |m| m.indices.len() / 3);
        if n > 1 { format!("{n} models · {tris} triangles") } else { format!("{tris} triangles") }
    }

    fn mesh(&self) -> Option<&Mesh> {
        self.drawables.get(self.selected)?.lods.get(self.lod).map(|(_, m)| m)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if self.drawables.iter().all(|d| d.lods.is_empty()) {
            ui.label("This model has no geometry to show.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if self.drawables.len() > 1 {
                let label = |d: &Drawable, i: usize| if d.name.is_empty() { format!("Model {}", i + 1) } else { d.name.clone() };
                let current = label(&self.drawables[self.selected], self.selected);
                crate::widgets::fit_combo(ui, &current);
                egui::ComboBox::from_id_salt("model").selected_text(current).show_ui(ui, |ui| {
                    for (i, d) in self.drawables.iter().enumerate() {
                        if ui.selectable_label(i == self.selected, label(d, i)).clicked() {
                            self.selected = i;
                            self.lod = 0;
                        }
                    }
                });
            }
            let lods = &self.drawables[self.selected].lods;
            if !lods.is_empty() {
                self.lod = self.lod.min(lods.len() - 1);
                crate::widgets::fit_combo(ui, lods[self.lod].0);
                egui::ComboBox::from_id_salt("lod").selected_text(lods[self.lod].0).show_ui(ui, |ui| {
                    for (i, (name, _)) in lods.iter().enumerate() {
                        ui.selectable_value(&mut self.lod, i, *name);
                    }
                });
            }
            if ui.button("Reset view").clicked() {
                (self.yaw, self.pitch, self.zoom) = (START_YAW, START_PITCH, 1.0);
            }
            if !self.drawables[self.selected].textures.is_empty() {
                ui.checkbox(&mut self.textured, "Textures");
            }
        });
        let Some(mesh) = self.mesh() else {
            ui.label("This model has no geometry at this level of detail.");
            return;
        };
        ui.label(RichText::new(format!("{} vertices · {} triangles · drag to turn, scroll to zoom", mesh.positions.len(), mesh.indices.len() / 3)).weak());

        let area = ui.available_size().max(egui::vec2(64.0, 64.0));
        let (rect, response) = ui.allocate_exact_size(area, egui::Sense::click_and_drag());
        if response.dragged() {
            let d = response.drag_delta();
            self.yaw -= d.x * 0.01;
            self.pitch = (self.pitch + d.y * 0.01).clamp(-1.5, 1.5);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.2, 20.0);
            }
        }
        let ppp = ui.ctx().pixels_per_point();
        let size = [((rect.width() * ppp) as usize).clamp(16, 1600), ((rect.height() * ppp) as usize).clamp(16, 1200)];
        let key = Key {
            selected: self.selected,
            lod: self.lod,
            camera: [self.yaw.to_bits(), self.pitch.to_bits(), self.zoom.to_bits()],
            size,
            dark: ui.visuals().dark_mode,
            textured: self.textured,
        };
        if self.shown.as_ref().map(|s| s.0) != Some(key) {
            let textures: &[Picture] = if self.textured { &self.drawables[self.selected].textures } else { &[] };
            let Some(mesh) = self.mesh() else { return };
            let image = render(mesh, textures, size, self.yaw, self.pitch, self.zoom, key.dark);
            let handle = ui.ctx().load_texture("model-preview", image, egui::TextureOptions::LINEAR);
            self.shown = Some((key, handle));
        }
        if let Some((_, handle)) = &self.shown {
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            ui.painter_at(rect).image(handle.id(), rect, uv, Color32::WHITE);
        }
    }
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let len = dot(a, a).sqrt();
    if len > 1e-12 { [a[0] / len, a[1] / len, a[2] / len] } else { [0.0, 0.0, 0.0] }
}

/// Renders a mesh seen from an orbiting camera (the game's Z axis is up),
/// textured with `textures` when its parts have one.
fn render(mesh: &Mesh, textures: &[Picture], [w, h]: [usize; 2], yaw: f32, pitch: f32, zoom: f32, dark: bool) -> egui::ColorImage {
    let background = if dark { [0x26, 0x27, 0x2B] } else { [0xD6, 0xD8, 0xDE] };
    let mut color = vec![0u8; w * h * 4];
    for px in color.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[background[0], background[1], background[2], 255]);
    }
    let image = |color: Vec<u8>| egui::ColorImage::from_rgba_unmultiplied([w, h], &color);
    if mesh.positions.is_empty() {
        return image(color);
    }

    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for c in 0..3 {
            lo[c] = lo[c].min(p[c]);
            hi[c] = hi[c].max(p[c]);
        }
    }
    let center = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let radius = (dot(sub(hi, lo), sub(hi, lo)).sqrt() / 2.0).max(1e-3);
    let fov = 40f32.to_radians();
    let distance = radius / (fov / 2.0).sin() / zoom;
    let dir = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
    let eye = [center[0] + dir[0] * distance, center[1] + dir[1] * distance, center[2] + dir[2] * distance];
    let forward = normalize(sub(center, eye));
    let right = normalize(cross(forward, [0.0, 0.0, 1.0]));
    let up = cross(right, forward);
    // The field of view spans the shorter side, so the whole model fits.
    let focal = w.min(h) as f32 / 2.0 / (fov / 2.0).tan();
    let near = radius * 0.01;

    // Screen x, y and depth of every vertex (depth <= near: behind the camera).
    let projected: Vec<V3> = mesh
        .positions
        .iter()
        .map(|&p| {
            let v = sub(p, eye);
            let z = dot(v, forward);
            [w as f32 / 2.0 + dot(v, right) * focal / z, h as f32 / 2.0 - dot(v, up) * focal / z, z]
        })
        .collect();
    let base = if dark { [0.78, 0.80, 0.85] } else { [0.62, 0.65, 0.72] };
    let mut depth = vec![f32::MAX; w * h];
    let whole = [paleto_formats::drawable::Part { start: 0, count: mesh.indices.len(), texture: None }];
    let parts = if mesh.parts.is_empty() { &whole[..] } else { &mesh.parts[..] };
    for part in parts {
        let texture = part.texture.and_then(|t| textures.get(t)).filter(|t| t.width > 0 && t.height > 0);
        for tri in mesh.indices[part.start..part.start + part.count].as_chunks::<3>().0 {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| projected[i as usize]);
            if a[2] <= near || b[2] <= near || c[2] <= near {
                continue;
            }
            let [pa, pb, pc] = [tri[0], tri[1], tri[2]].map(|i| mesh.positions[i as usize]);
            // Lit from the camera, both sides alike.
            let normal = normalize(cross(sub(pb, pa), sub(pc, pa)));
            let light = 0.25 + 0.75 * dot(normal, forward).abs();
            let shade = base.map(|b: f32| (b * light * 255.0) as u8);
            let uv = [tri[0], tri[1], tri[2]].map(|i| mesh.uvs.get(i as usize).copied().unwrap_or_default());
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area.abs() < 1e-6 {
                continue;
            }
            let x0 = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
            let x1 = (a[0].max(b[0]).max(c[0]).ceil() as usize).min(w);
            let y0 = a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize;
            let y1 = (a[1].max(b[1]).max(c[1]).ceil() as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let wa = ((b[0] - px) * (c[1] - py) - (b[1] - py) * (c[0] - px)) / area;
                    let wb = ((c[0] - px) * (a[1] - py) - (c[1] - py) * (a[0] - px)) / area;
                    let wc = 1.0 - wa - wb;
                    if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                        continue;
                    }
                    let z = wa * a[2] + wb * b[2] + wc * c[2];
                    let i = y * w + x;
                    if z >= depth[i] {
                        continue;
                    }
                    let rgb = match texture {
                        Some(t) => {
                            // Perspective-correct texture coordinates, repeating.
                            let (ia, ib, ic) = (wa / a[2], wb / b[2], wc / c[2]);
                            let sum = ia + ib + ic;
                            let u = (ia * uv[0][0] + ib * uv[1][0] + ic * uv[2][0]) / sum;
                            let v = (ia * uv[0][1] + ib * uv[1][1] + ic * uv[2][1]) / sum;
                            let tx = ((u.rem_euclid(1.0) * t.width as f32) as u32).min(t.width - 1);
                            let ty = ((v.rem_euclid(1.0) * t.height as f32) as u32).min(t.height - 1);
                            let at = ((ty * t.width + tx) * 4) as usize;
                            // Cut-out transparency (leaves, fences, decals).
                            if t.rgba[at + 3] < 64 {
                                continue;
                            }
                            [0, 1, 2].map(|k| (t.rgba[at + k] as f32 * light) as u8)
                        }
                        None => shade,
                    };
                    depth[i] = z;
                    color[i * 4..i * 4 + 3].copy_from_slice(&rgb);
                }
            }
        }
    }
    image(color)
}
