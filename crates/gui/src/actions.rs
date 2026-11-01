//! The actions of the window: what they mean for the state and the game's files, and the
//! router that runs them.

use std::path::PathBuf;

use eframe::egui;
use paleto_core::texture_batch;
use paleto_core::{ArchivePath, Game, vfs};

use crate::app::{App, Info, Location, Status};
use crate::audio::{self, AudioRequest};
use crate::browser::{AfterListing, HistoryMove, RowKind, sort_rows};
use crate::crash_window::CrashWindow;
use crate::dialogs::{AddonDialog, LegacyAdd, NameDialog, Outcome, TextureBatchDialog, TextureDialog, edit_message, install_failure};
use crate::export::{ExportDest, extract, with_notes};
use crate::export_window::ExportWindow;
use crate::mods_check::ModsCheck;
use crate::prefix_window::PrefixWindow;
use crate::scripthook_window::ScriptHookWindow;
use crate::settings::Settings;
use crate::snapshots_window::SnapshotsWindow;
use crate::preview::{PreviewRequest, PreviewSource, load_preview, subfolders};
use crate::task::Task;
use crate::textures::{self, Export, TextureEdit};

impl Action {
    /// Whether it must wait for the background task: it starts one (there is one
    /// at a time), or switches game under it.
    fn waits_for_task(&self) -> bool {
        self.starts_task() || matches!(self, Action::SelectGame(_) | Action::PickGame)
    }

    /// Whether it changes the game's files or its mods folder: refused while an interrupted change is pending.
    fn mutates(&self) -> bool {
        match self {
            Action::Install
                | Action::EditTextures(..)
                | Action::ReplaceSound(..)
                | Action::EditTexture(..)
                | Action::SavePreview
                | Action::Add(..)
                | Action::AddLegacy { .. }
                | Action::InstallAddonPacks
                | Action::DeleteCopies(..)
                | Action::UndoConversion(..)
                | Action::Rename { .. }
                | Action::CreateFolder(..)
                | Action::Delete
                | Action::ToggleMod(..)
                | Action::ScriptHookUpdate(..)
                | Action::TurnOff(..)
                | Action::HuntStart
                | Action::HuntAnswer(..)
                | Action::HuntStop
                | Action::HuntInside(..)
                | Action::SaveSnapshot(..)
                | Action::RestoreSnapshot(..)
                | Action::DeleteSnapshot(..)
                | Action::AddDlcPacks(..)
                | Action::ConvertMods(..) => true,
            Action::Quit
                | Action::CheckMods(..)
                | Action::ScriptHook(..)
                | Action::Snapshots(..)
                | Action::PrefixTweaks(..)
                | Action::Export(..)
                | Action::WriteExport(..)
                | Action::CrashDetection(..)
                | Action::AskAddonPacks(..)
                | Action::RecoverInterrupted
                | Action::AbandonInterrupted(..)
                | Action::FindCopies
                | Action::Unsaved(..)
                | Action::SelectGame(..)
                | Action::PickGame
                | Action::PickArchive
                | Action::PickPackage
                | Action::CloseInstall
                | Action::Go(..)
                | Action::Reveal(..)
                | Action::Back
                | Action::Forward
                | Action::Enter(..)
                | Action::Preview(..)
                | Action::ClosePreview
                | Action::ExportTextures(..)
                | Action::ExportModel(..)
                | Action::ExportSounds(..)
                | Action::PickTexture(..)
                | Action::AskTexture(..)
                | Action::PickTextureBatch { .. }
                | Action::AskTextureBatch(..)
                | Action::FormatPreview
                | Action::Extract(..)
                | Action::PickAdd { .. }
                | Action::AskDelete(..)
                | Action::AskRename(..)
                | Action::AskNewFolder
                | Action::ToggleOriginals
                | Action::PeekOriginal(..)
                | Action::Refresh
                | Action::ExpandAll
                | Action::CollapseAll
                | Action::CancelTask
                | Action::ShowInfo(..)
                | Action::Settings(..) => false,
        }
    }

    /// Whether running it starts the background task.
    fn starts_task(&self) -> bool {
        matches!(
            self,
            Action::RecoverInterrupted
                | Action::AbandonInterrupted(_)
                | Action::Install
                | Action::EditTextures(_)
                | Action::ReplaceSound(..)
                | Action::ExportSounds(..)
                | Action::ExportTextures(..)
                | Action::ExportModel(_)
                | Action::SavePreview
                | Action::Extract(..)
                | Action::Add(_)
                | Action::AddLegacy { .. }
                | Action::InstallAddonPacks
                | Action::DeleteCopies(_)
                | Action::UndoConversion(_)
                | Action::Rename { .. }
                | Action::CreateFolder(_)
                | Action::Delete
                | Action::ToggleMod(..)
                | Action::ScriptHookUpdate(_)
                | Action::WriteExport(..)
                | Action::TurnOff(_)
                | Action::HuntStart
                | Action::HuntAnswer(_)
                | Action::HuntStop
                | Action::HuntInside(_)
                | Action::SaveSnapshot(_)
                | Action::RestoreSnapshot(..)
                | Action::DeleteSnapshot(_)
                | Action::AddDlcPacks(_)
                | Action::ConvertMods(_)
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Unsaved {
    Save,
    Discard,
    Cancel,
}

pub(crate) enum Action {
    Quit,
    /// Open (or refresh) the "Mods in game" window, or close it.
    CheckMods(bool),
    /// Turn a mod (its part path) on or off.
    ToggleMod(String, bool),
    /// Open the "Script Hook V" window, or close it.
    ScriptHook(bool),
    /// Install this Script Hook V release over the installed one.
    ScriptHookUpdate(paleto_core::scripthook::Release),
    /// Add DLC packs to the mods copy's dlclist.xml.
    AddDlcPacks(Vec<String>),
    /// Open the "Snapshots" window, or close it.
    Snapshots(bool),
    /// Open the "Prefix tweaks" window, or close it.
    PrefixTweaks(bool),
    SaveSnapshot(String),
    /// Restore a snapshot, saving the current mods folder first if true.
    RestoreSnapshot(String, bool),
    DeleteSnapshot(String),
    /// Open the "Export mods" window (comparing the mods folder), or close it.
    Export(bool),
    WriteExport(PathBuf, Vec<paleto_core::export::Changed>, paleto_core::export::Metadata),
    /// Open the "Crash detection" window, or close it.
    CrashDetection(bool),
    /// Turn an `.asi` plugin off (from Crash detection).
    TurnOff(String),
    HuntStart,
    /// Whether the game still crashed.
    HuntAnswer(bool),
    HuntStop,
    /// Look inside this archive of the mods folder for the culprit file.
    HuntInside(String),
    /// Convert these files of the mods folder to the Enhanced format.
    ConvertMods(Vec<String>),
    /// Open the add-on pack dialog for these packs.
    AskAddonPacks(Vec<paleto_core::addon::Pack>),
    InstallAddonPacks,
    /// Put back the Legacy files of a conversion of the mods folder.
    UndoConversion(String),
    /// Undo (or finish) the changes an earlier run left interrupted.
    RecoverInterrupted,
    /// Set these interrupted journals (by id) aside, after the confirmation.
    AbandonInterrupted(Vec<String>),
    FindCopies,
    DeleteCopies(Vec<paleto_core::cleanup::Copy>),
    /// Add the files of the Legacy dialog, converting them or not.
    AddLegacy { convert: bool },
    Unsaved(Unsaved),
    SelectGame(PathBuf),
    PickGame,
    PickArchive,
    PickPackage,
    Install,
    CloseInstall,
    Go(Location),
    /// Opens the folder containing a file and selects the file.
    Reveal(PathBuf),
    Back,
    Forward,
    Enter(String),
    Preview(String),
    ClosePreview,
    /// Export textures of the previewed dictionary; asks where when `dest` is None.
    ExportTextures(Export, Option<PathBuf>),
    /// Export the previewed model as CodeWalker XML with its textures; asks for a folder when `dest` is None.
    ExportModel(Option<PathBuf>),
    /// Export sounds of the previewed audio container; asks where when `dest` is None.
    ExportSounds(AudioRequest, Option<PathBuf>),
    /// Replace a sound (by id) of the previewed container with a WAV or MP3 file; asks which when None.
    ReplaceSound(u32, Option<PathBuf>),
    /// Ask for a DDS or PNG file to replace this texture with, or to add when None.
    PickTexture(Option<String>),
    /// Ask to confirm or complete a change to the previewed textures.
    AskTexture(TextureEdit),
    EditTexture(TextureEdit),
    /// Pick several DDS/PNG files (or a folder) to replace textures by name.
    PickTextureBatch { folder: bool },
    /// Match these files (or the pictures in these folders) to the textures.
    AskTextureBatch(Vec<PathBuf>),
    /// Several changes to the previewed dictionary, in one go.
    EditTextures(Vec<TextureEdit>),
    SavePreview,
    /// Re-indent the XML being edited.
    FormatPreview,
    /// Extract the named entries into a folder (asked for unless given).
    Extract(Vec<String>, Option<PathBuf>),
    Add(Vec<PathBuf>),
    PickAdd { folders: bool },
    AskDelete(Vec<String>),
    /// Ask for a new name for an item of the current archive folder.
    AskRename(String),
    AskNewFolder,
    Rename { from: String, to: String },
    CreateFolder(String),
    Delete,
    ToggleOriginals,
    /// Show the open archive's original (true) or its mods copy (false).
    PeekOriginal(bool),
    Refresh,
    ExpandAll,
    CollapseAll,
    /// Stops the cancellable task the status bar shows.
    CancelTask,
    ShowInfo(Option<Info>),
    Settings(Settings),
}

impl App {
    /// Runs an action, first asking about unsaved edits it would throw away.
    pub(crate) fn apply(&mut self, action: Action, ctx: &egui::Context) {
        if self.refused_while_interrupted(&action) {
            return;
        }
        // Refused before asking about unsaved edits: discarding them would be for nothing.
        if action.waits_for_task() && self.busy() {
            self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
            return;
        }
        if self.preview_dirty() && self.discards_preview(&action) {
            self.pending = Some(action);
            return;
        }
        self.run(action, ctx);
    }

    /// Refuses a change to the game while an interrupted one is pending: the next change would undo it
    /// without telling, and what it names should be seen first. Reads stay available.
    fn refused_while_interrupted(&mut self, action: &Action) -> bool {
        if self.interrupted.is_empty() || !action.mutates() {
            return false;
        }
        // Settled meanwhile (by the command line, say): the gate lifts. Else the notice comes back, so Undo can be chosen.
        if let Some(game) = &self.game {
            self.interrupted = paleto_core::txn::pending(game);
        }
        if self.interrupted.is_empty() {
            return false;
        }
        self.startup.offer_interrupted(self.interrupted.clone());
        self.status = Status::Error("An interrupted change must be undone first".into());
        true
    }

    fn run(&mut self, action: Action, ctx: &egui::Context) {
        if self.refused_while_interrupted(&action) {
            return;
        }
        // A second operation would replace the running one halfway.
        if action.waits_for_task() && self.busy() {
            self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
            return;
        }
        match action {
            Action::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::Unsaved(choice) => {
                let pending = self.pending.take();
                match choice {
                    Unsaved::Save if self.busy() => {
                        // The save would be refused, and the pending action run unsaved.
                        self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
                        self.pending = pending;
                    }
                    Unsaved::Save => {
                        self.run(Action::SavePreview, ctx);
                        // Continue once the file is written.
                        if self.task.is_some() {
                            self.after_task = pending;
                        } else if let Some(action) = pending {
                            self.apply(action, ctx);
                        }
                    }
                    Unsaved::Discard => {
                        if let Some((text, saved)) = self.preview.as_mut().and_then(|p| p.text.as_mut()) {
                            *text = saved.clone();
                        }
                        if let Some(action) = pending {
                            self.apply(action, ctx);
                        }
                    }
                    Unsaved::Cancel => {}
                }
            }
            Action::SelectGame(path) => match Game::open(&path) {
                Ok(game) => self.select_game(game, ctx),
                Err(e) => self.status = Status::Error(e.to_string()),
            },
            Action::PickGame => {
                if let Some(dir) = rfd::FileDialog::new().set_title("GTA V folder").pick_folder() {
                    self.apply(Action::SelectGame(dir), ctx);
                }
            }
            Action::PickArchive => {
                let mut dialog = rfd::FileDialog::new().add_filter("RPF archive", &["rpf"]);
                if let Some(game) = &self.game {
                    dialog = dialog.set_directory(&game.root);
                }
                if let Some(path) = dialog.pick_file() {
                    self.open(ArchivePath::new(path));
                }
            }
            Action::PickPackage => {
                let dialog = rfd::FileDialog::new().add_filter("Mod package (.oiv, dlc.rpf)", &["oiv", "rpf"]).set_title("Install a package or an add-on pack");
                if let Some(path) = dialog.pick_file() {
                    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpf")) {
                        let packs = paleto_core::addon::find_packs(std::slice::from_ref(&path), self.keys.as_deref());
                        if packs.is_empty() {
                            self.status = Status::Error(format!("{} is not a dlc.rpf", path.display()));
                        } else {
                            self.apply(Action::AskAddonPacks(packs), ctx);
                        }
                    } else {
                        self.open_package(&path, ctx);
                    }
                }
            }
            Action::Install => {
                let (Some(dialog), Some(game), Some(keys)) = (&mut self.install, &self.game, &self.keys) else { return };
                let Some(mut package) = dialog.package.take() else { return };
                let (game, keys) = (game.clone(), keys.clone());
                let tool = (dialog.convert && !dialog.legacy.is_empty()).then(paleto_core::convert::converter).flatten();
                self.task = Some(Task::spawn(format!("Installing {}", dialog.metadata.name), ctx, move || {
                    let mut log = Vec::new();
                    if let Some(tool) = &tool {
                        match package.convert_legacy(tool, Some(&keys)) {
                            Ok(converted) => log = converted,
                            Err(e) => return Outcome::Installed(Err(install_failure(&e, log))),
                        }
                    }
                    Outcome::Installed(match package.install(&game, &keys) {
                        Ok(done) => {
                            log.extend(done);
                            Ok(log)
                        }
                        Err(e) => Err(install_failure(&e, log)),
                    })
                }));
            }
            Action::CloseInstall => self.install = None,
            Action::Go(location) => self.go(location),
            Action::Reveal(path) => {
                let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
                let select = Some(name.to_string_lossy().into_owned());
                self.go_then(Location::Folder(dir.to_path_buf()), AfterListing { select, ..Default::default() });
            }
            Action::Back => {
                if let Some(location) = self.back.pop() {
                    if let Some(current) = self.current() {
                        self.forward.push(current);
                    }
                    self.reveal = true;
                    self.show_moving(location, AfterListing::default(), Some(HistoryMove::Back));
                }
            }
            Action::Forward => {
                if let Some(location) = self.forward.pop() {
                    if let Some(current) = self.current() {
                        self.back.push(current);
                    }
                    self.reveal = true;
                    self.show_moving(location, AfterListing::default(), Some(HistoryMove::Forward));
                }
            }
            Action::Enter(name) => {
                let Some(b) = &self.browser else { return };
                let Some(row) = b.rows.iter().find(|r| r.name == name) else { return };
                let location = match (&b.location, row.kind) {
                    (Location::Folder(dir), RowKind::Folder) => Location::Folder(dir.join(&name)),
                    (Location::Folder(dir), RowKind::Archive) => Location::Archive(ArchivePath::new(dir.join(&name))),
                    (Location::Archive(a), RowKind::Folder | RowKind::Archive) => Location::Archive(a.join(&name)),
                    _ => return self.apply(Action::Preview(name), ctx),
                };
                self.go(self.within(b, location));
            }
            Action::Preview(name) => {
                // The file being edited is already shown: reading it again would lose the edits.
                if self.preview_dirty() && self.previewing() == Some(name.as_str()) {
                    return;
                }
                let Some(b) = &self.browser else { return };
                let request = PreviewRequest {
                    location: b.location.clone(),
                    rows: b.rows.iter().map(|r| r.name.clone()).collect(),
                    keys: self.keys.clone(),
                    shared: self.shared.clone(),
                    select: self.preview_select.take(),
                    delay: self.load_delay,
                    name,
                };
                // Replacing a load drops it: its result never comes.
                self.preview_task = Some(Task::spawn_cancellable(format!("Loading {}", request.name), ctx, move |_| load_preview(request)));
            }
            Action::ClosePreview => self.clear_preview(),
            Action::PickTexture(replace) => {
                let title = replace.as_ref().map_or("Add texture".to_string(), |name| format!("Replace {name}"));
                let dialog = rfd::FileDialog::new().set_title(title).add_filter("DDS or PNG", &["dds", "png"]);
                let Some(file) = dialog.pick_file() else { return };
                let png = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
                match replace {
                    // Only a PNG needs a format; a DDS file is used as it is.
                    Some(name) if !png => self.apply(Action::EditTexture(TextureEdit::Replace { name, file, format: None }), ctx),
                    Some(name) => self.apply(Action::AskTexture(TextureEdit::Replace { name, file, format: None }), ctx),
                    None => {
                        let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        self.apply(Action::AskTexture(TextureEdit::Add { name, file, format: None }), ctx);
                    }
                }
            }
            Action::AskTexture(edit) => self.texture_dialog = Some(TextureDialog { edit, focus: true }),
            Action::EditTexture(edit) => self.apply(Action::EditTextures(vec![edit]), ctx),
            Action::PickTextureBatch { folder } => {
                let dialog = rfd::FileDialog::new().set_title("Replace textures from files named after them");
                let picked = if folder { dialog.pick_folder().map(|f| vec![f]) } else { dialog.add_filter("DDS or PNG", &["dds", "png"]).pick_files() };
                if let Some(paths) = picked {
                    self.apply(Action::AskTextureBatch(paths), ctx);
                }
            }
            Action::AskTextureBatch(paths) => {
                let Some(view) = self.preview.as_ref().and_then(|p| p.textures.as_ref()) else { return };
                let files = texture_batch::texture_files(&paths);
                if files.is_empty() {
                    self.status = Status::Error("No DDS or PNG files there.".into());
                    return;
                }
                let names: Vec<&str> = view.dict.textures.iter().map(|t| t.name.as_str()).collect();
                self.texture_batch = Some(TextureBatchDialog { batch: texture_batch::match_files(&names, &files), add_unmatched: false });
            }
            Action::EditTextures(edits) => {
                let Some(p) = &self.preview else { return };
                let PreviewSource::Archive(path) = &p.source else { return };
                let Some(first) = edits.first() else { return };
                let (path, ytd_name, keys) = (path.clone(), p.name.clone(), self.keys.clone());
                let (label, select) = match first {
                    _ if edits.len() > 1 => ("Replacing textures", String::new()),
                    TextureEdit::Replace { name, .. } => ("Replacing texture", name.clone()),
                    TextureEdit::Add { name, .. } => ("Adding texture", name.clone()),
                    TextureEdit::Rename { to, .. } => ("Renaming texture", to.clone()),
                    TextureEdit::Remove(_) => ("Removing texture", String::new()),
                };
                self.refresh_preview = Some(select);
                self.spawn_edit(label, ctx, path.parent().unwrap(), move |editor, folder| {
                    use paleto_formats::Edit;
                    let read = |file: &PathBuf| std::fs::read(file).map_err(|e| std::io::Error::other(format!("{}: {e}", file.display())));
                    let files = edits
                        .iter()
                        .map(|e| match e {
                            TextureEdit::Replace { file, .. } | TextureEdit::Add { file, .. } => read(file),
                            _ => Ok(Vec::new()),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let changes: Vec<Edit> = edits
                        .iter()
                        .zip(&files)
                        .map(|(e, data)| match e {
                            TextureEdit::Replace { name, format, .. } => Edit::Replace { name, file: data, format: *format },
                            TextureEdit::Add { name, format, .. } => Edit::Add { name, file: data, format: *format },
                            TextureEdit::Rename { from, to } => Edit::Rename { from, to },
                            TextureEdit::Remove(name) => Edit::Remove(name),
                        })
                        .collect();
                    let inner = vfs::join_inner(folder, &ytd_name);
                    let ytd = editor.read(&inner, keys.as_deref())?;
                    // On failure, try the changes one by one to name the file at fault.
                    let rebuilt = paleto_formats::edit_textures(&ytd, &changes).map_err(|e| {
                        let culprit = edits.iter().zip(&changes).find_map(|(edit, change)| {
                            let file = match edit {
                                TextureEdit::Replace { file, .. } | TextureEdit::Add { file, .. } => file,
                                _ => return None,
                            };
                            paleto_formats::edit_textures(&ytd, std::slice::from_ref(change)).err().map(|e| format!("{}: {e}", file.display()))
                        });
                        std::io::Error::other(culprit.unwrap_or_else(|| e.to_string()))
                    })?;
                    editor.add_file(&inner, rebuilt)?;
                    Ok(edit_message(&edits, &ytd_name))
                });
            }
            Action::ReplaceSound(id, file) => {
                let Some(p) = &self.preview else { return };
                let PreviewSource::Archive(path) = &p.source else { return };
                let picked = file.or_else(|| {
                    let dialog = rfd::FileDialog::new().set_title(format!("Replace sound {id:08X}"));
                    dialog.add_filter("WAV or MP3 audio", &["wav", "mp3"]).pick_file()
                });
                let Some(file) = picked else { return };
                let (path, name, keys) = (path.clone(), p.name.clone(), self.keys.clone());
                self.refresh_preview = Some(format!("{id:08X}"));
                self.spawn_edit("Replacing sound", ctx, path.parent().unwrap(), move |editor, folder| {
                    let inner = vfs::join_inner(folder, &name);
                    let data = std::fs::read(&file).map_err(|e| std::io::Error::other(format!("{}: {e}", file.display())))?;
                    let awc = editor.read(&inner, keys.as_deref())?;
                    let key = keys.as_ref().and_then(|k| k.awc);
                    let rebuilt = paleto_formats::awc::replace(&awc, key.as_ref(), id, &data)
                        .map_err(|e| std::io::Error::other(format!("{}: {e}", file.display())))?;
                    editor.add_file(&inner, rebuilt)?;
                    Ok(format!("replaced sound {id:08X} in {name}"))
                });
            }
            Action::ExportSounds(request, dest) => {
                let Some(view) = self.preview.as_ref().and_then(|p| p.audio.as_ref()) else { return };
                let sounds = view.sounds.clone();
                let (indices, dest) = match request {
                    AudioRequest::Export(i) => {
                        let name = format!("{}.wav", audio::file_stem(&sounds[i]));
                        let picked = dest.or_else(|| {
                            rfd::FileDialog::new().set_title("Export sound").set_file_name(&name).add_filter("WAV", &["wav"]).save_file()
                        });
                        let Some(path) = picked else { return };
                        (vec![i], ExportDest::File(path))
                    }
                    AudioRequest::ExportAll => {
                        let picked = dest.or_else(|| rfd::FileDialog::new().set_title("Export sounds to").pick_folder());
                        let Some(dir) = picked else { return };
                        ((0..sounds.len()).collect(), ExportDest::Folder(dir))
                    }
                    AudioRequest::Replace(_) => return,
                };
                self.task = Some(Task::spawn("Exporting sounds", ctx, move || {
                    let run = || -> Result<String, String> {
                        let (dir, picked) = dest.folder_and_name();
                        let (_, log) = vfs::write_into(dir, "exporting sounds", |out| {
                            for &i in &indices {
                                out.write(&picked.clone().unwrap_or_else(|| format!("{}.wav", audio::file_stem(&sounds[i]))), &sounds[i].to_wav())?;
                            }
                            Ok(())
                        })
                        .map_err(|e| e.to_string())?;
                        Ok(with_notes(match &dest {
                            ExportDest::File(path) => format!("exported {}", path.display()),
                            ExportDest::Folder(dir) => format!("{} sounds exported to {}", indices.len(), dir.display()),
                        }, &log))
                    };
                    match run() {
                        Ok(message) => Outcome::Done { message, reload: None },
                        Err(e) => Outcome::Failed(e),
                    }
                }));
            }
            Action::ExportTextures(export, dest) => {
                let Some(view) = self.preview.as_ref().and_then(|p| p.textures.as_ref()) else { return };
                let (dict, selected) = (view.dict.clone(), view.selected);
                let (indices, png, dest) = match export {
                    Export::Selected { png } => {
                        let name = dict.textures[selected].file_name(png);
                        let ext = if png { "png" } else { "dds" };
                        let picked = dest.or_else(|| {
                            let dialog = rfd::FileDialog::new().set_title("Export texture").set_file_name(&name);
                            dialog.add_filter(ext.to_uppercase(), &[ext]).save_file()
                        });
                        let Some(path) = picked else { return };
                        (vec![selected], png, ExportDest::File(path))
                    }
                    Export::All { png } => {
                        let picked = dest.or_else(|| rfd::FileDialog::new().set_title("Export textures to").pick_folder());
                        let Some(dir) = picked else { return };
                        ((0..dict.textures.len()).collect(), png, ExportDest::Folder(dir))
                    }
                };
                self.task = Some(Task::spawn("Exporting textures", ctx, move || {
                    let run = || -> Result<String, String> {
                        let (dir, picked) = dest.folder_and_name();
                        // Every texture is encoded before anything is written.
                        let files = indices.iter().map(|&i| Ok((picked.clone().unwrap_or_else(|| dict.textures[i].file_name(png)), textures::encode(&dict.textures[i], png)?))).collect::<Result<Vec<_>, String>>()?;
                        let (_, log) = vfs::write_into(dir, "exporting textures", |out| files.iter().try_for_each(|(name, data)| out.write(name, data).map(drop))).map_err(|e| e.to_string())?;
                        Ok(with_notes(match &dest {
                            ExportDest::File(path) => format!("exported {}", path.display()),
                            ExportDest::Folder(dir) => format!("{} textures exported to {}", indices.len(), dir.display()),
                        }, &log))
                    };
                    match run() {
                        Ok(message) => Outcome::Done { message, reload: None },
                        Err(e) => Outcome::Failed(e),
                    }
                }));
            }
            Action::ExportModel(dest) => {
                let Some(p) = &self.preview else { return };
                let Some(source) = p.model_source.clone() else { return };
                let file = p.name.clone();
                let Some(dir) = dest.or_else(|| rfd::FileDialog::new().set_title("Export model to").pick_folder()) else { return };
                self.task = Some(Task::spawn("Exporting model", ctx, move || {
                    let run = || -> Result<String, String> {
                        let (stem, ext) = file.rsplit_once('.').ok_or_else(|| format!("{file}: not a model"))?;
                        let export = paleto_formats::xml_export::export(&source.data, ext, stem, &source.dictionaries).map_err(|e| format!("{file}: {e}"))?;
                        let (_, log) = vfs::write_into(&dir, "exporting a model", |out| {
                            out.mkdir("")?;
                            out.write(&format!("{file}.xml"), export.xml.as_bytes())?;
                            // The textures go in a folder named after the model, where CodeWalker looks for them.
                            let textures = paleto_formats::xml_export::texture_folder(stem);
                            for (name, dds) in &export.textures {
                                out.write(&format!("{textures}/{name}"), dds)?;
                            }
                            Ok(())
                        })
                        .map_err(|e| e.to_string())?;
                        Ok(with_notes(format!("exported {file}.xml and {} textures to {}", export.textures.len(), dir.display()), &log))
                    };
                    match run() {
                        Ok(message) => Outcome::Done { message, reload: None },
                        Err(e) => Outcome::Failed(e),
                    }
                }));
            }
            Action::FormatPreview => {
                if let Some((text, _)) = self.preview.as_mut().and_then(|p| p.text.as_mut()) {
                    *text = paleto_core::xml::reindent(text);
                }
            }
            Action::SavePreview => {
                let Some(p) = &self.preview else { return };
                let PreviewSource::Archive(path) = &p.source else { return };
                let Some((text, _)) = &p.text else { return };
                // Converted text (XML, text tables) is checked before anything is saved.
                let data = match p.codec.encode(text) {
                    Ok(data) => data,
                    Err(e) => {
                        self.status = Status::Error(format!("not saved: {e}"));
                        self.after_task = None;
                        return;
                    }
                };
                let (path, name) = (path.clone(), p.name.clone());
                self.saving = Some((name.clone(), text.clone()));
                self.spawn_edit("Saving", ctx, path.parent().unwrap(), move |editor, folder| {
                    editor.add_file(&vfs::join_inner(folder, &name), data)?;
                    Ok(format!("saved {name}"))
                });
            }
            Action::Extract(names, picked) => {
                let Some(Location::Archive(location)) = self.browser.as_ref().map(|b| b.location.clone()) else { return };
                let Some(dest) = picked.or_else(|| rfd::FileDialog::new().set_title("Extract to").pick_folder()) else { return };
                let keys = self.keys.clone();
                self.task = Some(Task::spawn("Extracting", ctx, move || match extract(&location, keys.as_deref(), &names, &dest) {
                    Ok((n, notes)) => Outcome::Done { message: with_notes(format!("{n} files extracted to {}", dest.display()), &notes), reload: None },
                    Err(e) => Outcome::Failed(e),
                }));
            }
            Action::PickAdd { folders } => {
                let dialog = rfd::FileDialog::new().set_title("Add to archive");
                let picked = if folders { dialog.pick_folders() } else { dialog.pick_files() };
                if let Some(paths) = picked {
                    self.apply(Action::Add(paths), ctx);
                }
            }
            Action::Add(paths) => {
                let Some(Location::Archive(location)) = self.browser.as_ref().map(|b| b.location.clone()) else { return };
                if let Some(missing) = paths.iter().find(|p| std::fs::metadata(p).is_err()) {
                    let hint = if crate::sandboxed() {
                        " (the Flatpak cannot see that folder: drag the file from a file manager, or allow the folder with \"flatpak override --user --filesystem=FOLDER io.github.the_beper.PaletoModkit\")"
                    } else {
                        ""
                    };
                    self.status = Status::Error(format!("Cannot read {}{hint}", missing.display()));
                    return;
                }
                // Files made for Legacy while working on the Enhanced edition: ask first.
                if self.game.as_ref().is_some_and(|g| g.edition == paleto_core::Edition::Enhanced) {
                    let files: Vec<_> = paths.iter().filter_map(|p| vfs::local_files(p).ok()).flatten().collect();
                    let legacy = paleto_core::convert::legacy_in_local(&files, self.keys.as_deref());
                    if !legacy.is_empty() {
                        let convert = paleto_core::convert::converter().is_some();
                        self.legacy_add = Some(LegacyAdd { paths, legacy, convert });
                        return;
                    }
                }
                self.spawn_add(ctx, location, paths, None);
            }
            Action::AskAddonPacks(packs) => {
                let Some(game) = &self.game else { return };
                let legacy = if game.edition == paleto_core::Edition::Enhanced {
                    let files: Vec<_> = packs.iter().map(|p| (p.dlc.clone(), "dlc.rpf".to_string())).collect();
                    paleto_core::convert::legacy_in_local(&files, self.keys.as_deref()).len()
                } else {
                    0
                };
                let convert = paleto_core::convert::converter().is_some();
                self.addon_dialog = Some(AddonDialog { packs: packs.into_iter().map(|p| (p.dlc, p.name)).collect(), legacy, convert });
            }
            Action::InstallAddonPacks => {
                let (Some(dialog), Some(game)) = (self.addon_dialog.take(), self.game.clone()) else { return };
                let keys = self.keys.clone();
                let tool = (dialog.convert && dialog.legacy > 0).then(paleto_core::convert::converter).flatten();
                self.task = Some(Task::spawn("Installing add-on packs", ctx, move || {
                    let log = match paleto_core::addon::install_packs(&game, keys.as_deref(), &dialog.packs, tool.as_deref()) {
                        Ok(log) => log,
                        Err(e) => {
                            let (text, log) = install_failure(&e, Vec::new());
                            let text = match dialog.packs.as_slice() {
                                [(_, name)] => format!("{name}: {text}"),
                                _ => text,
                            };
                            return Outcome::Installed(Err((text, log)));
                        }
                    };
                    let converted = log.iter().filter(|l| l.starts_with("converted ")).count();
                    let names: Vec<String> = dialog.packs.iter().map(|(_, n)| n.to_ascii_lowercase()).collect();
                    let mut message = format!("installed {} and added to the DLC list", names.join(", "));
                    if converted > 0 {
                        message += &format!(" ({converted} files converted to Enhanced)");
                    }
                    Outcome::Done { message, reload: None }
                }));
            }
            Action::FindCopies => {
                let Some(game) = self.game.clone() else { return };
                self.copies_task = Some(Task::spawn("Comparing the mods folder with the game's files", ctx, move || {
                    paleto_core::cleanup::identical_copies(&game)
                }));
            }
            Action::DeleteCopies(copies) => {
                let Some(game) = self.game.clone() else { return };
                self.clear_preview();
                self.task = Some(Task::spawn("Deleting unchanged copies", ctx, move || match paleto_core::cleanup::delete(&game, &copies) {
                    Ok(n) => Outcome::Done { message: format!("deleted {n} copies of game files from the mods folder"), reload: None },
                    Err(e) => Outcome::Failed(e.to_string()),
                }));
                self.after_task = Some(Action::Refresh);
            }
            Action::RecoverInterrupted => {
                let Some(game) = self.game.clone() else { return };
                self.clear_preview();
                self.task = Some(Task::spawn("Undoing the interrupted change", ctx, move || Outcome::Recovered(paleto_core::txn::recover(&game).map_err(|e| e.to_string()), false)));
            }
            Action::AbandonInterrupted(ids) => {
                let Some(game) = self.game.clone() else { return };
                self.task = Some(Task::spawn("Setting the interrupted change aside", ctx, move || {
                    let mut all = paleto_core::txn::Report::default();
                    for id in ids {
                        match paleto_core::txn::abandon(&game, &id) {
                            Ok(report) => all.log.extend(report.log),
                            Err(e) => all.failures.push(format!("{id}: {e}")),
                        }
                    }
                    Outcome::Recovered(Ok(all), true)
                }));
            }
            Action::UndoConversion(time) => {
                let Some(game) = self.game.clone() else { return };
                let keys = self.keys.clone();
                self.task = Some(Task::spawn("Undoing the conversion", ctx, move || {
                    match paleto_core::convert::undo(&game, keys.as_deref(), &time) {
                        Ok(report) => {
                            let failed: Vec<String> =
                                report.iter().filter_map(|r| r.error.as_ref().map(|e| format!("{}: {e}", r.path))).collect();
                            if failed.is_empty() {
                                let n = report.len();
                                Outcome::Done { message: format!("{n} Legacy {} put back", if n == 1 { "file" } else { "files" }), reload: None }
                            } else {
                                Outcome::Failed(format!("not put back: {}", failed.join("; ")))
                            }
                        }
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
            }
            Action::AddLegacy { convert } => {
                let Some(add) = self.legacy_add.take() else { return };
                let Some(Location::Archive(location)) = self.browser.as_ref().map(|b| b.location.clone()) else { return };
                let tool = convert.then(paleto_core::convert::converter).flatten();
                self.spawn_add(ctx, location, add.paths, tool);
            }
            Action::AskDelete(names) => self.confirm_delete = Some(names),
            Action::AskRename(name) => {
                if self.refuse_original(std::slice::from_ref(&name)) {
                    return;
                }
                self.name_dialog = Some(NameDialog { renaming: Some(name.clone()), name, focus: true });
            }
            Action::AskNewFolder => self.name_dialog = Some(NameDialog { renaming: None, name: "new folder".into(), focus: true }),
            Action::Rename { from, to } => {
                if self.refuse_original(std::slice::from_ref(&from)) {
                    return;
                }
                if self.preview.as_ref().is_some_and(|p| p.name == from) {
                    self.clear_preview();
                }
                if let Some(dir) = self.disk_folder() {
                    self.select_after = Some(to.clone());
                    self.task = Some(Task::spawn("Renaming", ctx, move || {
                        let renamed = match vfs::owner_of(&dir) {
                            // In a game folder: under its lock, through its confined folders, never over what is at the new name.
                            Some(game) => vfs::local_rel(&dir.join(&from), &game).and_then(|rel| vfs::rename_local(&game, &rel, &to)).map_err(|e| e.to_string()),
                            None => {
                                let target = dir.join(&to);
                                // Names differing only in case are the same item for the game.
                                let clash = !from.eq_ignore_ascii_case(&to) && std::fs::symlink_metadata(&target).is_ok();
                                if clash { Err(std::io::ErrorKind::AlreadyExists.into()) } else { std::fs::rename(dir.join(&from), &target) }.map_err(|e: std::io::Error| e.to_string())
                            }
                        };
                        match renamed {
                            Ok(()) => Outcome::Done { message: format!("renamed {from} to {to}"), reload: None },
                            Err(e) => Outcome::Failed(format!("renaming {from}: {e}")),
                        }
                    }));
                    return;
                }
                let Some(location) = self.archive_folder().cloned() else { return };
                let keys = self.keys.clone();
                self.select_after = Some(to.to_ascii_lowercase());
                self.spawn_edit("Renaming", ctx, location, move |editor, folder| {
                    editor.rename(&vfs::join_inner(folder, &from), &to, keys.as_deref())?;
                    Ok(format!("renamed {from} to {}", to.to_ascii_lowercase()))
                });
            }
            Action::CreateFolder(name) => {
                if let Some(dir) = self.disk_folder() {
                    self.select_after = Some(name.clone());
                    self.task = Some(Task::spawn("Creating folder", ctx, move || {
                        let made = match vfs::owner_of(&dir) {
                            // In a game folder: under its lock, through its confined folders.
                            Some(game) => vfs::local_rel(&dir.join(&name), &game).and_then(|rel| vfs::mkdir_local(&game, &rel)).map_err(|e| e.to_string()),
                            None => std::fs::create_dir(dir.join(&name)).map_err(|e| e.to_string()),
                        };
                        match made {
                            Ok(()) => Outcome::Done { message: format!("created folder {name}"), reload: None },
                            Err(e) => Outcome::Failed(format!("creating {name}: {e}")),
                        }
                    }));
                    return;
                }
                let Some(location) = self.archive_folder().cloned() else { return };
                self.select_after = Some(name.to_ascii_lowercase());
                self.spawn_edit("Creating folder", ctx, location, move |editor, folder| {
                    editor.create_dir(&vfs::join_inner(folder, &name))?;
                    Ok(format!("created folder {}", name.to_ascii_lowercase()))
                });
            }
            Action::Delete => {
                let Some(names) = self.confirm_delete.take() else { return };
                if self.refuse_original(&names) {
                    return;
                }
                if self.preview.as_ref().is_some_and(|p| names.contains(&p.name)) {
                    self.clear_preview();
                }
                if let Some(dir) = self.disk_folder() {
                    self.task = Some(Task::spawn("Deleting", ctx, move || {
                        let deleted = match vfs::owner_of(&dir) {
                            // In a game folder: one transaction under its lock, through its confined folders. A link is
                            // removed itself, never followed, and folders go with all they hold.
                            Some(game) => names
                                .iter()
                                .map(|name| vfs::local_rel(&dir.join(name), &game))
                                .collect::<Result<Vec<_>, _>>()
                                .and_then(|rels| vfs::delete_local(&game, &rels))
                                .map_err(|e| format!("deleting: {e}")),
                            // Elsewhere, a link is removed itself too (`symlink_metadata` does not follow, and `remove_dir_all` leaves links inside alone).
                            None => names.iter().try_for_each(|name| {
                                let path = dir.join(name);
                                let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                                if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.map_err(|e| format!("deleting {name}: {e}"))
                            }),
                        };
                        match deleted {
                            Ok(()) => Outcome::Done { message: format!("{} deleted", names.len()), reload: None },
                            Err(e) => Outcome::Failed(e),
                        }
                    }));
                    return;
                }
                let Some(Location::Archive(location)) = self.browser.as_ref().map(|b| b.location.clone()) else { return };
                self.spawn_edit("Deleting", ctx, location, move |editor, folder| {
                    for name in &names {
                        editor.delete(&vfs::join_inner(folder, name))?;
                    }
                    Ok(format!("{} deleted", names.len()))
                });
            }
            Action::PeekOriginal(original) => {
                let Some(b) = &self.browser else { return };
                let location = self.original_of(&b.location);
                self.peek_original = match (&location, original) {
                    (Location::Archive(a), true) => Some(a.disk.clone()),
                    _ => None,
                };
                self.show(location);
            }
            Action::ToggleOriginals => {
                self.show_originals = !self.show_originals;
                self.clear_tree();
                if let Some(current) = self.current() {
                    let location = self.original_of(&current);
                    self.show(location);
                }
            }
            Action::ExpandAll => {
                if let Some(game) = &self.game {
                    let (root, delay) = (game.root.clone(), self.load_delay);
                    self.expand_task = Some(Task::spawn_cancellable("Opening every folder", ctx, move |cancel| subfolders(&root, cancel, delay)));
                }
            }
            Action::CollapseAll => {
                self.expand_task = None;
                self.open_dirs.clear();
            }
            Action::CancelTask => {
                // Dropping a task tells its worker to stop.
                let mut stopped = Vec::new();
                stopped.extend(self.expand_task.take().map(|t| t.label.clone()));
                if let Some((pending, task)) = self.listing_task.take() {
                    stopped.push(task.label.clone());
                    // The place shown stays, and the history as it was.
                    self.undo_history(&pending);
                }
                stopped.extend(self.preview_task.take().map(|t| t.label.clone()));
                if !stopped.is_empty() {
                    self.status = Status::Info(format!("{}: cancelled", stopped.join(", ")));
                }
            }
            Action::ShowInfo(info) => self.info = info,
            Action::CheckMods(open) => {
                self.mods_check = match (&self.game, open) {
                    (Some(game), true) => Some(ModsCheck::gather(game, self.keys.clone(), &self.ctx)),
                    _ => None,
                };
            }
            Action::ToggleMod(part, enabled) => {
                let Some(game) = self.game.clone() else { return };
                // Files are about to move: an open preview may point at one.
                self.clear_preview();
                self.task = Some(Task::spawn(if enabled { "Turning a mod on" } else { "Turning a mod off" }, ctx, move || {
                    match paleto_core::crash_hunt::set_enabled(&game, &part, enabled) {
                        Ok(()) => Outcome::Done { message: format!("{part} turned {}", if enabled { "on" } else { "off" }), reload: None },
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
            }
            Action::ScriptHook(open) => {
                self.script_hook = match (&self.game, open) {
                    (Some(game), true) => Some(ScriptHookWindow::gather(game, self.startup.latest.clone(), self.startup.online, &self.ctx)),
                    _ => None,
                };
            }
            Action::ScriptHookUpdate(release) => {
                if self.busy() {
                    self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
                    return;
                }
                let Some(game) = self.game.clone() else { return };
                let zip = self.startup.zip.clone();
                self.task = Some(Task::spawn("Updating Script Hook V", ctx, move || {
                    let result = match zip {
                        Some(zip) => paleto_core::scripthook::install_from_zip(&game, &zip),
                        None => paleto_core::scripthook::update(&game, &release),
                    };
                    match result {
                        Ok(version) => Outcome::Done { message: format!("Script Hook V updated to v{version}"), reload: None },
                        Err(e) => Outcome::Failed(format!("could not update Script Hook V: {e}")),
                    }
                }));
            }
            Action::Snapshots(open) => {
                self.snapshots = match (&self.game, open) {
                    (Some(game), true) => Some(SnapshotsWindow::gather(game)),
                    _ => None,
                };
            }
            Action::PrefixTweaks(open) => {
                self.prefix_tweaks = match (&self.game, open) {
                    (Some(game), true) => Some(PrefixWindow::gather(game)),
                    _ => None,
                };
            }
            Action::Export(open) => {
                self.export = None;
                self.export_task = None;
                let (true, Some(game)) = (open, self.game.clone()) else { return };
                let keys = self.keys.clone();
                self.export = Some(ExportWindow::new());
                // Closing the window drops the task, which stops the comparison.
                self.export_task = Some(Task::spawn_cancellable("Comparing the mods folder with the game's files", ctx, move |cancel| {
                    let stop = || cancel.is_set();
                    paleto_core::export::changed_files_cached_at(&game, keys.as_deref(), &paleto_core::export::ChangesCache::default_path(), &stop).map_err(|e| e.to_string())
                }));
            }
            Action::WriteExport(path, changes, meta) => {
                let Some(game) = self.game.clone() else { return };
                let keys = self.keys.clone();
                self.task = Some(Task::spawn("Writing the package", ctx, move || {
                    match paleto_core::export::write_oiv(&game, keys.as_deref(), &changes, &meta, &path) {
                        Ok(n) => Outcome::Done { message: format!("wrote {} ({n} changes)", path.display()), reload: None },
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
            }
            Action::CrashDetection(open) => {
                self.crash = match (&self.game, open) {
                    (Some(game), true) => Some(CrashWindow::gather(game, self.keys.clone(), ctx)),
                    _ => None,
                };
            }
            Action::TurnOff(part) => {
                if self.busy() {
                    self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
                    return;
                }
                let Some(game) = self.game.clone() else { return };
                self.task = Some(Task::spawn("Turning a plugin off", ctx, move || match paleto_core::crash_hunt::set_enabled(&game, &part, false) {
                    Ok(()) => Outcome::Done { message: format!("{part} is turned off (Crash detection can turn it back on)"), reload: None },
                    Err(e) => Outcome::Failed(e.to_string()),
                }));
                self.after_task = Some(Action::Refresh);
            }
            Action::HuntStart | Action::HuntAnswer(_) | Action::HuntStop | Action::HuntInside(_) => {
                use paleto_core::crash_hunt;
                let Some(game) = self.game.clone() else { return };
                let keys = self.keys.clone();
                // Files are about to move: an open preview may point at one.
                self.clear_preview();
                let label = match action {
                    Action::HuntStart | Action::HuntInside(_) => "Comparing the mods folder with the game's files",
                    Action::HuntStop => "Turning everything back on",
                    _ => "Turning mods off",
                };
                self.task = Some(Task::spawn(label, ctx, move || {
                    let keys = keys.as_deref();
                    let result = match action {
                        Action::HuntStart => crash_hunt::start(&game).map(Some),
                        Action::HuntAnswer(crashed) => crash_hunt::answer(&game, keys, crashed).map(Some),
                        Action::HuntInside(part) => {
                            crash_hunt::start_inside(&game, keys, &part, &paleto_core::export::ChangesCache::default_path()).map(Some)
                        }
                        _ => crash_hunt::stop(&game, keys).map(|_| None),
                    };
                    match result {
                        Ok(Some(crash_hunt::Outcome::Test(hunt))) => Outcome::Done {
                            message: format!("{} suspects left: start the game and say whether it crashed", hunt.suspects.len()),
                            reload: None,
                        },
                        Ok(Some(crash_hunt::Outcome::Found(part))) => {
                            Outcome::Done { message: format!("Found: {part} (left turned off; play to confirm)"), reload: None }
                        }
                        Ok(None) => Outcome::Done { message: "everything is turned back on".into(), reload: None },
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
                self.after_task = Some(Action::Refresh);
            }
            Action::SaveSnapshot(label) => {
                let Some(game) = self.game.clone() else { return };
                self.task = Some(Task::spawn("Saving a snapshot", ctx, move || match paleto_core::snapshots::create(&game, &label) {
                    Ok(_) => Outcome::Done { message: "snapshot of the mods folder saved".into(), reload: None },
                    Err(e) => Outcome::Failed(e.to_string()),
                }));
            }
            Action::RestoreSnapshot(id, save_first) => {
                let Some(game) = self.game.clone() else { return };
                self.clear_preview();
                self.task = Some(Task::spawn("Restoring a snapshot", ctx, move || {
                    let run = || -> Result<(), paleto_core::Error> {
                        if save_first {
                            paleto_core::snapshots::create(&game, "before restoring a snapshot")?;
                        }
                        paleto_core::snapshots::restore(&game, &id)
                    };
                    match run() {
                        Ok(()) => Outcome::Done { message: "mods folder restored from the snapshot".into(), reload: None },
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
                self.after_task = Some(Action::Refresh);
            }
            Action::DeleteSnapshot(id) => {
                let Some(game) = self.game.clone() else { return };
                self.task = Some(Task::spawn("Deleting a snapshot", ctx, move || match paleto_core::snapshots::delete(&game, &id) {
                    Ok(()) => Outcome::Done { message: "snapshot deleted".into(), reload: None },
                    Err(e) => Outcome::Failed(e.to_string()),
                }));
            }
            Action::AddDlcPacks(packs) => {
                let Some(game) = &self.game else { return };
                let (rel, inner) = paleto_core::loader::DLC_LIST;
                let (folder, name) = inner.rsplit_once('/').unwrap();
                let location = folder.split('/').fold(ArchivePath::new(game.original(rel)), |p, part| p.join(part));
                let keys = self.keys.clone();
                self.spawn_edit("Updating the DLC list", ctx, location, move |editor, folder| {
                    let inner = vfs::join_inner(folder, name);
                    let xml = editor.read(&inner, keys.as_deref())?;
                    let updated = paleto_core::loader::add_dlc_packs(&String::from_utf8_lossy(&xml), &packs)?;
                    editor.add_file(&inner, updated.into_bytes())?;
                    Ok(format!("added {} to the DLC list", packs.join(", ")))
                });
            }
            Action::ConvertMods(paths) => {
                let Some(game) = self.game.clone() else { return };
                let tool = match paleto_core::convert::converter_checked() {
                    Ok(tool) => tool,
                    Err(e) => {
                        self.status = Status::Note(format!("{} (the converter) is not installed", paleto_core::convert::TOOL), e.to_string());
                        return;
                    }
                };
                let keys = self.keys.clone();
                self.task = Some(Task::spawn("Converting to Enhanced", ctx, move || {
                    use paleto_core::convert;
                    let backup = convert::backup_dir(&game);
                    match convert::convert_in_mods(&game, keys.as_deref(), &tool, &paths, Some(&backup)) {
                        Ok(report) => {
                            let failed: Vec<String> =
                                report.iter().filter_map(|r| r.error.as_ref().map(|e| format!("{}: {e}", r.path))).collect();
                            let done = report.len() - failed.len();
                            if failed.is_empty() {
                                let what = if done == 1 { "1 file converted; the Legacy one is" } else { &format!("{done} files converted; the Legacy ones are") };
                                Outcome::Done { message: format!("{what} in {}", backup.display()), reload: None }
                            } else {
                                Outcome::Failed(format!("{done} of {} files converted; not converted: {}", report.len(), failed.join("; ")))
                            }
                        }
                        Err(e) => Outcome::Failed(e.to_string()),
                    }
                }));
            }
            Action::Refresh => self.refresh(),
            Action::Settings(settings) => {
                if settings.tree_all_files != self.settings.tree_all_files {
                    self.clear_tree();
                }
                settings.apply(ctx);
                self.settings = settings;
                if let Some(b) = &mut self.browser {
                    sort_rows(&mut b.rows, &self.settings);
                }
            }
        }
    }

}
