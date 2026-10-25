//! The "Mods in game" window: a banner saying whether the game will load the
//! mods, the inventory of the mods, and the maintenance tools.

use std::sync::Arc;

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::loader::{self, ForeignFile, HookStatus, ModEntry, ModKind, ModStatus, ScriptHook, Session, Setup};
use paleto_core::scripthook::Compat;
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

/// Which mods the inventory shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filter {
    All,
    Plugins,
    Scripts,
    Packs,
    Archives,
}

impl Filter {
    const ALL: [(Filter, &'static str); 5] =
        [(Filter::All, "All"), (Filter::Plugins, "Plugins"), (Filter::Scripts, "Scripts"), (Filter::Packs, "Add-on packs"), (Filter::Archives, "Archives")];

    fn accepts(self, kind: ModKind) -> bool {
        matches!(
            (self, kind),
            (Filter::All, _) | (Filter::Plugins, ModKind::Plugin) | (Filter::Scripts, ModKind::Script) | (Filter::Packs, ModKind::AddonPack) | (Filter::Archives, ModKind::Archive)
        )
    }
}

/// What fixes a setup problem.
enum Fix {
    AddDlcPacks(Vec<String>),
    OpenScriptHook,
    PrefixTweaks,
    Convert(Vec<String>),
}

/// A reason why the game may not load the mods, and what to do about it.
struct Problem {
    text: String,
    fix: Option<Fix>,
}

pub struct ModsCheck {
    game: Game,
    setup: Setup,
    session: Option<Session>,
    /// DLC packs missing from the mods copy's dlclist.xml; `None` when unknown.
    missing_dlc: Option<Result<Vec<String>, String>>,
    /// Add-on packs of the mods folder that the DLC list does not name.
    unlisted_dlc: Vec<String>,
    formats: Formats,
    /// Whether paleto-cwconv, the converter, is installed.
    can_convert: bool,
    script_hook: Option<ScriptHook>,
    compat: Compat,
    /// .NET in the game's Wine prefix, for ScriptHookVDotNet mods.
    dotnet: Option<paleto_core::prefix::Report>,
    /// Files identical to the game's, once looked for.
    copies: Option<Vec<paleto_core::cleanup::Copy>>,
    /// The last conversion that can be undone: its time and how many files it kept.
    last_conversion: Option<(String, usize)>,
    inventory: Vec<ModEntry>,
    filter: Filter,
    /// A crash hunt owns the turned off mods: they cannot be switched meanwhile.
    hunting: bool,
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
    /// Turn a mod (its part path) on or off.
    Toggle(String, bool),
    OpenScriptHook,
    PrefixTweaks,
}

fn filter_id() -> egui::Id {
    egui::Id::unique("mods-check-filter")
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
        // Without keys only unencrypted archives can be compared: no error then.
        let unlisted_dlc = loader::unlisted_dlc_packs(game, keys).unwrap_or_default();
        let filter = ctx.data(|d| d.get_temp::<u8>(filter_id())).and_then(|i| Filter::ALL.get(usize::from(i))).map_or(Filter::All, |f| f.0);
        let mut check = Self {
            game: game.clone(),
            formats,
            can_convert: paleto_core::convert::converter().is_some(),
            last_conversion: paleto_core::convert::backups(game).into_iter().next(),
            script_hook: loader::script_hook(game),
            compat: crate::scripthook_window::compat_now(game),
            dotnet: paleto_core::prefix::report(game),
            copies: None,
            setup: loader::setup(game),
            inventory: Vec::new(),
            filter,
            hunting: paleto_core::crash_hunt::load(game).is_some(),
            unlisted_dlc,
            missing_dlc: match loader::missing_dlc_packs(game, keys) {
                Ok(missing) => Some(Ok(missing)),
                Err(e) => keys.map(|_| Err(e.to_string())),
            },
            session,
        };
        check.rebuild();
        check
    }

    /// Builds the inventory from what is known now (the format check may still be running).
    fn rebuild(&mut self) {
        self.inventory = loader::inventory(&self.game, self.session.as_ref(), &self.unlisted_dlc, self.foreign());
    }

    fn foreign(&self) -> &[ForeignFile] {
        match &self.formats {
            Formats::Done(Ok(files)) => files,
            _ => &[],
        }
    }

    /// Takes in the result of the format check when it is done.
    pub fn poll(&mut self) {
        if let Formats::Checking(task) = &self.formats
            && let Some(result) = task.poll()
        {
            self.formats = result.map_or(Formats::Skipped, Formats::Done);
            self.rebuild();
        }
    }

    pub fn set_copies(&mut self, copies: Vec<paleto_core::cleanup::Copy>) {
        self.copies = Some(copies);
    }

    /// Whether the format check is still running.
    pub fn checking(&self) -> bool {
        matches!(self.formats, Formats::Checking(_))
    }

    /// The reasons why the game may not load the mods, the most important first.
    fn problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        if let Some(problem) = self.setup.problem() {
            out.push(Problem { text: capital(&problem), fix: None });
        }
        if let Compat::Incompatible { supports, game } = &self.compat {
            out.push(Problem {
                text: format!("Script Hook V supports {supports} but the game is {game}: script mods will not run until a newer Script Hook V is installed."),
                fix: Some(Fix::OpenScriptHook),
            });
        }
        match &self.missing_dlc {
            Some(Ok(missing)) if !missing.is_empty() => out.push(Problem {
                text: format!("The mods copy of update.rpf predates the last game update: its DLC list lacks {}, which the game will not load.", missing.join(", ")),
                fix: Some(Fix::AddDlcPacks(missing.clone())),
            }),
            Some(Err(e)) => out.push(Problem { text: format!("Could not compare the DLC lists: {e}"), fix: None }),
            _ => {}
        }
        if !self.unlisted_dlc.is_empty() {
            let (packs, it, is) = if self.unlisted_dlc.len() == 1 { ("This pack", "it", "is") } else { ("These packs", "them", "are") };
            out.push(Problem {
                text: format!("{packs} of the mods folder {is} not in the DLC list, so the game never loads {it}: {}.", self.unlisted_dlc.join(", ")),
                fix: Some(Fix::AddDlcPacks(self.unlisted_dlc.clone())),
            });
        }
        if let Some(problem) = self.dotnet.as_ref().and_then(|r| r.problem()) {
            out.push(Problem { text: capital(&problem), fix: Some(Fix::PrefixTweaks) });
        }
        match &self.formats {
            Formats::Done(Ok(files)) if !files.is_empty() => {
                let (what, it) = match files.len() {
                    1 => ("1 file in the mods folder was".to_string(), "it"),
                    n => (format!("{n} files in the mods folder were"), "them"),
                };
                out.push(Problem {
                    text: format!("{what} made for Legacy: the Enhanced game may crash on {it} or ignore {it}."),
                    fix: self.can_convert.then(|| Fix::Convert(files.iter().map(|f| f.path.clone()).collect())),
                });
            }
            Formats::Done(Err(e)) => out.push(Problem { text: format!("Could not check the file formats: {e}"), fix: None }),
            _ => {}
        }
        out
    }

    /// The files of the mods folder inside this mod that are not in the Enhanced format.
    fn foreign_of(&self, entry: &ModEntry) -> Vec<String> {
        let lower = entry.part.to_ascii_lowercase();
        let Some(dir) = lower.strip_prefix("mods/") else { return Vec::new() };
        let prefix = format!("{dir}/");
        self.foreign().iter().filter(|f| f.path.to_ascii_lowercase() == dir || f.path.to_ascii_lowercase().starts_with(&prefix)).map(|f| f.path.clone()).collect()
    }

    /// The window's text, for tests.
    pub fn summary(&self) -> String {
        let problems = self.problems();
        let mut lines = vec![match problems.first() {
            Some(p) => format!("banner: {}", p.text),
            None => "banner: The game will load your mods".into(),
        }];
        lines.extend(problems.iter().skip(1).map(|p| format!("problem: {}", p.text)));
        if let Some(Ok(missing)) = &self.missing_dlc
            && !missing.is_empty()
        {
            lines.push(format!("missing DLC packs: {}", missing.join(", ")));
        }
        if !self.unlisted_dlc.is_empty() {
            lines.push(format!("unlisted DLC packs: {}", self.unlisted_dlc.join(", ")));
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
        if let Some((time, n)) = &self.last_conversion {
            lines.push(format!("last conversion: {time} ({n} files)"));
        }
        match &self.formats {
            Formats::Checking(_) => lines.push("checking the file formats".into()),
            Formats::Done(Ok(files)) if files.is_empty() => lines.push("all files are in the Enhanced format".into()),
            Formats::Done(Err(e)) => lines.push(format!("could not check the file formats: {e}")),
            _ => {}
        }
        if self.inventory.is_empty() {
            lines.push("No mods found in the game folder".into());
        }
        for e in &self.inventory {
            let statuses: Vec<String> = e.status.iter().map(status_text).collect();
            lines.push(if statuses.is_empty() { format!("{}: {}", e.name, kind_name(e.kind)) } else { format!("{}: {}, {}", e.name, kind_name(e.kind), statuses.join(", ")) });
        }
        lines.join("\n")
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "mods_check", "Mods in game", 620.0, |ui| self.contents(ui));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        let problems = self.problems();
        // The banner: all is well, or the first problem with its fix.
        let color = if problems.is_empty() { theme::ok(dark) } else { theme::error(dark) };
        egui::Frame::new().fill(color.gamma_multiply(0.15)).stroke(egui::Stroke::new(1.0, color)).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match problems.first() {
                None => {
                    ui.label(RichText::new("✔ The game will load your mods").strong().color(color));
                }
                Some(p) => {
                    ui.label(RichText::new(format!("✖ {}", p.text)).strong().color(color));
                    request = fix_button(ui, p).or(request.take());
                }
            }
        });
        for p in problems.iter().skip(1) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("⚠").color(theme::warning(dark)));
                ui.label(&p.text);
            });
            request = fix_button(ui, p).or(request.take());
        }
        if self.checking() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking the file formats…");
            });
        }
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new("Setup").strong()).default_open(!problems.is_empty()).show(ui, |ui| self.setup_ui(ui));

        crate::widgets::section(ui, "Mods");
        ui.horizontal_wrapped(|ui| {
            for (filter, label) in Filter::ALL {
                if ui.selectable_label(self.filter == filter, label).clicked() {
                    self.filter = filter;
                    let index = Filter::ALL.iter().position(|f| f.0 == filter).unwrap_or(0) as u8;
                    ui.ctx().data_mut(|d| d.insert_temp(filter_id(), index));
                }
            }
        });
        ui.add_space(4.0);
        if let Some(r) = self.inventory_ui(ui) {
            request = Some(r);
        }
        if self.session.is_none() {
            let log = self.setup.plugin.trim_end_matches(".asi");
            crate::widgets::hint(ui, format!("No {log} log yet: start the game once, then refresh to see which mods it reads."));
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Maintenance").strong()).show(ui, |ui| {
            if let Some(r) = self.maintenance_ui(ui) {
                request = Some(r);
            }
        });
        ui.add_space(6.0);
        if ui.button("Refresh").clicked() {
            request = Some(Request::Refresh);
        }
        request
    }

    /// The facts behind the banner: the loader, Script Hook V and .NET.
    fn setup_ui(&self, ui: &mut egui::Ui) {
        let setup = &self.setup;
        let check = |ui: &mut egui::Ui, ok: bool, text: String| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(if ok { "✔" } else { "✖" }).color(if ok { theme::ok(ui.visuals().dark_mode) } else { theme::error(ui.visuals().dark_mode) }));
                ui.label(text);
            });
        };
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
    }

    fn inventory_ui(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        if self.inventory.is_empty() {
            ui.label("No mods found in the game folder");
            return None;
        }
        let shown: Vec<&ModEntry> = self.inventory.iter().filter(|e| self.filter.accepts(e.kind)).collect();
        if shown.is_empty() {
            crate::widgets::hint(ui, "Nothing of this kind.");
        }
        egui::ScrollArea::vertical().id_salt("inventory").max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
            egui::Grid::new("inventory-rows").num_columns(3).spacing([14.0, 4.0]).show(ui, |ui| {
                for entry in shown {
                    let mut on = entry.enabled;
                    let toggle = ui.add_enabled(!self.hunting, egui::Checkbox::new(&mut on, entry.name.as_str())).on_disabled_hover_text("A crash hunt is in progress: finish or stop it first (Tools > Crash detection)");
                    if toggle.changed() {
                        request = Some(Request::Toggle(entry.part.clone(), on));
                    }
                    ui.label(RichText::new(kind_name(entry.kind)).weak());
                    ui.horizontal_wrapped(|ui| {
                        let foreign = self.foreign_of(entry);
                        for status in entry.status.iter().take(3) {
                            ui.label(RichText::new(status_text(status)).small().color(status_color(status, dark)));
                        }
                        let more = entry.status.len().saturating_sub(3);
                        if more > 0 {
                            ui.label(RichText::new(format!("and {more} more")).small().weak());
                        }
                        if entry.status.contains(&ModStatus::NotInDlcList) && ui.small_button("Add to DLC list").clicked() {
                            request = Some(Request::AddDlcPacks(vec![entry.name.clone()]));
                        }
                        if !foreign.is_empty() && self.can_convert && ui.small_button("Convert to Enhanced").on_hover_text("The Legacy files are kept in the game folder's mods_legacy folder").clicked() {
                            request = Some(Request::Convert(foreign));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        request
    }

    /// Conversion, identical copies and the game's log.
    fn maintenance_ui(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        match &self.formats {
            Formats::Done(Ok(files)) if !files.is_empty() => {
                if self.can_convert {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} not in the Enhanced format.", if files.len() == 1 { "1 file is".to_string() } else { format!("{} files are", files.len()) }));
                        if ui.add(crate::widgets::primary(ui, "Convert all to Enhanced")).on_hover_text("With CodeWalker's converter. The Legacy files are kept in the game folder's mods_legacy folder.").clicked() {
                            request = Some(Request::Convert(files.iter().map(|f| f.path.clone()).collect()));
                        }
                    });
                } else {
                    crate::widgets::hint(ui, format!("{} (the converter) is not installed: convert the Legacy files with CodeWalker's Asset Converter.", paleto_core::convert::TOOL));
                }
                ui.collapsing("Legacy files", |ui| {
                    egui::ScrollArea::vertical().id_salt("foreign-files").max_height(crate::widgets::dialog_list_height(ui.ctx()) / 2.0).show(ui, |ui| {
                        for f in files {
                            ui.add(egui::Label::new(RichText::new(&f.path).monospace()).wrap());
                            ui.label(RichText::new(&f.problem).weak());
                        }
                    });
                });
            }
            Formats::Done(Ok(_)) => {
                ui.label("Every model, texture and particle effect is in the Enhanced format");
            }
            _ => {}
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
        ui.add_space(4.0);
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
        if let Some(session) = &self.session {
            ui.add_space(4.0);
            let when = session.time.and_then(|t| t.elapsed().ok()).map(|d| format!(" ({} ago)", age(d.as_secs()))).unwrap_or_default();
            ui.label(RichText::new(format!("{}{when}", session.log.display())).weak());
            if !session.errors.is_empty() {
                ui.collapsing(format!("Log errors ({})", session.errors.len()), |ui| {
                    for e in &session.errors {
                        ui.label(RichText::new(e).monospace().color(theme::error(ui.visuals().dark_mode)));
                    }
                });
            }
        }
        request
    }
}

/// The button that fixes a problem, if it has one.
fn fix_button(ui: &mut egui::Ui, problem: &Problem) -> Option<Request> {
    let fix = problem.fix.as_ref()?;
    let (label, request) = match fix {
        Fix::AddDlcPacks(packs) => ("Add to DLC list", Request::AddDlcPacks(packs.clone())),
        Fix::OpenScriptHook => ("Open Script Hook V", Request::OpenScriptHook),
        Fix::PrefixTweaks => ("Prefix tweaks…", Request::PrefixTweaks),
        Fix::Convert(paths) => (if paths.len() == 1 { "Convert it to Enhanced" } else { "Convert them to Enhanced" }, Request::Convert(paths.clone())),
    };
    ui.add(crate::widgets::primary(ui, label)).clicked().then_some(request)
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn kind_name(kind: ModKind) -> &'static str {
    match kind {
        ModKind::Plugin => "plugin",
        ModKind::Script => "script",
        ModKind::AddonPack => "add-on pack",
        ModKind::Archive => "archive",
    }
}

fn status_text(status: &ModStatus) -> String {
    match status {
        ModStatus::Read => "Read in the last session".into(),
        ModStatus::NotRead => "Not read by the game".into(),
        ModStatus::NotInDlcList => "Not in the DLC list".into(),
        ModStatus::OlderThanGame => "Older than the game update".into(),
        ModStatus::TurnedOff => "Turned off".into(),
        ModStatus::NotEnhanced(problem) => format!("Not Enhanced: {problem}"),
    }
}

fn status_color(status: &ModStatus, dark: bool) -> egui::Color32 {
    match status {
        ModStatus::Read => theme::ok(dark),
        ModStatus::NotRead | ModStatus::NotInDlcList => theme::error(dark),
        ModStatus::OlderThanGame | ModStatus::NotEnhanced(_) => theme::warning(dark),
        ModStatus::TurnedOff => if dark { egui::Color32::GRAY } else { egui::Color32::DARK_GRAY },
    }
}

impl ModsCheck {
    /// Script Hook V and .NET: after a game update they are the usual cause of
    /// crashes at start.
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
