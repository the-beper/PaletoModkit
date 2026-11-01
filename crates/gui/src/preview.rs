//! The preview panel: reading and parsing the previewed file, its views (text, textures,
//! model, audio, hex), and the panel's UI.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use paleto_core::shared_textures::SharedTextures;
use paleto_core::ArchivePath;
use paleto_formats::{MetaFile, TextureDictionary};
use paleto_rpf::Keys;

use crate::app::{Action, App, Info, Location, MAX_TEXT, Status};
use crate::menus::add_info;
use crate::audio::{AudioRequest, AudioView};
use crate::export::is_ytd;
use crate::find::{Codec, Find};
use crate::hex_view::{self, HexView};
use crate::highlight::{self, Syntax};
use crate::model::ModelView;
use crate::theme;
use crate::task::Cancel;
use crate::textures::{Request, TextureEdit, TextureView};

pub(crate) enum PreviewSource {
    Disk(PathBuf),
    Archive(ArchivePath),
}

pub(crate) struct Preview {
    pub(crate) source: PreviewSource,
    pub(crate) name: String,
    syntax: Syntax,
    info: String,
    /// Text content (edited in place) and the saved version.
    pub(crate) text: Option<(String, String)>,
    /// How the text is turned back into the file.
    pub(crate) codec: Codec,
    pub(crate) textures: Option<TextureView>,
    model: Option<ModelView>,
    /// What exporting the model needs, for models that can be exported.
    pub(crate) model_source: Option<Arc<ModelSource>>,
    pub(crate) audio: Option<AudioView>,
    /// Files with no other preview: their bytes.
    hex: Option<HexView>,
    /// The search bar of a text preview, while it is open.
    pub(crate) find: Option<Find>,
}

/// A previewed model's file and the texture dictionaries found for it, kept for exporting it.
pub(crate) struct ModelSource {
    pub(crate) data: Vec<u8>,
    pub(crate) dictionaries: Vec<TextureDictionary>,
}

/// What loading a preview needs, taken from the app when it is asked for:
/// the load runs on another thread.
pub(crate) struct PreviewRequest {
    pub(crate) name: String,
    pub(crate) location: Location,
    /// The names in the folder, where a model's textures are looked for.
    pub(crate) rows: Vec<String>,
    pub(crate) keys: Option<Arc<Keys>>,
    pub(crate) shared: Arc<SharedTextures>,
    /// The texture or sound to select once loaded (a reload after an edit).
    pub(crate) select: Option<String>,
    pub(crate) delay: std::time::Duration,
}

/// A preview read and parsed; its views are made on the UI thread.
pub(crate) struct Loaded {
    source: PreviewSource,
    name: String,
    info: String,
    content: Content,
    error: Option<String>,
    select: Option<String>,
}

enum Content {
    Text { text: String, codec: Codec, syntax: Syntax },
    Textures(TextureDictionary),
    Model(Vec<paleto_formats::drawable::Drawable>, Option<Arc<ModelSource>>),
    Audio(Vec<paleto_formats::awc::Sound>),
    /// The bytes, and the whole size when only the start was read.
    Hex(Vec<u8>, Option<u64>),
    Nothing,
}

impl App {
    pub fn preview(&mut self, name: &str, ctx: &egui::Context) {
        self.apply(Action::Preview(name.to_string()), ctx);
    }

    /// Opens what the user asked for on the command line: a package shows
    /// the installer, an archive or folder is browsed, other files are selected
    /// in their folder.
    /// Takes the requests of later launches (files to open, raising the window).
    #[cfg(target_os = "linux")]
    pub fn set_instance(&mut self, instance: crate::single_instance::Instance, ctx: &egui::Context) {
        instance.attach(ctx);
        self.instance = Some(instance);
    }

    /// Opens what later launches handed over, and brings the window forward.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_requests(&mut self, ctx: &egui::Context) {
        while let Some(request) = self.instance.as_ref().and_then(|i| i.poll()) {
            for file in &request.files {
                self.open_path(file, ctx);
            }
            let raised = match (&self.activator, &request.token) {
                (Some(activator), Some(token)) => activator.activate(token).is_some(),
                _ => false,
            };
            if !raised {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
        }
    }

    pub fn open_path(&mut self, path: &Path, ctx: &egui::Context) {
        // Links kept: a game folder linking to other drives stays one folder.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if path.is_dir() {
            self.go(Location::Folder(path));
        } else if ext == "oiv" {
            self.open_package(&path, ctx);
        } else if ext == "rpf" {
            self.open(ArchivePath::new(path));
        } else {
            self.apply(Action::Reveal(path), ctx);
        }
    }

    /// Replaces the text in the preview editor, as if typed.
    pub fn edit_preview(&mut self, text: &str) {
        if let Some((current, _)) = self.preview.as_mut().and_then(|p| p.text.as_mut()) {
            *current = text.to_string();
        }
    }

    /// Closes the preview panel.
    pub fn close_preview(&mut self, ctx: &egui::Context) {
        self.apply(Action::ClosePreview, ctx);
    }

    pub fn preview_text(&self) -> Option<&str> {
        self.preview.as_ref()?.text.as_ref().map(|(t, _)| t.as_str())
    }

    /// Re-indents the XML being edited, as the Format button does.
    pub fn format_preview(&mut self, ctx: &egui::Context) {
        self.apply(Action::FormatPreview, ctx);
    }

    /// Saves the edited preview, as Ctrl+S does.
    pub fn save_preview(&mut self, ctx: &egui::Context) {
        self.apply(Action::SavePreview, ctx);
    }


    /// Textures of the previewed dictionary, and the selected one.
    pub fn preview_textures(&self) -> Option<(Vec<String>, usize)> {
        let view = self.preview.as_ref()?.textures.as_ref()?;
        Some((view.dict.textures.iter().map(|t| t.name.clone()).collect(), view.selected))
    }

    pub fn edit_texture(&mut self, edit: TextureEdit, ctx: &egui::Context) {
        self.apply(Action::EditTexture(edit), ctx);
    }

    /// The details line of the preview panel.
    pub fn preview_info(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.info.as_str())
    }

    /// Opens a help window (or closes it with `None`), as the menus do.
    pub fn show_info(&mut self, info: Option<Info>, ctx: &egui::Context) {
        self.apply(Action::ShowInfo(info), ctx);
    }

    pub fn previewing(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.name.as_str())
    }

    /// Closes the preview, and drops a load that would open one.
    pub(crate) fn clear_preview(&mut self) {
        self.preview = None;
        self.preview_task = None;
    }

    /// Makes the views of a preview read in the background, and shows it.
    pub(crate) fn show_loaded(&mut self, loaded: Loaded) {
        let Loaded { source, name, mut info, content, error, select } = loaded;
        if let Some(e) = error {
            self.status = Status::Error(e);
        }
        let (mut text, mut textures, mut model, mut model_source, mut audio, mut hex) = (None, None, None, None, None, None);
        let (mut codec, mut syntax) = (Codec::Plain, Syntax::for_file(&name));
        match content {
            Content::Text { text: t, codec: c, syntax: s } => (text, codec, syntax) = (Some((t.clone(), t)), c, s),
            Content::Textures(dict) => {
                let mut view = TextureView::new(dict);
                add_info(&mut info, view.summary());
                if let Some(i) = select.as_ref().and_then(|s| view.dict.textures.iter().position(|t| t.name.eq_ignore_ascii_case(s))) {
                    view.select(i);
                }
                textures = Some(view);
            }
            Content::Model(drawables, source) => {
                let view = ModelView::new(drawables);
                add_info(&mut info, view.summary());
                model = Some(view);
                model_source = source;
            }
            Content::Audio(sounds) => {
                let mut view = AudioView::new(sounds);
                add_info(&mut info, view.summary());
                if let Some(id) = select.as_ref().and_then(|s| u32::from_str_radix(s, 16).ok()) {
                    view.select_id(id);
                }
                audio = Some(view);
            }
            Content::Hex(data, total) => {
                let view = HexView::new(data, total);
                add_info(&mut info, view.summary());
                hex = Some(view);
            }
            Content::Nothing => {}
        }
        self.preview = Some(Preview { source, name, syntax, info, text, codec, textures, model, model_source, audio, hex, find: None });
    }

    pub(crate) fn preview_dirty(&self) -> bool {
        self.preview.as_ref().and_then(|p| p.text.as_ref()).is_some_and(|(text, saved)| text != saved)
    }

    /// Whether `action` would close the preview and lose its edits.
    pub(crate) fn discards_preview(&self, action: &Action) -> bool {
        let Some(p) = &self.preview else { return false };
        let here = self.browser.as_ref().map(|b| self.original_of(&b.location));
        match action {
            Action::Quit | Action::ClosePreview | Action::SelectGame(_) | Action::PickGame => true,
            Action::Preview(name) | Action::Enter(name) => *name != p.name,
            Action::Go(location) => Some(self.original_of(location)) != here,
            Action::Reveal(path) => path.parent().map(|d| Location::Folder(d.to_path_buf())) != here,
            Action::Back => !self.back.is_empty(),
            Action::Forward => !self.forward.is_empty(),
            Action::Delete => self.confirm_delete.as_ref().is_some_and(|names| names.contains(&p.name)),
            Action::Rename { from, .. } => *from == p.name,
            // Files of the mods folder move or go: the preview is closed.
            Action::DeleteCopies(_)
            | Action::ToggleMod(..)
            | Action::HuntStart
            | Action::HuntAnswer(_)
            | Action::HuntStop
            | Action::HuntInside(_)
            | Action::RestoreSnapshot(..) => true,
            _ => false,
        }
    }

    pub(crate) fn preview_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let Some(p) = &mut self.preview else { return };
        let editable = matches!(p.source, PreviewSource::Archive(_)) && !matches!(p.codec, Codec::Summary);
        let dirty = p.text.as_ref().is_some_and(|(text, saved)| text != saved);
        let height = crate::widgets::bar_height(ui);
        let has_text = p.text.is_some();
        let can_format = editable && has_text && p.syntax == Syntax::Xml;
        // Labels while they fit with the name, else icons alone (names on hover).
        const SEARCH: &str = "🔍 Search";
        const FORMAT: &str = "⚙ Format";
        const SAVE: &str = "💾 Save";
        let padding = 2.0 * ui.ctx().global_style().spacing.button_padding.x;
        let width_of = |ui: &egui::Ui, text: &str| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + padding;
        let shown: Vec<&str> = [(has_text, SEARCH), (can_format, FORMAT), (editable && has_text, SAVE)].into_iter().filter(|(on, _)| *on).map(|(_, t)| t).collect();
        let spacing = ui.spacing().item_spacing.x;
        let needed = height + shown.iter().map(|t| width_of(ui, t) + spacing).sum::<f32>() + spacing + 110.0;
        let compact = needed > ui.available_width();
        let mut toggle_find = false;
        ui.horizontal(|ui| {
            ui.set_min_height(height);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::bar_button(ui, egui::Button::new("✖"), height).on_hover_text("Close the preview").clicked() {
                    actions.push(Action::ClosePreview);
                }
                let button = |label: &str| {
                    let text = if compact { label.split_once(' ').map_or(label, |(icon, _)| icon) } else { label };
                    egui::Button::new(text)
                };
                let min = if compact { height } else { 0.0 };
                if let Some((text, saved)) = &p.text
                    && editable
                {
                    let enabled = text != saved && !busy;
                    let response = ui.add_enabled_ui(enabled, |ui| crate::widgets::bar_button(ui, button(SAVE), min)).inner;
                    if response.on_hover_text("Save (Ctrl+S)").clicked() {
                        actions.push(Action::SavePreview);
                    }
                }
                if let Some((text, _)) = &p.text
                    && can_format
                {
                    let tidy = highlight::is_tidy(ui.ctx(), text);
                    let hint = "Format: indent the lines by the XML structure and drop blank lines between tags (only whitespace changes); save to keep it";
                    let enabled = !tidy && !busy;
                    let response = ui.add_enabled_ui(enabled, |ui| crate::widgets::bar_button(ui, button(FORMAT), min)).inner;
                    if response.on_hover_text(hint).on_disabled_hover_text(if tidy { "Already indented" } else { hint }).clicked() {
                        actions.push(Action::FormatPreview);
                    }
                }
                if has_text && crate::widgets::bar_button(ui, button(SEARCH).selected(p.find.is_some()), min).on_hover_text("Search in the file (Ctrl+F)").clicked() {
                    toggle_find = true;
                }
                // The name takes the room left, shortened if need be.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let title = RichText::new(if dirty { format!("{} *", p.name) } else { p.name.clone() }).strong();
                    let title = if dirty { title.color(theme::modded(ui.visuals().dark_mode)) } else { title };
                    ui.add(egui::Label::new(title).truncate()).on_hover_text(if dirty { format!("{} (unsaved changes)", p.name) } else { p.name.clone() });
                });
            });
        });
        if toggle_find {
            p.find = if p.find.is_some() { None } else { Some(Find::new()) };
        }
        // The search bar.
        let mut close_find = false;
        if let (Some(find), Some((text, _))) = (p.find.as_mut(), p.text.as_ref()) {
            find.refresh(text);
            ui.horizontal(|ui| {
                ui.set_min_height(height);
                let controls = 3.0 * (height + spacing) + 90.0;
                let field = ui.add(
                    egui::TextEdit::singleline(&mut find.query)
                        .hint_text("Find in file")
                        .desired_width((ui.available_width() - controls).max(80.0))
                        .min_size(egui::vec2(0.0, height))
                        .align(egui::Align2::LEFT_CENTER),
                );
                if find.focus {
                    field.request_focus();
                    find.focus = false;
                }
                if field.changed() {
                    find.current = 0;
                    find.jump = true;
                    find.refresh(text);
                }
                let (enter, shift, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
                if field.lost_focus() && enter {
                    find.step(!shift);
                    field.request_focus();
                }
                // The field gives up the keyboard on Esc itself, in the same frame.
                if (field.has_focus() || field.lost_focus()) && escape {
                    close_find = true;
                }
                let count = if find.query.is_empty() {
                    String::new()
                } else if find.matches.is_empty() {
                    "No match".to_string()
                } else {
                    format!("{} of {}", find.current + 1, find.matches.len())
                };
                ui.label(RichText::new(count).weak());
                let some = !find.matches.is_empty();
                ui.add_enabled_ui(some, |ui| {
                    if crate::widgets::bar_button(ui, egui::Button::new("⏶"), height).on_hover_text("Previous match (Shift+Enter)").clicked() {
                        find.step(false);
                    }
                    if crate::widgets::bar_button(ui, egui::Button::new("⏷"), height).on_hover_text("Next match (Enter)").clicked() {
                        find.step(true);
                    }
                });
                if crate::widgets::bar_button(ui, egui::Button::new("✖"), height).on_hover_text("Close the search (Esc)").clicked() {
                    close_find = true;
                }
            });
        }
        if close_find {
            p.find = None;
        }
        if !p.info.is_empty() {
            ui.label(RichText::new(&p.info).weak());
        }
        ui.separator();
        match &mut p.text {
            Some((text, _)) => {
                let syntax = p.syntax;
                // Bring the match into view: its line to the middle, and its column in sight.
                let mut scroll = None;
                if let Some(find) = p.find.as_mut().filter(|f| f.jump) {
                    find.jump = false;
                    if let Some(m) = find.matches.get(find.current) {
                        let font = egui::TextStyle::Monospace.resolve(ui.style());
                        let (row, glyph) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
                        let before = &text[..m.start];
                        let line = before.bytes().filter(|b| *b == b'\n').count();
                        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
                        let y = (line as f32 * row - ui.available_height() / 2.0).max(0.0);
                        let x = ((column as f32 + 8.0) * glyph - ui.available_width()).max(0.0);
                        scroll = Some((x, y));
                    }
                }
                let marks = p.find.as_ref().filter(|f| !f.matches.is_empty());
                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                    let mut job = match marks {
                        Some(f) => highlight::layout_marked(ui, buf.as_str(), syntax, &f.matches, f.current),
                        None => highlight::layout(ui, buf.as_str(), syntax),
                    };
                    job.wrap.max_width = wrap_width;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let mut area = egui::ScrollArea::both().auto_shrink(false);
                if let Some((x, y)) = scroll {
                    area = area.vertical_scroll_offset(y).horizontal_scroll_offset(x);
                }
                let edited = area
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .code_editor()
                                .interactive(editable)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter),
                        )
                        .changed()
                    })
                    .inner;
                if edited && let Some(find) = &mut p.find {
                    find.stale = true;
                }
            }
            None => match &mut p.textures {
                Some(view) => match view.ui(ui, busy, editable) {
                    Some(Request::Export(export)) => actions.push(Action::ExportTextures(export, None)),
                    Some(Request::Replace(texture)) => actions.push(Action::PickTexture(Some(texture))),
                    Some(Request::Add) => actions.push(Action::PickTexture(None)),
                    Some(Request::ReplaceMany { folder }) => actions.push(Action::PickTextureBatch { folder }),
                    Some(Request::Rename(name)) => actions.push(Action::AskTexture(TextureEdit::Rename { from: name.clone(), to: name })),
                    Some(Request::Remove(name)) => actions.push(Action::AskTexture(TextureEdit::Remove(name))),
                    None => {}
                },
                None => {
                    if let Some(view) = &mut p.model {
                        if p.model_source.is_some() {
                            let button = ui.add_enabled(!busy, egui::Button::new("⬇ Export as XML…"));
                            if button.on_hover_text("Write CodeWalker XML (for Sollumz or CodeWalker) and the model's textures as DDS into a folder").clicked() {
                                actions.push(Action::ExportModel(None));
                            }
                        }
                        view.ui(ui);
                    } else if let Some(view) = &mut p.audio {
                        if let Some(request) = view.ui(ui, busy, editable) {
                            actions.push(match request {
                                AudioRequest::Replace(id) => Action::ReplaceSound(id, None),
                                request => Action::ExportSounds(request, None),
                            });
                        }
                    } else if let Some(view) = &p.hex {
                        view.ui(ui);
                    } else {
                        let ext = extension(&p.name);
                        let kind = if ext.is_empty() { "this file type".to_string() } else { format!(".{} files", ext.to_ascii_lowercase()) };
                        ui.vertical_centered(|ui| {
                            ui.add_space((ui.available_height() / 2.0 - 40.0).max(0.0));
                            ui.label(RichText::new("🚫").size(32.0).weak());
                            ui.label(RichText::new(format!("No preview for {kind}")).size(17.0).strong());
                            ui.label(RichText::new("Extract the file to open it with another program.").weak());
                        });
                    }
                }
            },
        }
    }

}

/// Reads and parses a file for its preview (on a worker thread).
pub(crate) fn load_preview(req: PreviewRequest) -> Loaded {
    std::thread::sleep(req.delay);
    let name = &req.name;
    // Only what the file list does not say: its type and size are there.
    let mut info = String::new();
    // The size of a large file on disk of which only the start is read, for the hex view.
    let mut partial = None;
    let (source, data) = match &req.location {
        Location::Folder(dir) => {
            let path = dir.join(name);
            let data = match std::fs::metadata(&path) {
                Ok(m) if m.len() as usize <= MAX_TEXT || binary_preview(name) => std::fs::read(&path).map_err(|e| e.to_string()),
                Ok(m) => {
                    partial = Some(m.len());
                    read_start(&path, hex_view::MAX_HEX).map_err(|e| e.to_string())
                }
                Err(_) => Ok(Vec::new()),
            };
            add_info(&mut info, "read-only");
            (PreviewSource::Disk(path), data)
        }
        Location::Archive(a) => {
            let path = a.join(name);
            let data = path.read(req.keys.as_deref()).map_err(|e| e.to_string());
            (PreviewSource::Archive(path), data)
        }
    };
    let ext = extension(name).to_ascii_lowercase();
    let fail = |e: paleto_formats::Error| (Content::Nothing, Some(format!("{name}: {e}")));
    let text = |text: String, codec: Codec, syntax: Syntax| Content::Text { text, codec, syntax };
    let (content, error) = match data {
        // Only its start was read: nothing but the hex view can show that.
        Ok(data) if partial.is_some() => (Content::Hex(data, partial), None),
        Ok(data) if is_ytd(name) => TextureDictionary::parse(&data).map_or_else(fail, |dict| (Content::Textures(dict), None)),
        Ok(data) if matches!(ext.as_str(), "ydr" | "yft" | "ydd") => {
            let dictionaries = sibling_textures(&req);
            match paleto_formats::drawable::parse(&data, &ext, &dictionaries) {
                Ok(drawables) => (Content::Model(drawables, Some(Arc::new(ModelSource { data, dictionaries }))), None),
                Err(e) => fail(e),
            }
        }
        Ok(data) if ext == "ybn" => match paleto_formats::bounds::parse(&data) {
            Ok(drawables) => (Content::Model(drawables, Some(Arc::new(ModelSource { data, dictionaries: Vec::new() }))), None),
            Err(e) => fail(e),
        },
        Ok(data) if ext == "ynv" => paleto_formats::navmesh::parse(&data).map_or_else(fail, |drawables| (Content::Model(drawables, None), None)),
        Ok(data) if ext == "ycd" => match paleto_formats::clips::to_text(&data) {
            Ok(summary) => {
                add_info(&mut info, "animation clips, a summary (read-only)");
                (text(summary, Codec::Summary, Syntax::Plain), None)
            }
            Err(e) => fail(e),
        },
        Ok(data) if ext == "awc" => {
            let key = req.keys.as_ref().and_then(|k| k.awc);
            paleto_formats::awc::parse(&data, key.as_ref()).map_or_else(fail, |sounds| (Content::Audio(sounds), None))
        }
        Ok(data) if ext == "gxt2" => match paleto_formats::gxt2::parse(&data) {
            Ok(entries) => {
                add_info(&mut info, format!("{} texts, shown as 0xHASH = text", entries.len()));
                (text(paleto_formats::gxt2::to_text(&entries), Codec::Gxt2, Syntax::for_file(name)), None)
            }
            Err(e) => fail(e),
        },
        Ok(data) if MetaFile::is_meta(&data) => match MetaFile::parse(&data) {
            Ok(meta) => {
                add_info(&mut info, format!("{}, shown as XML", meta.kind()));
                (text(meta.to_xml(), Codec::Meta(Arc::new(meta)), Syntax::Xml), None)
            }
            Err(e) => fail(e),
        },
        Ok(data) => match as_text(&data) {
            Some(t) => (text(t, Codec::Plain, Syntax::for_file(name)), None),
            None => (Content::Hex(data, None), None),
        },
        Err(e) => (Content::Nothing, Some(e)),
    };
    Loaded { source, name: req.name.clone(), info, content, error, select: req.select.clone() }
}

/// Texture dictionaries next to a model, named after it (`name.ytd`, and
/// `name+hi.ytd` as vehicles have).
/// The texture dictionaries a model draws from: those of its name next to
/// it, then the ones its dictionary inherits from (up to vehshare.ytd).
/// Tuning parts (in "<vehicle>_mods.rpf") use their vehicle's.
fn sibling_textures(req: &PreviewRequest) -> Vec<TextureDictionary> {
    let model = req.name.as_str();
    let stem = model.rsplit_once('.').map_or(model, |(s, _)| s).to_ascii_lowercase();
    let read_here = |name: &str| match &req.location {
        Location::Folder(dir) => std::fs::read(dir.join(name)).ok(),
        Location::Archive(a) => a.join(name).read(req.keys.as_deref()).ok(),
    };
    let here = |name: &str| req.rows.iter().find(|r| r.eq_ignore_ascii_case(&format!("{name}.ytd"))).cloned();
    let mut names = vec![stem.clone(), format!("{stem}+hi")];
    if let Location::Archive(a) = &req.location {
        let archive = a.inner.iter().rev().chain(a.disk.file_name().map(|n| n.to_str().unwrap_or_default().to_string()).iter()).find(|p| p.to_ascii_lowercase().ends_with("_mods.rpf")).cloned();
        if let Some(vehicle) = archive.map(|n| n.to_ascii_lowercase().trim_end_matches("_mods.rpf").to_string()) {
            names.extend([vehicle.clone(), format!("{vehicle}+hi")]);
        }
    }
    // The model's own dictionaries come from next to it; shared ones from
    // where the game loads them last (a DLC may patch them), else next to it.
    let own = names.len();
    for name in names.clone() {
        for parent in req.shared.parents_of(&name) {
            if !names.contains(&parent) {
                names.push(parent);
            }
        }
    }
    let shared = |name: &str| req.shared.place(name).and_then(|p| p.read(req.keys.as_deref()).ok());
    let local = |name: &str| here(name).and_then(|file| read_here(&file));
    names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let data = if i < own { local(name).or_else(|| shared(name)) } else { shared(name).or_else(|| local(name)) };
            TextureDictionary::parse(&data?).ok()
        })
        .collect()
}

/// Every folder under `root`; `None` once cancelled. `delay` slows each folder down, for tests.
pub(crate) fn subfolders(root: &Path, cancel: &Cancel, delay: std::time::Duration) -> Option<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.is_set() {
            return None;
        }
        std::thread::sleep(delay);
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
                out.push(path.clone());
                stack.push(path);
            }
        }
    }
    Some(out)
}

/// Binary files with a preview of their own, read whatever their size.
fn binary_preview(name: &str) -> bool {
    matches!(extension(name).as_str(), "YTD" | "YDR" | "YFT" | "YDD" | "YBN" | "YNV" | "YCD" | "AWC" | "GXT2" | "YMT" | "YTYP" | "YMAP" | "PSO")
}

fn as_text(data: &[u8]) -> Option<String> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    if data.len() > MAX_TEXT || data.iter().take(4096).any(|&b| b == 0) {
        return None;
    }
    String::from_utf8(data.to_vec()).ok()
}

/// The first `limit` bytes of a file.
fn read_start(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::with_capacity(limit);
    std::fs::File::open(path)?.take(limit as u64).read_to_end(&mut data)?;
    Ok(data)
}

pub(crate) fn load_icon(ctx: &egui::Context, png: &[u8]) -> Option<egui::TextureHandle> {
    // A package's picture is small; a bigger one (or one that claims to be) is not decoded.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(64 << 20);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture("package-icon", color, Default::default()))
}

pub(crate) fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()).unwrap_or_default()
}
