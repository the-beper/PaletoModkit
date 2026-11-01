//! The dialogs of the app window: installing packages and add-on packs, saving the previewed
//! text, replacing textures, asking for a name, deleting.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use paleto_core::oiv::Metadata;
use paleto_core::texture_batch::{self, TextureBatch};
use paleto_core::{ArchivePath, Package};

use crate::scripthook_window::ScriptHookWindow;
use crate::theme;
use crate::textures::TextureEdit;

use crate::app::{App, Action, Info, Status, Unsaved};
use crate::menus::{about, format_combo, getting_started, shortcuts};
use crate::preview::{PreviewSource, load_icon};


/// Add-on packs (dlc.rpf) dropped or chosen, to install into the mods folder.
pub(crate) struct AddonDialog {
    /// Each pack and the name it goes under (editable).
    pub(crate) packs: Vec<(PathBuf, String)>,
    /// How many files made for Legacy the packs hold (Enhanced edition).
    pub(crate) legacy: usize,
    pub(crate) convert: bool,
}

/// Files dropped or chosen to add to an archive, some made for Legacy.
pub(crate) struct LegacyAdd {
    pub(crate) paths: Vec<PathBuf>,
    /// The Legacy files, relative to what was chosen.
    pub(crate) legacy: Vec<String>,
    pub(crate) convert: bool,
}

pub(crate) struct InstallDialog {
    path: PathBuf,
    pub(crate) package: Option<Package>,
    pub(crate) metadata: Metadata,
    icon: Option<egui::TextureHandle>,
    /// The package's files made for Legacy (Enhanced edition only).
    pub(crate) legacy: Vec<String>,
    /// Convert them before installing.
    pub(crate) convert: bool,
    result: Option<InstallResult>,
}

/// The log of an install, or why it failed and what it did and undid before.
type InstallResult = Result<Vec<String>, (String, Vec<String>)>;

pub(crate) enum Outcome {
    /// An operation finished; show the message and reopen `reload` if given.
    Done { message: String, reload: Option<ArchivePath> },
    Installed(InstallResult),
    Failed(String),
    /// What undoing or setting aside interrupted changes did.
    Recovered(Result<paleto_core::txn::Report, String>, bool),
}

pub(crate) struct NameDialog {
    /// The item being renamed, or None for a new folder.
    pub(crate) renaming: Option<String>,
    pub(crate) name: String,
    /// Focus the text field (and select the name) on the next frame.
    pub(crate) focus: bool,
}

pub(crate) struct TextureDialog {
    pub(crate) edit: TextureEdit,
    /// Focus the name field on the next frame.
    pub(crate) focus: bool,
}

/// Replacing many textures of the previewed dictionary from files.
pub(crate) struct TextureBatchDialog {
    pub(crate) batch: TextureBatch,
    /// Also add the files that match no texture, as new textures.
    pub(crate) add_unmatched: bool,
}

impl App {
    pub fn open_package(&mut self, path: &Path, ctx: &egui::Context) {
        match Package::open(path) {
            Ok(mut package) => {
                let icon = package.icon().and_then(|png| load_icon(ctx, &png));
                let metadata = package.metadata.clone();
                let legacy = match &self.game {
                    Some(game) if game.edition == paleto_core::Edition::Enhanced => package.legacy_files(self.keys.as_deref()).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let convert = paleto_core::convert::converter().is_some();
                self.install = Some(InstallDialog { path: path.to_path_buf(), package: Some(package), metadata, icon, legacy, convert, result: None });
            }
            Err(e) => self.status = Status::Error(format!("{}: {e}", path.display())),
        }
    }
    /// The text of the rename / new folder dialog, if it is open.
    pub fn name_dialog(&self) -> Option<&str> {
        self.name_dialog.as_ref().map(|d| d.name.as_str())
    }

    /// Whether the "unsaved changes" prompt is showing.
    pub fn asking_unsaved(&self) -> bool {
        self.pending.is_some()
    }
    /// Opens the texture dialog, as after picking a file for `Add` or `Replace`.
    pub fn ask_texture(&mut self, edit: TextureEdit, ctx: &egui::Context) {
        self.apply(Action::AskTexture(edit), ctx);
    }

    /// The change the texture dialog is asking about, if it is open.
    /// Matches files (or the pictures in folders) to the previewed
    /// dictionary's textures and asks before replacing them.
    pub fn ask_texture_batch(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        self.apply(Action::AskTextureBatch(paths), ctx);
    }

    /// The textures about to be replaced from files, while that dialog is open.
    pub fn texture_batch(&self) -> Option<&TextureBatch> {
        self.texture_batch.as_ref().map(|d| &d.batch)
    }

    /// Confirms the batch dialog, as its button does.
    pub fn confirm_texture_batch(&mut self, add_unmatched: bool, ctx: &egui::Context) {
        if let Some(d) = self.texture_batch.take() {
            self.apply(Action::EditTextures(batch_edits(&d.batch, add_unmatched)), ctx);
        }
    }

    pub fn texture_dialog(&self) -> Option<&TextureEdit> {
        self.texture_dialog.as_ref().map(|d| &d.edit)
    }
    /// Shows how an install ended in its dialog; a failed add-on install (which has none) opens one.
    pub(crate) fn show_install_result(&mut self, result: InstallResult) {
        if result.is_err()
            && let Some(game) = &self.game
        {
            // What a failed install could not undo is an interrupted change now: the next change is refused until it is settled.
            self.interrupted = paleto_core::txn::pending(game);
        }
        // Add-on packs have no dialog of their own: only a failure opens one.
        if result.is_ok() && self.install.is_none() {
            return;
        }
        let dialog = self.install.get_or_insert_with(|| InstallDialog {
            path: PathBuf::new(),
            package: None,
            metadata: paleto_core::oiv::Metadata { name: "Add-on packs".into(), ..Default::default() },
            icon: None,
            legacy: Vec::new(),
            convert: false,
            result: None,
        });
        dialog.result = Some(result);
    }

    /// Shows this failed install as if it had just run, for the tests.
    #[doc(hidden)]
    pub fn offer_install_failure(&mut self, e: &paleto_core::Error) {
        self.show_install_result(Err(install_failure(e, Vec::new())));
    }

    /// Tells the app of changes an earlier run left interrupted, as if they were found when the game was chosen.
    #[doc(hidden)]
    pub fn offer_interrupted(&mut self, pending: Vec<paleto_core::txn::Pending>) {
        self.interrupted = pending.clone();
        self.startup.offer_interrupted(pending);
    }

    /// How many interrupted changes are pending in the chosen game.
    #[doc(hidden)]
    pub fn interrupted_count(&self) -> usize {
        self.interrupted.len()
    }

    /// Installs this zip as the Script Hook V update instead of downloading one.
    pub fn set_script_hook_zip(&mut self, zip: Vec<u8>) {
        self.startup.zip = Some(zip);
    }

    /// The notice on screen, in words.
    pub fn notice_text(&self) -> Option<String> {
        self.startup.text()
    }

    pub fn open_script_hook(&mut self, ctx: &egui::Context) {
        self.apply(Action::ScriptHook(true), ctx);
    }

    /// What the "Script Hook V" window shows, while it is open.
    pub fn script_hook_summary(&self) -> Option<String> {
        self.script_hook.as_ref().map(ScriptHookWindow::summary)
    }
    /// Asks to install the add-on packs found in `paths` (as when dropped).
    pub fn ask_addon_packs(&mut self, paths: &[PathBuf], ctx: &egui::Context) {
        let packs = paleto_core::addon::find_packs(paths, self.keys.as_deref());
        self.apply(Action::AskAddonPacks(packs), ctx);
    }

    pub fn add_dlc_packs(&mut self, packs: &[&str], ctx: &egui::Context) {
        self.apply(Action::AddDlcPacks(packs.iter().map(|p| p.to_string()).collect()), ctx);
    }
    pub(crate) fn install_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let (game_ready, keys_ready, keys_pending) = (self.game.is_some(), self.keys.is_some(), self.keys_task.is_some());
        let can_convert = paleto_core::convert::converter().is_some();
        let Some(d) = &mut self.install else { return };
        egui::Modal::new(egui::Id::unique("install")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                if let Some(icon) = &d.icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(64.0, 64.0)));
                }
                ui.vertical(|ui| {
                    ui.heading(&d.metadata.name);
                    let version = (!d.metadata.version.is_empty()).then(|| format!("Version {}", d.metadata.version));
                    let author = (!d.metadata.author.is_empty()).then(|| format!("by {}", d.metadata.author));
                    let line: Vec<String> = version.into_iter().chain(author).collect();
                    if !line.is_empty() {
                        ui.label(line.join("   ·   "));
                    }
                    if !d.path.as_os_str().is_empty() {
                        ui.label(RichText::new(d.path.display().to_string()).weak().small());
                    }
                });
            });
            ui.separator();
            match &d.result {
                None => {
                    egui::ScrollArea::vertical().max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
                        ui.label(&d.metadata.description);
                    });
                    ui.separator();
                    crate::widgets::hint(ui, "Game data is installed into the mods folder; the original files are not modified.");
                    if !d.legacy.is_empty() {
                        let dark = ui.visuals().dark_mode;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("⚠").color(theme::warning(dark)));
                            ui.label(match d.legacy.len() {
                                1 => "1 file in this package was made for Legacy: the Enhanced game may crash on it.".to_string(),
                                n => format!("{n} files in this package were made for Legacy: the Enhanced game may crash on them."),
                            });
                        })
                        .response
                        .on_hover_text(d.legacy.join("\n"));
                        if can_convert {
                            let it = if d.legacy.len() == 1 { "it" } else { "them" };
                            ui.checkbox(&mut d.convert, format!("Convert {it} to Enhanced while installing (with CodeWalker's converter)"));
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "{} (the converter) is not installed: convert {} with CodeWalker's Asset Converter.",
                                    paleto_core::convert::TOOL,
                                    if d.legacy.len() == 1 { "it" } else { "them" }
                                ))
                                .weak(),
                            );
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if busy {
                            ui.spinner();
                            ui.label("Installing…");
                        } else if !game_ready {
                            ui.label(RichText::new("Choose the game folder first (Game menu).").color(theme::error(ui.visuals().dark_mode)));
                            if ui.button("Cancel").clicked() {
                                actions.push(Action::CloseInstall);
                            }
                        } else if !keys_ready {
                            if keys_pending {
                                ui.spinner();
                                ui.label("Preparing keys…");
                            } else {
                                ui.label(
                                    RichText::new("The game's encryption keys could not be prepared (see the status bar).")
                                        .color(theme::error(ui.visuals().dark_mode)),
                                );
                            }
                            if ui.button("Cancel").clicked() {
                                actions.push(Action::CloseInstall);
                            }
                        } else {
                            if ui.add(crate::widgets::primary(ui, "Install")).clicked() {
                                actions.push(Action::Install);
                            }
                            if ui.button("Cancel").clicked() {
                                actions.push(Action::CloseInstall);
                            }
                        }
                    });
                }
                Some(result) => {
                    match result {
                        Ok(_) => ui.label(RichText::new("✔ Installed").strong().color(theme::ok(ui.visuals().dark_mode))),
                        Err((e, _)) => ui.label(RichText::new(format!("✖ {e}")).color(theme::error(ui.visuals().dark_mode))),
                    };
                    let log = match result {
                        Ok(log) | Err((_, log)) => log,
                    };
                    // What is left and how to recover come before the log, so a long log cannot hide them.
                    let (log, left): (&[String], &[String]) = match log.iter().position(|l| l.starts_with("Left behind")) {
                        Some(at) => (&log[..at], &log[at..]),
                        None => (log, &[]),
                    };
                    for line in left {
                        ui.label(RichText::new(line).monospace().small().color(theme::error(ui.visuals().dark_mode)));
                    }
                    if !log.is_empty() {
                        egui::ScrollArea::vertical().max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
                            for line in log {
                                let text = RichText::new(line).monospace().small();
                                ui.label(if line.starts_with("warning") { text.color(theme::modded(ui.visuals().dark_mode)) } else { text });
                            }
                        });
                    }
                    ui.add_space(6.0);
                    if ui.add(crate::widgets::primary(ui, "Close")).clicked() {
                        actions.push(Action::CloseInstall);
                    }
                }
            }
        });
    }

    pub(crate) fn unsaved_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.pending.is_none() {
            return;
        }
        let name = self.preview.as_ref().map_or(String::new(), |p| p.name.clone());
        let can_save = !self.busy();
        let response = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("{name} has changes that are not saved. Save them before going on?"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(can_save, crate::widgets::primary(ui, "💾 Save")).clicked() {
                    actions.push(Action::Unsaved(Unsaved::Save));
                }
                if ui.add(crate::widgets::danger(ui, "Discard")).clicked() {
                    actions.push(Action::Unsaved(Unsaved::Discard));
                }
                if ui.button("Cancel").clicked() {
                    actions.push(Action::Unsaved(Unsaved::Cancel));
                }
            });
        });
        if response.should_close() {
            actions.push(Action::Unsaved(Unsaved::Cancel));
        }
    }

    pub(crate) fn info_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(info) = self.info else { return };
        let title = match info {
            Info::GettingStarted => "Getting started",
            Info::Shortcuts => "Keyboard and mouse shortcuts",
            Info::About => "About",
        };
        let ((), closed) = crate::widgets::dialog(ctx, "info", title, 460.0, |ui| match info {
            Info::GettingStarted => getting_started(ui),
            Info::Shortcuts => shortcuts(ui),
            Info::About => about(ui),
        });
        if closed {
            actions.push(Action::ShowInfo(None));
        }
    }

    /// Asks for the new name of an item, or the name of a new folder.
    pub(crate) fn name_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let taken: Vec<String> = self.browser.iter().flat_map(|b| b.rows.iter().map(|r| r.name.to_ascii_lowercase())).collect();
        let on_disk = self.disk_folder().is_some();
        let Some(d) = &mut self.name_dialog else { return };
        let renaming = d.renaming.clone();
        // Unchanged (rename only), or why the name cannot be used.
        let check = |name: &str| -> Result<bool, &'static str> {
            let lower = name.to_ascii_lowercase();
            if name.is_empty() || name == "." || name == ".." {
                Err("Enter a name.")
            } else if name.contains(['/', '\\']) {
                Err("Names cannot contain / or \\.")
            } else if renaming.as_ref().is_some_and(|old| if on_disk { old == name } else { old.to_ascii_lowercase() == lower }) {
                Ok(true)
            } else if renaming.as_ref().is_some_and(|old| old.to_ascii_lowercase() == lower) {
                // Only the case changes, on disk.
                Ok(false)
            } else if taken.contains(&lower) {
                Err("An item with this name already exists here.")
            } else {
                Ok(false)
            }
        };
        let mut submit = None;
        let response = egui::Modal::new(egui::Id::unique("name")).show(ctx, |ui| {
            ui.heading(if renaming.is_some() { "Rename" } else { "New folder" });
            let id = egui::Id::unique("name-field");
            let field = ui.add(egui::TextEdit::singleline(&mut d.name).id(id).desired_width(320.0));
            if d.focus {
                d.focus = false;
                field.request_focus();
                // Select the name without its extension, like file managers do.
                let stem = d.name.rfind('.').filter(|&i| i > 0 && renaming.is_some()).unwrap_or(d.name.len());
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                    let range = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(d.name[..stem].chars().count()));
                    state.cursor.set_char_range(Some(range));
                    state.store(ui.ctx(), id);
                }
            }
            let name = d.name.trim().to_string();
            let checked = check(&name);
            if let Err(problem) = checked {
                ui.label(RichText::new(problem).color(theme::error(ui.visuals().dark_mode)));
            }
            if !on_disk {
                ui.label(RichText::new("Names are stored in lower case. Original game archives are copied to the mods folder first.").weak());
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ok = ui.add_enabled(checked.is_ok(), crate::widgets::primary(ui, if renaming.is_some() { "Rename" } else { "Create" }));
                if checked.is_ok() && (ok.clicked() || enter) {
                    submit = Some((name, checked == Ok(true)));
                }
                ui.button("Cancel").clicked()
            })
            .inner
        });
        let done = submit.is_some() || response.inner || response.should_close();
        match (submit, renaming) {
            (Some((to, false)), Some(from)) => actions.push(Action::Rename { from, to }),
            (Some((name, _)), None) => actions.push(Action::CreateFolder(name)),
            _ => {}
        }
        if done {
            self.name_dialog = None;
        }
    }

    /// Completes or confirms a change to the previewed textures: a name for
    /// new and renamed textures, a format for pictures, or a removal.
    pub(crate) fn texture_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(view) = self.preview.as_ref().and_then(|p| p.textures.as_ref()) else {
            self.texture_dialog = None;
            return;
        };
        let (dict, ytd_name) = (view.dict.clone(), self.preview.as_ref().map(|p| p.name.clone()).unwrap_or_default());
        let Some(d) = &mut self.texture_dialog else { return };
        let mut submit = false;
        let response = egui::Modal::new(egui::Id::unique("texture")).show(ctx, |ui| {
            let (title, button) = match &d.edit {
                TextureEdit::Replace { .. } => ("Replace texture", "Replace"),
                TextureEdit::Add { .. } => ("Add texture", "Add"),
                TextureEdit::Rename { .. } => ("Rename texture", "Rename"),
                TextureEdit::Remove(_) => ("Remove texture?", "Remove"),
            };
            ui.heading(title);
            let mut problem = None;
            let mut enter = false;
            // The name to enter, with the current one when renaming.
            let (name, old): (Option<&mut String>, Option<String>) = match &mut d.edit {
                TextureEdit::Add { name, .. } => (Some(name), None),
                TextureEdit::Rename { from, to } => (Some(to), Some(from.clone())),
                _ => (None, None),
            };
            if let Some(name) = name {
                let id = egui::Id::unique("texture-name");
                let field = ui.add(egui::TextEdit::singleline(name).id(id).desired_width(320.0));
                if d.focus {
                    d.focus = false;
                    field.request_focus();
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(name.chars().count()));
                        state.cursor.set_char_range(Some(all));
                        state.store(ui.ctx(), id);
                    }
                }
                enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let name = name.trim();
                problem = if name.is_empty() {
                    Some("Enter a name.")
                } else if name.contains(['/', '\\']) {
                    Some("Names cannot contain / or \\.")
                } else if old.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(name)) {
                    None
                } else if dict.textures.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
                    Some("A texture with this name already exists.")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    ui.label(RichText::new(problem).color(theme::error(ui.visuals().dark_mode)));
                }
            }
            let replacing = matches!(d.edit, TextureEdit::Replace { .. });
            match &mut d.edit {
                TextureEdit::Replace { name: texture, file, format } | TextureEdit::Add { name: texture, file, format } => {
                    let file_name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(format!("From {file_name}"));
                    if file.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
                        let current = dict.textures.iter().find(|t| t.name.eq_ignore_ascii_case(texture)).map(|t| t.format);
                        let auto = match current.filter(|f| replacing && f.can_encode()) {
                            Some(f) => format!("Same as now: {}", f.name()),
                            None => "Automatic: BC1, or BC3 with transparency".to_string(),
                        };
                        format_combo(ui, format, auto);
                        ui.label(RichText::new("Mipmaps are generated when the size is a power of two.").weak());
                    } else {
                        ui.label(RichText::new("The DDS file is used as it is, with its format and mipmaps.").weak());
                    }
                }
                TextureEdit::Remove(name) => {
                    ui.label(format!("{name} will be removed from {ytd_name}."));
                }
                TextureEdit::Rename { .. } => {}
            }
            ui.label(RichText::new("Original game archives are copied to the mods folder first.").weak());
            ui.horizontal(|ui| {
                let ok = ui.add_enabled(
                    problem.is_none(),
                    if matches!(d.edit, TextureEdit::Remove(_)) { crate::widgets::danger(ui, button) } else { crate::widgets::primary(ui, button) },
                );
                submit = problem.is_none() && (ok.clicked() || enter);
                ui.button("Cancel").clicked()
            })
            .inner
        });
        if submit {
            let mut edit = d.edit.clone();
            if let TextureEdit::Add { name, .. } | TextureEdit::Rename { to: name, .. } = &mut edit {
                *name = name.trim().to_string();
            }
            // Renaming to the very same name changes nothing.
            if !matches!(&edit, TextureEdit::Rename { from, to } if from == to) {
                actions.push(Action::EditTexture(edit));
            }
        }
        if submit || response.inner || response.should_close() {
            self.texture_dialog = None;
        }
    }

    /// Whether dropped files are pictures for the dictionary being previewed
    /// (inside an archive): DDS or PNG files, or folders holding some.
    pub(crate) fn dropped_textures(&self, files: &[PathBuf]) -> bool {
        let editable_ytd = self.preview.as_ref().is_some_and(|p| p.textures.is_some() && matches!(p.source, PreviewSource::Archive(_)));
        editable_ytd
            && !files.is_empty()
            && files.iter().all(|f| texture_batch::is_texture_file(f) || f.is_dir())
            && !texture_batch::texture_files(files).is_empty()
    }

    pub(crate) fn texture_batch_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.preview.as_ref().is_none_or(|p| p.textures.is_none()) {
            self.texture_batch = None;
        }
        let ytd_name = self.preview.as_ref().map(|p| p.name.clone()).unwrap_or_default();
        let Some(d) = &mut self.texture_batch else { return };
        let file_name = |f: &PathBuf| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut confirm = false;
        let response = egui::Modal::new(egui::Id::unique("texture-batch")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading(format!("Replace textures in {ytd_name}"));
            let height = crate::widgets::dialog_list_height(ui.ctx()) / 2.0;
            let batch = &d.batch;
            if batch.replace.is_empty() {
                ui.label(format!("No file is named after a texture of {ytd_name}."));
            } else {
                ui.label(format!("{} {} replaced:", batch.replace.len(), if batch.replace.len() == 1 { "texture will be" } else { "textures will be" }));
                egui::ScrollArea::vertical().id_salt("batch-replace").max_height(height).show(ui, |ui| {
                    for (texture, file) in &batch.replace {
                        ui.label(format!("{texture}  ⬅  {}", file_name(file)));
                    }
                });
            }
            if !batch.unmatched.is_empty() {
                ui.add_space(6.0);
                ui.label(format!("{} {} no texture:", batch.unmatched.len(), if batch.unmatched.len() == 1 { "file matches" } else { "files match" }));
                egui::ScrollArea::vertical().id_salt("batch-unmatched").max_height(height).show(ui, |ui| {
                    for file in &batch.unmatched {
                        ui.label(RichText::new(file_name(file)).weak());
                    }
                });
                ui.checkbox(&mut d.add_unmatched, "Add them as new textures");
            }
            if !batch.duplicates.is_empty() {
                ui.label(RichText::new(format!("{} skipped: another file has the same name (DDS files win over PNG).", batch.duplicates.len())).weak());
            }
            ui.add_space(6.0);
            ui.label(RichText::new("DDS files are used as they are; PNG files keep each texture's format. Original game archives are copied to the mods folder first.").weak());
            ui.horizontal(|ui| {
                let count = batch.replace.len() + if d.add_unmatched { batch.unmatched.len() } else { 0 };
                confirm = ui.add_enabled(count > 0, crate::widgets::primary(ui, "Replace")).clicked();
                ui.button("Cancel").clicked()
            })
            .inner
        });
        if confirm {
            actions.push(Action::EditTextures(batch_edits(&d.batch, d.add_unmatched)));
        }
        if confirm || response.inner || response.should_close() {
            self.texture_batch = None;
        }
    }

    pub(crate) fn addon_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let can_convert = paleto_core::convert::converter().is_some();
        let Some(game) = &self.game else { return };
        let Some(dialog) = &mut self.addon_dialog else { return };
        let response = egui::Modal::new(egui::Id::unique("addon-packs")).show(ctx, |ui| {
            let dark = ui.visuals().dark_mode;
            ui.set_max_width(560.0);
            ui.heading(if dialog.packs.len() == 1 { "Install add-on pack" } else { "Install add-on packs" });
            ui.label("Copied to the mods folder as update/x64/dlcpacks/<name>/dlc.rpf and added to its DLC list, so the game loads it.");
            let mut valid = true;
            for (dlc, name) in &mut dialog.packs {
                ui.add_space(4.0);
                ui.label(RichText::new(dlc.display().to_string()).weak().small());
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(name);
                });
                let lower = name.to_ascii_lowercase();
                if !paleto_core::addon::valid_name(&lower) {
                    valid = false;
                    ui.label(RichText::new("Use letters, digits, '_' and '-'.").color(theme::error(dark)));
                } else if paleto_core::addon::pack_path(game, &lower).is_file() {
                    ui.label(RichText::new(format!("Replaces the pack {lower} already in the mods folder.")).color(theme::warning(dark)));
                }
            }
            if dialog.legacy > 0 {
                ui.add_space(4.0);
                let (what, it) = match dialog.legacy {
                    1 => ("1 file in it is".to_string(), "it"),
                    n => (format!("{n} files in them are"), "them"),
                };
                ui.label(format!("⚠ {what} in the Legacy format: the Enhanced game may crash on {it}."));
                if can_convert {
                    ui.checkbox(&mut dialog.convert, format!("Convert {it} to Enhanced (with CodeWalker's converter)"));
                }
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(valid, crate::widgets::primary(ui, "Install")).clicked() {
                    actions.push(Action::InstallAddonPacks);
                }
                ui.button("Cancel").clicked()
            })
            .inner
        });
        if response.inner || response.should_close() {
            self.addon_dialog = None;
        }
    }

    pub(crate) fn legacy_add_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let can_convert = paleto_core::convert::converter().is_some();
        let Some(add) = &mut self.legacy_add else { return };
        let response = egui::Modal::new(egui::Id::unique("legacy-add")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Files made for Legacy");
            let (what, it) = match add.legacy.len() {
                1 => ("1 file is".to_string(), "it"),
                n => (format!("{n} files are"), "them"),
            };
            ui.label(format!("{what} in the Legacy format: the Enhanced game may crash on {it} or ignore {it}."));
            let shown: Vec<&str> = add.legacy.iter().take(8).map(String::as_str).collect();
            ui.label(RichText::new(shown.join("\n")).monospace().small());
            if add.legacy.len() > 8 {
                ui.label(format!("…and {} more", add.legacy.len() - 8));
            }
            if can_convert {
                ui.checkbox(&mut add.convert, format!("Convert {it} to Enhanced (with CodeWalker's converter)"));
                ui.label(RichText::new("The files on disk are not changed.").weak());
            } else {
                ui.label(
                    RichText::new(format!(
                        "{} (the converter) is not installed: convert {it} with CodeWalker's Asset Converter.",
                        paleto_core::convert::TOOL
                    ))
                    .weak(),
                );
            }
            ui.horizontal(|ui| {
                if ui.add(crate::widgets::primary(ui, "Add")).clicked() {
                    actions.push(Action::AddLegacy { convert: can_convert && add.convert });
                }
                ui.button("Cancel").clicked()
            })
            .inner
        });
        if response.inner || response.should_close() {
            self.legacy_add = None;
        }
    }

    pub(crate) fn delete_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(names) = &self.confirm_delete else { return };
        let dir = self.disk_folder();
        let original = self.original_data(names);
        // Files of the game itself, outside the mods folder.
        let game_files = match (&dir, &self.game) {
            (Some(dir), Some(game)) => dir.starts_with(&game.root) && !dir.starts_with(game.mods_dir()) && !(dir == &game.root && names.iter().all(|n| n == "mods")),
            _ => false,
        };
        let response = egui::Modal::new(egui::Id::unique("delete")).show(ctx, |ui| {
            let dark = ui.visuals().dark_mode;
            ui.heading(if names.len() == 1 { "Delete this item?".to_string() } else { format!("Delete these {} items?", names.len()) });
            ui.add_space(4.0);
            for name in names.iter().take(8) {
                ui.label(RichText::new(name).monospace());
            }
            if names.len() > 8 {
                ui.label(format!("…and {} more", names.len() - 8));
            }
            ui.add_space(4.0);
            if dir.is_some() {
                if original.is_none() {
                    let what = if names.len() == 1 { "It is" } else { "They are" };
                    ui.label(RichText::new(format!("{what} deleted from the disk for good, folders with all they hold.")).color(theme::warning(dark)));
                }
                if let Some((name, why)) = &original {
                    ui.label(RichText::new(format!("{name} {why}")).color(theme::error(dark)));
                } else if game_files {
                    ui.label(
                        RichText::new("These are game files: the game may not start until Steam downloads them again (Verify integrity of game files).")
                            .color(theme::error(dark)),
                    );
                }
            } else {
                ui.label(RichText::new("Original game archives are copied to the mods folder first.").weak());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(original.is_none(), crate::widgets::danger(ui, "Delete")).clicked() {
                    actions.push(Action::Delete);
                }
                ui.button("Cancel").clicked()
            })
            .inner
        });
        if response.inner || response.should_close() {
            self.confirm_delete = None;
        }
    }
}

/// The texture changes a batch makes: replacements, then the other files as new textures.
fn batch_edits(batch: &TextureBatch, add_unmatched: bool) -> Vec<TextureEdit> {
    let mut edits: Vec<TextureEdit> = batch.replace.iter().map(|(name, file)| TextureEdit::Replace { name: name.clone(), file: file.clone(), format: None }).collect();
    if add_unmatched {
        for file in &batch.unmatched {
            let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            edits.push(TextureEdit::Add { name, file: file.clone(), format: None });
        }
    }
    edits
}

/// What a set of texture changes did, for the status bar.
pub(crate) fn edit_message(edits: &[TextureEdit], ytd: &str) -> String {
    if let [edit] = edits {
        return match edit {
            TextureEdit::Replace { name, .. } => format!("replaced {name} in {ytd}"),
            TextureEdit::Add { name, .. } => format!("added {name} to {ytd}"),
            TextureEdit::Rename { from, to } => format!("renamed {from} to {to} in {ytd}"),
            TextureEdit::Remove(name) => format!("removed {name} from {ytd}"),
        };
    }
    let replaced = edits.iter().filter(|e| matches!(e, TextureEdit::Replace { .. })).count();
    let added = edits.iter().filter(|e| matches!(e, TextureEdit::Add { .. })).count();
    let textures = |n: usize| if n == 1 { "1 texture".to_string() } else { format!("{n} textures") };
    match (replaced, added) {
        (r, 0) => format!("replaced {} in {ytd}", textures(r)),
        (0, a) => format!("added {} to {ytd}", textures(a)),
        (r, a) => format!("replaced {} and added {} in {ytd}", textures(r), textures(a)),
    }
}

/// What the install dialog shows of a failed install: the error, then the log of what was done and undone
/// (after the lines of `log`, what ran earlier), and when the undo was not clean, what is left.
pub(crate) fn install_failure(e: &paleto_core::Error, mut log: Vec<String>) -> (String, Vec<String>) {
    let paleto_core::Error::Txn(f) = e else { return (e.to_string(), log) };
    log.extend(f.log.iter().cloned());
    if f.undone && f.failures.is_empty() {
        return (e.to_string(), log);
    }
    log.push("Left behind (not undone):".into());
    log.extend(f.failures.iter().map(|l| format!("  {l}")));
    log.push(if f.journaled {
        "The backups and the journal were kept: Paleto offers to finish the recovery when you choose this game again, or run `paleto recover`.".into()
    } else {
        "These files were left as they are, with their backups next to them.".into()
    });
    (format!("{}: the changes could not be fully undone", f.cause), log)
}

/// What an install did, from its log: the files it added, edited, deleted or
/// converted, and its warnings. "saved" lines only close archives whose edits
/// are already counted.
pub(crate) fn install_summary(log: &[String]) -> String {
    let warnings = log.iter().filter(|l| l.starts_with("warning:")).count();
    let changes = log.iter().filter(|l| !l.starts_with("warning:") && !l.starts_with("saved ")).count();
    let plural = |n: usize, what: &str| if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") };
    match warnings {
        0 => format!("Installed successfully ({})", plural(changes, "change")),
        n => format!("Installed successfully ({}, {})", plural(changes, "change"), plural(n, "warning")),
    }
}

/// Sends a desktop notification via `notify-send` (fire-and-forget).
/// Does nothing if `notify-send` is not available.
pub(crate) fn desktop_notify(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("notify-send")
            .arg("--app-name=Paleto Modkit")
            .arg("--icon=io.github.the_beper.PaletoModkit")
            .arg(&summary)
            .arg(&body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}
