//! Texture dictionary (`.ytd`) viewer for the preview panel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use paleto_formats::{Format, Texture, TextureDictionary};

/// What the user asked to export.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Export {
    Selected { png: bool },
    All { png: bool },
}

/// A change to the previewed texture dictionary.
#[derive(Clone, Debug, PartialEq)]
pub enum TextureEdit {
    /// A new image from a DDS or PNG file. PNGs are compressed to `format`,
    /// or else to the texture's current format when possible.
    Replace { name: String, file: PathBuf, format: Option<Format> },
    /// A texture from a DDS or PNG file. PNGs are compressed to `format`, or
    /// else to BC1 if opaque and BC3 if not.
    Add { name: String, file: PathBuf, format: Option<Format> },
    Rename { from: String, to: String },
    Remove(String),
}

/// A request from the viewer to the application.
pub enum Request {
    Export(Export),
    /// Replace this texture with a DDS or PNG file.
    Replace(String),
    /// Add a texture from a DDS or PNG file.
    Add,
    /// Replace textures from several files (or a folder's), matched by name.
    ReplaceMany { folder: bool },
    Rename(String),
    Remove(String),
}

pub struct TextureView {
    pub dict: Arc<TextureDictionary>,
    pub selected: usize,
    mip: u32,
    /// Show the alpha channel (over a checkerboard) instead of opaque colours.
    alpha: bool,
    /// The uploaded image and the (texture, mip, alpha) it shows.
    shown: Option<((usize, u32, bool), egui::TextureHandle)>,
    /// Scroll the list to this index on the next frame (set by keyboard nav).
    scroll_to: Option<usize>,
}

impl TextureView {
    pub fn new(dict: TextureDictionary) -> Self {
        TextureView { dict: Arc::new(dict), selected: 0, mip: 0, alpha: true, shown: None, scroll_to: None }
    }

    pub fn select(&mut self, index: usize) {
        if index != self.selected {
            self.selected = index;
            self.mip = 0;
        }
    }

    /// Summary for the preview header.
    pub fn summary(&self) -> String {
        let edition = if self.dict.enhanced { "Enhanced" } else { "Legacy" };
        let shown = match self.dict.textures.len() {
            1 => format!("1 texture, {edition} format"),
            n => format!("{n} textures, {edition} format"),
        };
        match self.dict.unsupported {
            0 => shown,
            n => format!("{shown}; {n} skipped (format not supported)"),
        }
    }

    /// `editable`: the dictionary is inside an archive, so textures can be changed.
    pub fn ui(&mut self, ui: &mut egui::Ui, busy: bool, editable: bool) -> Option<Request> {
        let mut request = None;
        // Textures that cannot be read cannot be written back either.
        let skipped = self.dict.unsupported > 0;
        let hint = match (editable, skipped) {
            (_, true) => "It holds textures in a format not supported, so it cannot be changed",
            (true, false) => "Change the textures (in the mods folder copy)",
            (false, false) => "Only dictionaries inside archives can be changed",
        };
        let editable = editable && !skipped;
        if self.dict.textures.is_empty() {
            ui.label(if skipped { "This texture dictionary holds only textures in a format not supported." } else { "This texture dictionary is empty." });
            let add = ui.add_enabled(editable && !busy, egui::Button::new("➕ Add texture…"));
            if add.on_disabled_hover_text(hint).clicked() {
                request = Some(Request::Add);
            }
            return request;
        }
        let tex = &self.dict.textures[self.selected];
        let levels = tex.levels;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.menu_button("⬇ Export", |ui| {
                    for (label, choice) in [
                        ("Selected texture as DDS…", Export::Selected { png: false }),
                        ("Selected texture as PNG…", Export::Selected { png: true }),
                        ("All textures as DDS…", Export::All { png: false }),
                        ("All textures as PNG…", Export::All { png: true }),
                    ] {
                        if ui.button(label).clicked() {
                            request = Some(Request::Export(choice));
                        }
                    }
                });
                ui.add_enabled_ui(editable, |ui| {
                    ui.menu_button("✏ Edit", |ui| {
                        let items = [
                            ("🔄 Replace with DDS or PNG…", Request::Replace(tex.name.clone())),
                            ("🔄 Replace from several files…", Request::ReplaceMany { folder: false }),
                            ("🔄 Replace from a folder…", Request::ReplaceMany { folder: true }),
                            ("➕ Add texture…", Request::Add),
                            ("Rename…", Request::Rename(tex.name.clone())),
                            ("🗑 Remove", Request::Remove(tex.name.clone())),
                        ];
                        for (label, choice) in items {
                            if ui.button(label).clicked() {
                                request = Some(choice);
                            }
                        }
                    })
                    .response
                    .on_hover_text(hint)
                    .on_disabled_hover_text(hint);
                });
            });
            ui.checkbox(&mut self.alpha, "Alpha").on_hover_text("Show transparency over a checkerboard");
            if levels > 1 {
                let dims = |m: u32| format!("Mip {m}: {}×{}", (tex.width >> m).max(1), (tex.height >> m).max(1));
                crate::widgets::fit_combo(ui, &dims(self.mip));
                egui::ComboBox::from_id_salt("mip").selected_text(dims(self.mip)).show_ui(ui, |ui| {
                    for m in 0..levels {
                        ui.selectable_value(&mut self.mip, m, dims(m));
                    }
                });
            }
        });

        // Keyboard navigation: ↑/↓ move selection, Home/End jump to first/last.
        let count = self.dict.textures.len();
        if !ui.ctx().egui_wants_keyboard_input() {
            let (down, up, home, end) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Home),
                    i.key_pressed(egui::Key::End),
                )
            });
            let new = if down {
                Some((self.selected + 1).min(count - 1))
            } else if up {
                Some(self.selected.saturating_sub(1))
            } else if home {
                Some(0)
            } else if end {
                Some(count - 1)
            } else {
                None
            };
            if let Some(i) = new
                && i != self.selected
            {
                self.select(i);
                self.scroll_to = Some(i);
            }
        }

        // Texture list.
        let mut clicked = None;
        let list_height = (ui.available_height() * 0.35).max(80.0);
        let scroll_to = self.scroll_to.take();
        egui::ScrollArea::vertical().id_salt("texture-list").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
            for (i, t) in self.dict.textures.iter().enumerate() {
                let details = format!("{}×{} · {}", t.width, t.height, t.format.name());
                let response = crate::widgets::list_row(ui, i == self.selected, &t.name, &details);
                if response.clicked() {
                    clicked = Some(i);
                }
                if scroll_to == Some(i) {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
        if let Some(i) = clicked {
            self.select(i);
        }
        ui.separator();

        let Some(handle) = self.texture(ui.ctx()) else {
            ui.label(RichText::new("This texture cannot be shown.").weak());
            return request;
        };
        let size = handle.size_vec2();
        let area = ui.available_size();
        let scale = (area.x / size.x).min(area.y / size.y).min(8.0);
        let (rect, _) = ui.allocate_exact_size(area, egui::Sense::hover());
        let image = egui::Rect::from_center_size(rect.center(), size * scale);
        let painter = ui.painter_at(rect);
        if self.alpha {
            checkerboard(&painter, image, ui.visuals().dark_mode);
        }
        painter.image(handle.id(), image, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        let zoom = format!("{}×{} · {:.0}%", size.x, size.y, scale * 100.0);
        painter.text(rect.left_bottom(), egui::Align2::LEFT_BOTTOM, zoom, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
        request
    }

    /// The selected texture, decoded and uploaded on first use.
    fn texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let key = (self.selected, self.mip, self.alpha);
        if self.shown.as_ref().map(|s| s.0) != Some(key) {
            let tex = &self.dict.textures[self.selected];
            let (w, h, mut rgba) = tex.rgba(self.mip)?;
            if !self.alpha {
                rgba.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            // Sharp pixels when zoomed in, smooth when shrunk.
            let options = egui::TextureOptions { magnification: egui::TextureFilter::Nearest, ..egui::TextureOptions::LINEAR };
            self.shown = Some((key, ctx.load_texture(format!("ytd/{}", tex.name), image, options)));
        }
        self.shown.as_ref().map(|s| s.1.clone())
    }
}

fn checkerboard(painter: &egui::Painter, rect: egui::Rect, dark: bool) {
    let (a, b) = if dark { (Color32::from_gray(0x3A), Color32::from_gray(0x4A)) } else { (Color32::from_gray(0xD8), Color32::WHITE) };
    painter.rect_filled(rect, 0.0, a);
    const CELL: f32 = 8.0;
    let (cols, rows) = ((rect.width() / CELL).ceil() as usize, (rect.height() / CELL).ceil() as usize);
    for row in 0..rows {
        for col in (row % 2..cols).step_by(2) {
            let min = rect.min + egui::vec2(col as f32 * CELL, row as f32 * CELL);
            let cell = egui::Rect::from_min_size(min, egui::vec2(CELL, CELL)).intersect(rect);
            painter.rect_filled(cell, 0.0, b);
        }
    }
}

/// Writes a texture as DDS (all mip levels) or PNG (largest level).
pub fn write(tex: &Texture, png: bool, path: &Path) -> Result<(), String> {
    let data = if png { tex.to_png().map_err(|e| e.to_string())? } else { tex.to_dds() };
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_unsupported_textures() {
        let view = |unsupported| TextureView::new(TextureDictionary { enhanced: false, textures: Vec::new(), unsupported });
        assert_eq!(view(0).summary(), "0 textures, Legacy format");
        assert_eq!(view(2).summary(), "0 textures, Legacy format; 2 skipped (format not supported)");
    }
}
