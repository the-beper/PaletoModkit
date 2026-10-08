//! The "Mods in game" window: whether the game will load the mods folder, and
//! what the last game session read from it.

use std::sync::Arc;

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::loader::{self, AsiPlugin, ForeignFile, HookStatus, ModdedFile, ScriptHook, Session, Setup};
use paleto_rpf::Keys;

use crate::task::Task;
use crate::theme;

/// The check of the files' formats, which reads every archive of the mods folder.
enum Formats {
    /// Nothing to check: the Legacy edition, or no keys for the encrypted archives.
    Skipped,
    /// `None` when the archives could not be read for lack of keys.
    Checking(Task<Option<Result<Vec<ForeignFile>, String>>>),
    Done(Result<Vec<ForeignFile>, String>),
}

pub struct ModsCheck {
    setup: Setup,
    session: Option<Session>,
    files: Vec<ModdedFile>,
    /// DLC packs missing from the mods copy's dlclist.xml; `None` when unknown.
    missing_dlc: Option<Result<Vec<String>, String>>,
    /// Add-on packs of the mods folder that the DLC list does not name.
    unlisted_dlc: Vec<String>,
    formats: Formats,
    /// Whether paleto-cwconv, the converter, is installed.
    can_convert: bool,
    script_hook: Option<ScriptHook>,
    /// .NET in the game's Wine prefix, for ScriptHookVDotNet mods.
    dotnet: Option<paleto_core::prefix::Report>,
    /// Files identical to the game's, once looked for.
    copies: Option<Vec<paleto_core::cleanup::Copy>>,
    plugins: Vec<AsiPlugin>,
    /// The last conversion that can be undone: its time and how many files it kept.
    last_conversion: Option<(String, usize)>,
}

pub enum Request {
    Close,
    Refresh,
    AddDlcPacks(Vec<String>),
    /// Convert these files of the mods folder to the Enhanced format.
    Convert(Vec<String>),
    /// Put back the Legacy files of this conversion.
    UndoConversion(String),
    /// Look for files identical to the game's own.
    FindCopies,
    DeleteCopies(Vec<paleto_core::cleanup::Copy>),
}

impl ModsCheck {
    pub fn gather(game: &Game, keys: Option<Arc<Keys>>, ctx: &egui::Context) -> Self {
        let session = loader::last_session(game);
        let formats = if game.edition == paleto_core::Edition::Enhanced {
            let game = game.clone();
            let keys = keys.clone();
            Formats::Checking(Task::spawn("Checking the file formats", ctx, move || match loader::foreign_files_cached_at(&game, keys.as_deref(), &loader::ForeignCache::default_path()) {
                Err(_) if keys.is_none() => None,
                result => Some(result.map_err(|e| e.to_string())),
            }))
        } else {
            Formats::Skipped
        };
        let keys = keys.as_deref();
        Self {
            formats,
            can_convert: paleto_core::convert::converter().is_some(),
            last_conversion: paleto_core::convert::backups(game).into_iter().next(),
            script_hook: loader::script_hook(game),
            dotnet: paleto_core::prefix::report(game),
            copies: None,
            plugins: loader::asi_plugins(game),
            setup: loader::setup(game),
            files: loader::modded_files(game, session.as_ref()),
            // Without keys only unencrypted archives can be compared: no error then.
            unlisted_dlc: loader::unlisted_dlc_packs(game, keys).unwrap_or_default(),
            missing_dlc: match loader::missing_dlc_packs(game, keys) {
                Ok(missing) => Some(Ok(missing)),
                Err(e) => keys.map(|_| Err(e.to_string())),
            },
            session,
        }
    }

    /// Takes in the result of the format check when it is done.
    pub fn poll(&mut self) {
        if let Formats::Checking(task) = &self.formats
            && let Some(result) = task.poll()
        {
            self.formats = result.map_or(Formats::Skipped, Formats::Done);
        }
    }

    pub fn set_copies(&mut self, copies: Vec<paleto_core::cleanup::Copy>) {
        self.copies = Some(copies);
    }

    /// Whether the format check is still running.
    pub fn checking(&self) -> bool {
        matches!(self.formats, Formats::Checking(_))
    }

    /// The window's text, for tests.
    pub fn summary(&self) -> String {
        let mut lines = vec![self.setup.problem().unwrap_or_else(|| format!("{} is ready", self.setup.plugin))];
        if let Some(Ok(missing)) = &self.missing_dlc
            && !missing.is_empty()
        {
            lines.push(format!("missing DLC packs: {}", missing.join(", ")));
        }
        if let Some(copies) = &self.copies {
            lines.push(format!("identical copies: {}", copies.len()));
        }
        if let Some(hook) = &self.script_hook {
            lines.push(format!("script hook: {:?}{}", hook.status, if hook.stale { " (stale)" } else { "" }));
        }
        if let Some(r) = &self.dotnet {
            lines.push(format!("prefix: {}", r.problem().unwrap_or_else(|| r.state.describe())));
        }
        let old: Vec<&str> = self.plugins.iter().filter(|p| p.older_than_game).map(|p| p.name.as_str()).collect();
        lines.push(format!("plugins: {} ({} older than the game)", self.plugins.len(), old.len()));
        if let Some((time, n)) = &self.last_conversion {
            lines.push(format!("last conversion: {time} ({n} files)"));
        }
        if !self.unlisted_dlc.is_empty() {
            lines.push(format!("unlisted DLC packs: {}", self.unlisted_dlc.join(", ")));
        }
        match &self.formats {
            Formats::Checking(_) => lines.push("checking the file formats".into()),
            Formats::Done(Ok(files)) if files.is_empty() => lines.push("all files are in the Enhanced format".into()),
            Formats::Done(Ok(files)) => lines.extend(files.iter().map(|f| format!("not Enhanced: {}: {}", f.path, f.problem))),
            Formats::Done(Err(e)) => lines.push(format!("could not check the file formats: {e}")),
            Formats::Skipped => {}
        }
        for f in &self.files {
            lines.push(format!("{}: {:?}", f.path, f.loaded));
        }
        lines.join("\n")
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "mods_check", "Mods in game", 520.0, |ui| self.contents(ui));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let setup = &self.setup;
        let check = |ui: &mut egui::Ui, ok: bool, text: String| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if ok { "✔" } else { "✖" }).color(if ok { theme::ok(ui.visuals().dark_mode) } else { theme::error(ui.visuals().dark_mode) }));
                ui.label(text);
            });
        };
        ui.label(RichText::new("Setup").strong().size(egui::TextStyle::Body.resolve(ui.style()).size * 1.15));
        check(
            ui,
            setup.asi_loader.is_some(),
            match &setup.asi_loader {
                Some(dll) => format!("ASI loader: {dll}"),
                None => "No ASI loader (such as dinput8.dll from Script Hook V) in the game folder".into(),
            },
        );
        check(
            ui,
            setup.plugin_installed,
            if setup.plugin_installed {
                format!("{} is installed", setup.plugin)
            } else {
                format!("{} is missing: the game ignores the mods folder without it", setup.plugin)
            },
        );
        self.script_hook_ui(ui);
        match &self.missing_dlc {
            Some(Ok(missing)) if !missing.is_empty() => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(theme::warning(ui.visuals().dark_mode)));
                    ui.label(format!(
                        "The mods copy of update.rpf predates the last game update: its DLC list lacks {}, which the game will not load.",
                        missing.join(", ")
                    ));
                });
                if ui.add(crate::widgets::primary(ui, "Add them to the mods copy's DLC list")).clicked() {
                    request = Some(Request::AddDlcPacks(missing.clone()));
                }
            }
            Some(Err(e)) => {
                ui.label(RichText::new(format!("Could not compare the DLC lists: {e}")).color(theme::warning(ui.visuals().dark_mode)));
            }
            _ => {}
        }
        if !self.unlisted_dlc.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("⚠").color(theme::warning(ui.visuals().dark_mode)));
                let (packs, it) = match self.unlisted_dlc.len() {
                    1 => ("This pack", "it"),
                    _ => ("These packs", "them"),
                };
                ui.label(format!(
                    "{packs} of the mods folder {} not in the DLC list, so the game never loads {it}: {}.",
                    if it == "it" { "is" } else { "are" },
                    self.unlisted_dlc.join(", ")
                ));
            });
            if ui.add(crate::widgets::primary(ui, "Add them to the mods copy's DLC list")).clicked() {
                request = Some(Request::AddDlcPacks(self.unlisted_dlc.clone()));
            }
        }
        if let Some(r) = self.formats_ui(ui) {
            request = Some(r);
        }

        crate::widgets::section(ui, "Clean-up");
        match &self.copies {
            None => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Copies of game files left unchanged slow loading and hide the real changes.");
                    if ui.button("Look for them").on_hover_text("Reads the copies whole: tens of GB take tens of seconds").clicked() {
                        request = Some(Request::FindCopies);
                    }
                });
            }
            Some(copies) if copies.is_empty() => {
                ui.label("No file of the mods folder is a copy of the game's left unchanged.");
            }
            Some(copies) => {
                let gb = copies.iter().map(|c| c.size).sum::<u64>() as f64 / 1e9;
                ui.horizontal_wrapped(|ui| {
                    let what = if copies.len() == 1 { "1 file".to_string() } else { format!("{} files", copies.len()) };
                    let are = if copies.len() == 1 { "is" } else { "are" };
                    ui.label(format!("{what} ({gb:.1} GB) {are} identical to the game's own: the game reads its own without them."));
                    if ui.add(crate::widgets::danger(ui, "Delete them")).on_hover_text("The game reads its own files in their place").clicked() {
                        request = Some(Request::DeleteCopies(copies.clone()));
                    }
                });
                ui.collapsing("Files", |ui| {
                    egui::ScrollArea::vertical().id_salt("copies").max_height(crate::widgets::dialog_list_height(ui.ctx()) / 2.0).show(ui, |ui| {
                        for c in copies {
                            ui.label(RichText::new(&c.path).monospace().small());
                        }
                    });
                });
            }
        }

        crate::widgets::section(ui, "Last game session");
        let Some(session) = &self.session else {
            ui.label(format!(
                "No {} log yet: start the game once, then look here again to see which modded files it read.",
                setup.plugin.trim_end_matches(".asi")
            ));
            if ui.button("Refresh").clicked() {
                request = Some(Request::Refresh);
            }
            return request;
        };
        let when = session.time.and_then(|t| t.elapsed().ok()).map(|d| format!(" ({} ago)", age(d.as_secs()))).unwrap_or_default();
        ui.label(RichText::new(format!("{}{when}", session.log.display())).weak());
        // Archives the game did not read first.
        let mut archives: Vec<&ModdedFile> = self.files.iter().filter(|f| f.loaded.is_some()).collect();
        archives.sort_by_key(|f| f.loaded);
        let others = self.files.len() - archives.len();
        if archives.is_empty() {
            ui.label("The mods folder has no archives.");
        }
        egui::ScrollArea::vertical().max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
            for f in &archives {
                let loaded = f.loaded == Some(true);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if loaded { "✔" } else { "✖" }).color(if loaded { theme::ok(ui.visuals().dark_mode) } else { theme::error(ui.visuals().dark_mode) }));
                    ui.label(&f.path);
                    if !loaded {
                        ui.label(RichText::new("not read by the game").weak());
                    }
                });
            }
        });
        if others > 0 {
            ui.label(RichText::new(format!("{others} other files (the log lists archives only)")).weak());
        }
        if !session.errors.is_empty() {
            ui.add_space(4.0);
            ui.collapsing(format!("Log errors ({})", session.errors.len()), |ui| {
                for e in &session.errors {
                    ui.label(RichText::new(e).monospace().color(theme::error(ui.visuals().dark_mode)));
                }
            });
        }
        if ui.button("Refresh").clicked() {
            request = Some(Request::Refresh);
        }
        request
    }
}

impl ModsCheck {
    /// Script Hook V and the .asi plugins: after a game update they are the
    /// usual cause of crashes at start.
    fn script_hook_ui(&self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let line = |ui: &mut egui::Ui, icon: &str, color: egui::Color32, text: String| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icon).color(color));
                ui.label(text);
            });
        };
        if let Some(hook) = &self.script_hook {
            let supports = hook.supports.as_ref().map(|v| format!(" {v}")).unwrap_or_default();
            if hook.stale {
                line(
                    ui,
                    "⚠",
                    theme::warning(dark),
                    format!("Script Hook V{supports} has not run since the game was updated: start the game to see whether it supports the new version."),
                );
            } else {
                match &hook.status {
                    HookStatus::Ok(version) => line(ui, "✔", theme::ok(dark), format!("Script Hook V{supports} works with this game version ({version})")),
                    HookStatus::Failed(why) => line(
                        ui,
                        "✖",
                        theme::error(dark),
                        format!("Script Hook V{supports} did not start, so script mods do not run: {why}. A newer Script Hook V is needed after a game update."),
                    ),
                    HookStatus::NotRun => line(ui, "•", ui.visuals().weak_text_color(), format!("Script Hook V{supports} has not run yet")),
                }
            }
        }
        if let Some(r) = &self.dotnet {
            match (r.problem(), r.ok_line()) {
                (Some(problem), _) => line(ui, "⚠", theme::warning(dark), format!("{}{}", problem[..1].to_uppercase(), &problem[1..])),
                (None, Some(ok)) => line(ui, "✔", theme::ok(dark), ok),
                (None, None) => {}
            }
        }
        if !self.plugins.is_empty() {
            let old = self.plugins.iter().filter(|p| p.older_than_game).count();
            let title = match old {
                0 => format!("ASI plugins ({})", self.plugins.len()),
                n => format!("ASI plugins ({}, {n} made before the last game update)", self.plugins.len()),
            };
            ui.collapsing(title, |ui| {
                if old > 0 {
                    ui.label(
                        RichText::new("Most plugins keep working after a game update; those that patch the game's code may not. If the game crashes at start, update or remove the ones marked first.")
                            .weak(),
                    );
                }
                for p in &self.plugins {
                    ui.horizontal(|ui| {
                        let when = p.modified.and_then(|t| t.elapsed().ok()).map(|d| format!("{} old", age(d.as_secs()))).unwrap_or_default();
                        if p.older_than_game {
                            ui.label(RichText::new("⚠").color(theme::warning(dark)));
                        }
                        ui.label(&p.name);
                        ui.label(RichText::new(when).weak());
                    });
                }
            });
        }
    }

    /// Models, textures and particles made for Legacy, which the Enhanced game cannot read.
    fn formats_ui(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        match &self.formats {
            Formats::Skipped => {}
            Formats::Checking(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking the file formats…");
                });
            }
            Formats::Done(Ok(files)) if files.is_empty() => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✔").color(theme::ok(dark)));
                    ui.label("Every model, texture and particle effect is in the Enhanced format");
                });
            }
            Formats::Done(Ok(files)) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(theme::warning(dark)));
                    let (what, it) = match files.len() {
                        1 => ("1 file in the mods folder was".to_string(), "it"),
                        n => (format!("{n} files in the mods folder were"), "them"),
                    };
                    ui.label(format!("{what} made for Legacy: the Enhanced game may crash on {it} or ignore {it}."));
                });
                if self.can_convert {
                    let label = if files.len() == 1 { "Convert it to Enhanced" } else { "Convert them to Enhanced" };
                    if ui.add(crate::widgets::primary(ui, label)).on_hover_text("With CodeWalker's converter. The Legacy files are kept in the game folder's mods_legacy folder.").clicked() {
                        request = Some(Request::Convert(files.iter().map(|f| f.path.clone()).collect()));
                    }
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{} (the converter) is not installed: convert {} with CodeWalker's Asset Converter.",
                            paleto_core::convert::TOOL,
                            if files.len() == 1 { "it" } else { "them" }
                        ))
                        .weak(),
                    );
                }
                egui::ScrollArea::vertical().id_salt("foreign-files").max_height(crate::widgets::dialog_list_height(ui.ctx()) / 2.0).show(ui, |ui| {
                    for f in files {
                        ui.add(egui::Label::new(RichText::new(&f.path).monospace()).wrap());
                        ui.label(RichText::new(&f.problem).weak());
                    }
                });
            }
            Formats::Done(Err(e)) => {
                ui.label(RichText::new(format!("Could not check the file formats: {e}")).color(theme::warning(dark)));
            }
        }
        if let Some((time, n)) = &self.last_conversion {
            let when = time
                .parse::<u64>()
                .ok()
                .and_then(|t| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t)).ok())
                .map(|d| format!(", {} ago", age(d.as_secs())))
                .unwrap_or_default();
            let files = if *n == 1 { "1 file".to_string() } else { format!("{n} files") };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("The last conversion ({files}{when}) can be undone.")).weak());
                if ui.button("Undo the last conversion").on_hover_text("Puts back the Legacy files kept in the game folder's mods_legacy folder").clicked() {
                    request = Some(Request::UndoConversion(time.clone()));
                }
            });
        }
        request
    }
}

/// "3 min", "2 h", "5 days".
pub(crate) fn age(secs: u64) -> String {
    match secs {
        0..120 => format!("{secs} s"),
        120..7200 => format!("{} min", secs / 60),
        7200..172_800 => format!("{} h", secs / 3600),
        _ => format!("{} days", secs / 86400),
    }
}
