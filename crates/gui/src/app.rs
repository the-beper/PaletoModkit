use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use paleto_core::shared_textures::SharedTextures;
use paleto_core::{ArchivePath, Game, game, vfs};
use paleto_rpf::Keys;

use crate::actions::Action;
use crate::mods_check::{self, ModsCheck};
use crate::prefix_window::PrefixWindow;
use crate::notices::{self, Startup};
use crate::scripthook_window::{self, ScriptHookWindow};
use crate::snapshots_window::{self, SnapshotsWindow};
use crate::crash_window::{self, CrashWindow};
use crate::export_window::{self, ExportWindow};
use crate::settings::Settings;
use crate::task::Task;
use crate::theme::{self, SystemTheme};
use crate::dialogs::{AddonDialog, InstallDialog, LegacyAdd, NameDialog, Outcome, TextureBatchDialog, TextureDialog, desktop_notify, install_summary};
use crate::find::Find;
use crate::browser::{AfterListing, Browser, FsEntry, HistoryMove, Listed, PendingListing, TreeRead, mark_changes};
use crate::preview::{Loaded, Preview};

/// Text files up to this size can be edited in the preview panel.
pub(crate) const MAX_TEXT: usize = 8 << 20;
/// The buttons at the right end of the menu and location bars, one width
/// for all their labels so they line up and do not change size.
pub(crate) const INSTALL: &str = "📦 Install package…";
pub(crate) const INSTALL_SHORT: &str = "📦 Install…";
pub(crate) const TWEAKS: &str = "🛠 Prefix tweaks…";
pub(crate) const TWEAKS_SHORT: &str = "🛠 Tweaks…";
pub(crate) const ORIGINAL: &str = "↩ Show original";
pub(crate) const MODS_COPY: &str = "↪ Show mods copy";

pub(crate) fn right_width(ui: &egui::Ui) -> f32 {
    let padding = 2.0 * ui.ctx().global_style().spacing.button_padding.x;
    [INSTALL, TWEAKS, ORIGINAL, MODS_COPY]
        .iter()
        .map(|t| egui::WidgetText::from(*t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + padding)
        .fold(0.0, f32::max)
}

/// Marks a game file that has a modified copy in the mods folder.
pub(crate) const MODS_LINK: &str = "↪";
pub(crate) const MODS_LINK_HOVER: &str = "The game loads the modified copy of this file in the mods folder";

pub(crate) const REPO_URL: &str = "https://github.com/the-beper/PaletoModkit";

/// What the main view shows: a folder on disk or a folder inside an archive.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    Folder(PathBuf),
    Archive(ArchivePath),
}

/// The entries of a folder that differ from the original game files, by lowercase name.
pub(crate) type Changes = HashMap<String, vfs::Change>;
type Comparison = Task<Result<Changes, String>>;

pub struct App {
    pub(crate) game: Option<Game>,
    pub(crate) installs: Vec<Game>,
    pub(crate) keys: Option<Arc<Keys>>,
    pub(crate) keys_task: Option<Task<Result<(Keys, SharedTextures), String>>>,
    /// The check for files made for Legacy in the mods folder, run once the
    /// keys are ready (Enhanced edition): how many it found.
    legacy_scan: Option<Task<Result<usize, String>>>,
    /// Files of the mods folder made for Legacy, as the last scan found them.
    legacy_files: usize,
    /// The search for copies of game files left unchanged in the mods folder.
    pub(crate) copies_task: Option<Task<Vec<paleto_core::cleanup::Copy>>>,
    /// The location being listed; the one shown stays until it is ready.
    pub(crate) listing_task: Option<(PendingListing, Task<Listed>)>,
    /// The preview being read; the one shown stays until it is ready.
    pub(crate) preview_task: Option<Task<Loaded>>,
    /// The texture or sound to select in the next preview loaded.
    pub(crate) preview_select: Option<String>,
    /// Expand all: the folders found, `None` once cancelled.
    pub(crate) expand_task: Option<Task<Option<Vec<PathBuf>>>>,
    /// Tests make background loads this slow, to see them while they run.
    pub(crate) load_delay: std::time::Duration,
    /// Where the dictionaries that models share are (vehshare.ytd...).
    pub(crate) shared: Arc<SharedTextures>,
    pub(crate) settings: Settings,
    /// Folder listings for the side tree, cached until the next refresh.
    pub(crate) tree: HashMap<PathBuf, Vec<FsEntry>>,
    /// Side tree folders asked for and not read yet; those of `tree_queue` wait for `tree_task`.
    pub(crate) tree_loading: HashSet<PathBuf>,
    pub(crate) tree_queue: Vec<PathBuf>,
    pub(crate) tree_task: Option<Task<TreeRead>>,
    /// Expand the side tree down to the current location on the next frame.
    pub(crate) reveal: bool,
    /// Folders expanded in the side tree.
    pub(crate) open_dirs: BTreeSet<PathBuf>,
    /// Help or About window being shown.
    pub(crate) info: Option<Info>,
    /// Comparing the folder being shown (in a mods copy) with the original.
    /// For background work started outside a frame.
    pub(crate) ctx: egui::Context,
    /// Files handed over by later launches of the app.
    #[cfg(target_os = "linux")]
    pub(crate) instance: Option<crate::single_instance::Instance>,
    /// Raises the window when a later launch asks for it.
    #[cfg(target_os = "linux")]
    pub(crate) activator: Option<crate::wayland_activation::Activator>,
    /// The comparison running, and whether it has the keys. Without them encrypted
    /// originals cannot be read and every file looks added: such a result is not cached.
    pub(crate) changes_task: Option<(Location, bool, Comparison)>,
    /// Comparisons done, by folder and the archive's modification time.
    pub(crate) changes_cache: Vec<(Location, Option<std::time::SystemTime>, Arc<Changes>)>,
    /// The desktop's light or dark preference, for the "same as the system" theme.
    system_theme: SystemTheme,
    pub(crate) mods_check: Option<ModsCheck>,
    pub(crate) script_hook: Option<ScriptHookWindow>,
    /// The checks made at start and the notices they bring.
    pub(crate) startup: Startup,
    /// Changes an earlier run left interrupted in the chosen game: mutating actions are refused until they are settled.
    pub(crate) interrupted: Vec<paleto_core::txn::Pending>,
    pub(crate) snapshots: Option<SnapshotsWindow>,
    pub(crate) prefix_tweaks: Option<PrefixWindow>,
    pub(crate) crash: Option<CrashWindow>,
    pub(crate) export: Option<ExportWindow>,
    /// The comparison of the mods folder for the export window.
    pub(crate) export_task: Option<Task<Result<Vec<paleto_core::export::Changed>, String>>>,
    pub(crate) browser: Option<Browser>,
    pub(crate) back: Vec<Location>,
    pub(crate) forward: Vec<Location>,
    pub(crate) show_originals: bool,
    /// An original archive shown as it is although it has a mods copy (the
    /// location bar's Show original), until the browser leaves it.
    pub(crate) peek_original: Option<PathBuf>,
    pub(crate) preview: Option<Preview>,
    /// Reload the preview after the running edit, selecting this texture if present.
    pub(crate) refresh_preview: Option<String>,
    pub(crate) install: Option<InstallDialog>,
    pub(crate) confirm_delete: Option<Vec<String>>,
    /// Files about to be added, some of them made for Legacy.
    pub(crate) legacy_add: Option<LegacyAdd>,
    /// Add-on packs about to be installed.
    pub(crate) addon_dialog: Option<AddonDialog>,
    /// Asking for a name: renaming an item or creating a folder.
    pub(crate) name_dialog: Option<NameDialog>,
    /// Confirming a change to the previewed textures, or completing it (name, format).
    pub(crate) texture_dialog: Option<TextureDialog>,
    pub(crate) texture_batch: Option<TextureBatchDialog>,
    /// The single selected file last seen, for the preview to follow.
    followed: Option<String>,
    /// Select this item once the running edit has reloaded the folder.
    pub(crate) select_after: Option<String>,
    pub(crate) task: Option<Task<Outcome>>,
    /// The previewed text being saved (file name, text): marked saved once
    /// the save succeeds, so a failed save keeps the edits unsaved.
    pub(crate) saving: Option<(String, String)>,
    /// The window was asked to close while a task ran: a second request closes it.
    close_while_busy: bool,
    /// Runs when the current task finishes (e.g. navigating after a save).
    pub(crate) after_task: Option<Action>,
    /// An action waiting for the user to save or discard the edited file.
    pub(crate) pending: Option<Action>,
    /// The window may close without asking (unsaved changes were handled).
    pub(crate) allow_close: bool,
    /// Drops reported by the Wayland connection (winit has none on Wayland).
    #[cfg(target_os = "linux")]
    wayland_drops: Option<std::sync::mpsc::Receiver<crate::wayland_dnd::DndEvent>>,
    /// Files are being dragged over the window (Wayland).
    drag_hover: bool,
    pub(crate) status: Status,
}


pub(crate) enum Status {
    Info(String),
    /// A message with more on hover.
    Note(String, String),
    Error(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Info {
    GettingStarted,
    Shortcuts,
    About,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let mut app = App {
            game: None,
            installs: game::detect_installs(),
            keys: None,
            keys_task: None,
            legacy_scan: None,
            legacy_files: 0,
            copies_task: None,
            expand_task: None,
            preview_task: None,
            listing_task: None,
            preview_select: None,
            load_delay: std::time::Duration::ZERO,
            shared: Arc::default(),
            settings: Settings::default(),
            tree: HashMap::new(),
            tree_loading: HashSet::new(),
            tree_queue: Vec::new(),
            tree_task: None,
            reveal: false,
            open_dirs: BTreeSet::new(),
            info: None,
            system_theme: SystemTheme::watch(&cc.egui_ctx),
            ctx: cc.egui_ctx.clone(),
            #[cfg(target_os = "linux")]
            instance: None,
            #[cfg(target_os = "linux")]
            activator: crate::wayland_activation::Activator::new(cc),
            changes_task: None,
            changes_cache: Vec::new(),
            mods_check: None,
            script_hook: None,
            startup: Startup::default(),
            interrupted: Vec::new(),
            snapshots: None,
            prefix_tweaks: None,
            crash: None,
            export: None,
            export_task: None,
            browser: None,
            back: Vec::new(),
            forward: Vec::new(),
            show_originals: false,
            peek_original: None,
            preview: None,
            refresh_preview: None,
            install: None,
            confirm_delete: None,
            legacy_add: None,
            addon_dialog: None,
            name_dialog: None,
            texture_dialog: None,
            texture_batch: None,
            followed: None,
            select_after: None,
            task: None,
            saving: None,
            close_while_busy: false,
            after_task: None,
            pending: None,
            allow_close: false,
            #[cfg(target_os = "linux")]
            wayland_drops: crate::wayland_dnd::listen(cc, &cc.egui_ctx),
            drag_hover: false,
            status: Status::Info(String::new()),
        };
        match cc.storage {
            Some(storage) => app.restore(storage, &cc.egui_ctx),
            None => app.settings.apply(&cc.egui_ctx),
        }
        let saved = cc.storage.and_then(|s| s.get_string("game")).map(PathBuf::from);
        let initial = saved.and_then(|p| Game::open(&p).ok()).or_else(|| (app.installs.len() == 1).then(|| app.installs[0].clone()));
        if let Some(game) = initial {
            app.select_game(game, &cc.egui_ctx);
        }
        app
    }

    /// Restores the preferences and, if enabled, the open folders saved by
    /// the last session.
    pub fn restore(&mut self, storage: &dyn eframe::Storage, ctx: &egui::Context) {
        self.settings = storage.get_string("settings").map(|s| Settings::from_storage(&s)).unwrap_or_default();
        self.settings.apply(ctx);
        self.startup.last_seen_crash = storage.get_string("last_seen_crash").and_then(|s| s.parse().ok());
        if self.settings.remember_open_folders {
            let saved = storage.get_string("open_dirs").unwrap_or_default();
            self.open_dirs = saved.lines().map(PathBuf::from).filter(|p| p.is_dir()).collect();
        }
    }

    // ---- API used by tests -------------------------------------------------

    /// Folders expanded in the side tree.
    pub fn open_folders(&self) -> Vec<PathBuf> {
        self.open_dirs.iter().cloned().collect()
    }

    /// Whether keys are loaded and no background work is running.
    pub fn idle(&self) -> bool {
        self.keys_task.is_none()
            && self.task.is_none()
            && self.changes_task.is_none()
            && self.legacy_scan.is_none()
            && self.copies_task.is_none()
            && self.export_task.is_none()
            && self.expand_task.is_none()
            && self.preview_task.is_none()
            && self.listing_task.is_none()
            && self.tree_task.is_none()
            && !self.startup.running()
            && !self.mods_check.as_ref().is_some_and(ModsCheck::checking)
            && !self.crash.as_ref().is_some_and(CrashWindow::loading)
    }

    /// Makes background loads this slow, so tests can see them running.
    #[doc(hidden)]
    pub fn delay_loads(&mut self, delay: std::time::Duration) {
        self.load_delay = delay;
    }

    /// The names marked as differing from the original game files, and
    /// whether each is new.
    pub fn changed_names(&self) -> Vec<(String, bool)> {
        self.browser.iter().flat_map(|b| b.rows.iter().filter(|r| r.modded).map(|r| (r.name.clone(), r.added))).collect()
    }

    /// Goes to a folder or archive, recording it in the history.
    pub fn go(&mut self, location: Location) {
        self.go_then(location, AfterListing::default());
    }

    pub(crate) fn go_then(&mut self, location: Location, after: AfterListing) {
        let mut history = None;
        if let Some(current) = self.current()
            && current != location
        {
            self.back.push(current);
            self.forward.clear();
            history = Some(HistoryMove::Go);
        }
        self.reveal = true;
        self.show_moving(location, after, history);
    }

    /// Where the user is: the location being listed, else the one shown.
    pub(crate) fn current(&self) -> Option<Location> {
        match &self.listing_task {
            Some((pending, _)) => Some(pending.requested.clone()),
            None => self.browser.as_ref().map(|b| self.opened(b)),
        }
    }

    /// Opens an archive location.
    pub fn open(&mut self, location: ArchivePath) {
        self.go(Location::Archive(location));
    }

    pub fn location(&self) -> Option<&Location> {
        self.browser.as_ref().map(|b| &b.location)
    }

    /// Names in the current folder, in display order.
    pub fn names(&self) -> Vec<String> {
        self.browser.iter().flat_map(|b| b.rows.iter().map(|r| r.name.clone())).collect()
    }

    pub fn selected(&self) -> Vec<String> {
        self.browser.iter().flat_map(|b| b.selected.iter().cloned()).collect()
    }

    pub fn filter(&self) -> &str {
        self.browser.as_ref().map_or("", |b| b.filter.as_str())
    }


    pub fn add_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        self.apply(Action::Add(paths), ctx);
    }

    pub fn ask_delete(&mut self, names: &[&str]) {
        self.confirm_delete = Some(names.iter().map(|n| n.to_string()).collect());
    }

    pub fn delete_now(&mut self, names: &[&str], ctx: &egui::Context) {
        self.ask_delete(names);
        self.apply(Action::Delete, ctx);
    }

    pub fn cancel_dialogs(&mut self) {
        self.confirm_delete = None;
        self.name_dialog = None;
        self.legacy_add = None;
        self.addon_dialog = None;
        self.install = None;
    }

    /// Opens the "Mods in game" window.
    pub fn check_mods(&mut self, ctx: &egui::Context) {
        self.apply(Action::CheckMods(true), ctx);
    }

    /// The "Mods in game" window's findings, while it is open.
    /// The notes of the snapshots listed in the "Snapshots" window, if open.
    pub fn snapshot_labels(&self) -> Option<Vec<String>> {
        self.snapshots.as_ref().map(SnapshotsWindow::labels)
    }

    /// What the "Crash detection" window shows (verdict, causes, hunt state), if open.
    pub fn crash_detection_summary(&self) -> Option<String> {
        self.crash.as_ref().map(CrashWindow::summary)
    }

    pub fn open_crash_detection(&mut self, ctx: &egui::Context) {
        self.apply(Action::CrashDetection(true), ctx);
    }

    pub fn open_snapshots(&mut self, ctx: &egui::Context) {
        self.apply(Action::Snapshots(true), ctx);
    }

    pub fn open_prefix_tweaks(&mut self, ctx: &egui::Context) {
        self.apply(Action::PrefixTweaks(true), ctx);
    }

    /// What the "Prefix tweaks" window found, while it is open.
    pub fn prefix_tweaks_summary(&self) -> Option<String> {
        self.prefix_tweaks.as_ref().map(PrefixWindow::summary)
    }

    /// Whether to look on the internet (for Script Hook V updates); off unless the real app turns it on.
    pub fn set_online_checks(&mut self, online: bool) {
        self.startup.online = online;
    }

    /// Checks made at start (news about Script Hook V and the last crash), in the background:
    /// now if a game is chosen, else once one is.
    pub fn startup_checks(&mut self, _ctx: &egui::Context) {
        self.startup.want();
        self.startup.begin(self.game.as_ref(), &self.ctx);
    }

    /// Tells the app of the latest Script Hook V release, as if it had been found online.
    pub fn offer_script_hook(&mut self, release: paleto_core::scripthook::Release) {
        self.startup.offer_release(self.game.as_ref(), release);
    }

    /// Tells the app of a crash the user has not been told about.
    pub fn offer_crash(&mut self, time: std::time::SystemTime, verdict: &str) {
        self.startup.offer_crash(time, verdict.to_string());
    }


    pub fn mods_check_summary(&self) -> Option<String> {
        self.mods_check.as_ref().map(ModsCheck::summary)
    }

    /// The error shown in the status bar, if any.
    pub fn status_error(&self) -> Option<&str> {
        match &self.status {
            Status::Error(e) => Some(e),
            Status::Info(_) | Status::Note(..) => None,
        }
    }

    pub fn select_game_folder(&mut self, path: &Path, ctx: &egui::Context) {
        self.apply(Action::SelectGame(path.to_path_buf()), ctx);
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings, ctx: &egui::Context) {
        self.apply(Action::Settings(settings), ctx);
    }

    // ---- State changes -----------------------------------------------------

    pub(crate) fn select_game(&mut self, game: Game, ctx: &egui::Context) {
        self.clear_tree();
        self.browser = None;
        self.clear_preview();
        self.back.clear();
        self.forward.clear();
        self.keys = None;
        self.legacy_scan = None;
        self.legacy_files = 0;
        let g = game.clone();
        self.shared = Arc::default();
        self.keys_task = Some(Task::spawn("Preparing keys", ctx, move || {
            let keys = g.keys().map_err(|e| e.to_string())?;
            let shared = SharedTextures::index(&g, Some(&keys));
            Ok((keys, shared))
        }));
        // The edition and folder live at the right end of the status bar.
        self.status = match paleto_core::loader::setup(&game).problem() {
            Some(problem) => Status::Note(format!("⚠ {problem} (Tools > Mods in game)"), game.root.display().to_string()),
            None => Status::Info(String::new()),
        };
        self.mods_check = None;
        self.script_hook = None;
        self.expand_task = None;
        self.listing_task = None;
        let root = game.root.clone();
        self.startup.forget_interrupted();
        self.interrupted = paleto_core::txn::pending(&game);
        self.startup.offer_interrupted(self.interrupted.clone());
        self.game = Some(game);
        self.startup.begin(self.game.as_ref(), ctx);
        self.go(Location::Folder(root));
    }

    /// Whether a dialog covers the window: the rest of it ignores the keyboard.
    pub(crate) fn dialog_open(&self) -> bool {
        self.confirm_delete.is_some()
            || self.legacy_add.is_some()
            || self.addon_dialog.is_some()
            || self.install.is_some()
            || self.pending.is_some()
            || self.name_dialog.is_some()
            || self.texture_dialog.is_some()
            || self.texture_batch.is_some()
            || self.info.is_some()
            || self.mods_check.is_some()
            || self.script_hook.is_some()
            || self.startup.open()
            || self.snapshots.is_some()
            || self.prefix_tweaks.is_some()
            || self.crash.is_some()
            || self.export.is_some()
    }

    pub(crate) fn busy(&self) -> bool {
        self.task.is_some()
    }

    /// With a preview open, selecting another file previews it. Acts once per
    /// change of selection, so declining to lose edits does not ask again.
    fn follow_selection(&mut self, actions: &mut Vec<Action>) {
        let Some(b) = &self.browser else { return };
        let single = (b.selected.len() == 1).then(|| b.selected.first().unwrap().clone());
        let file = single.filter(|name| b.rows.iter().any(|r| &r.name == name && !r.is_container()));
        if self.followed == file {
            return;
        }
        self.followed = file.clone();
        if let (Some(name), Some(p)) = (file, &self.preview) {
            if p.name != name {
                actions.push(Action::Preview(name));
            } else {
                // Back on the file shown: another one still loading must not replace it.
                self.preview_task = None;
            }
        }
    }

    /// Runs an edit of the archive folder at `location` in the background,
    /// on the mods copy for original game archives.
    /// Looks for files made for Legacy in the mods folder, in the background.
    fn start_legacy_scan(&mut self, ctx: &egui::Context) {
        let Some(game) = self.game.clone().filter(|g| g.edition == paleto_core::Edition::Enhanced) else { return };
        let keys = self.keys.clone();
        self.legacy_scan = Some(Task::spawn("Checking the mods folder", ctx, move || {
            let cache = paleto_core::loader::ForeignCache::default_path();
            paleto_core::loader::foreign_files_cached_at(&game, keys.as_deref(), &cache).map(|f| f.len()).map_err(|e| e.to_string())
        }));
    }

    /// Adds files and folders from disk to the open archive folder, converting
    /// the Legacy ones with `tool` when given.
    pub(crate) fn spawn_add(&mut self, ctx: &egui::Context, location: ArchivePath, paths: Vec<PathBuf>, tool: Option<PathBuf>) {
        let keys = self.keys.clone();
        self.spawn_edit("Adding files", ctx, location, move |editor, folder| {
            let files: Vec<_> = paths.iter().map(|p| vfs::local_files(p)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect();
            let (data, log) = match &tool {
                Some(tool) => paleto_core::convert::read_converting(&files, tool, keys.as_deref())?,
                None => (files.iter().map(|(p, _)| std::fs::read(p)).collect::<Result<_, _>>()?, Vec::new()),
            };
            for ((_, rel), data) in files.iter().zip(data) {
                editor.add_file(&vfs::join_inner(folder, rel), data)?;
            }
            let count = files.len();
            let mut message = format!("{count} {} added or replaced", if count == 1 { "file" } else { "files" });
            let converted = log.iter().filter(|l| l.starts_with("converted ")).count();
            if converted > 0 {
                message += &format!(", {converted} converted to Enhanced");
            }
            if let Some(problem) = log.iter().find(|l| !l.starts_with("converted ")) {
                message += &format!(" ({problem})");
            }
            Ok(message)
        });
    }

    pub(crate) fn spawn_edit(
        &mut self,
        label: &str,
        ctx: &egui::Context,
        location: ArchivePath,
        f: impl FnOnce(&mut paleto_rpf::ArchiveEditor, &str) -> Result<String, paleto_core::Error> + Send + 'static,
    ) {
        let (game, keys) = (self.game.clone(), self.keys.clone());
        let location = self.original_archive(&location);
        self.show_originals = false;
        self.peek_original = None;
        self.task = Some(Task::spawn(label, ctx, move || {
            let run = || -> Result<(String, ArchivePath), paleto_core::Error> {
                let target = location.editable(game.as_ref())?;
                let message = target.edit(keys.as_deref(), f)?;
                Ok((message, target))
            };
            match run() {
                Ok((message, target)) => Outcome::Done { message, reload: Some(target) },
                Err(e) => Outcome::Failed(e.to_string()),
            }
        }));
    }

    /// Reads the side tree and the location shown again; when the location
    /// is gone, shows the closest folder still there.
    pub(crate) fn refresh(&mut self) {
        self.refresh_then(AfterListing::default());
    }

    fn refresh_then(&mut self, after: AfterListing) {
        self.clear_tree();
        let Some(current) = self.current() else { return };
        let location = match current {
            Location::Archive(a) if a.disk.is_file() => Location::Archive(a),
            Location::Archive(a) => Location::Folder(a.disk),
            Location::Folder(dir) => Location::Folder(dir),
        };
        let location = match location {
            Location::Folder(dir) => match dir.ancestors().find(|d| d.is_dir()) {
                Some(d) => Location::Folder(d.to_path_buf()),
                None => return,
            },
            archive => archive,
        };
        self.show_then(location, after);
    }

    fn poll_tasks(&mut self, ctx: &egui::Context) {
        if let Some(check) = &mut self.mods_check {
            check.poll();
        }
        if let Some(window) = &mut self.script_hook {
            window.poll();
        }
        self.startup.poll(self.game.as_ref());
        if !self.dialog_open() && !self.busy() {
            self.startup.promote(self.game.as_ref());
        }
        if let Some((location, with_keys, task)) = &self.changes_task
            && let Some(result) = task.poll()
        {
            let (location, with_keys) = (location.clone(), *with_keys);
            self.changes_task = None;
            if let (Ok(changes), Location::Archive(path)) = (result, &location) {
                let modified = std::fs::metadata(&path.disk).and_then(|m| m.modified()).ok();
                let changes = Arc::new(changes);
                if with_keys {
                    self.changes_cache.retain(|(l, _, _)| l != &location);
                    self.changes_cache.push((location.clone(), modified, changes.clone()));
                    if self.changes_cache.len() > 64 {
                        self.changes_cache.remove(0);
                    }
                }
                if let Some(b) = self.browser.as_mut().filter(|b| b.location == location) {
                    mark_changes(&mut b.rows, &changes);
                }
            }
        }
        if let Some(result) = self.keys_task.as_ref().and_then(Task::poll) {
            self.keys_task = None;
            match result {
                Ok((keys, shared)) => {
                    (self.keys, self.shared) = (Some(Arc::new(keys)), Arc::new(shared));
                    self.start_legacy_scan(ctx);
                }
                Err(e) => {
                    self.status = Status::Error(format!("keys: {e}"));
                    // Mods are often unencrypted: check them anyway.
                    self.start_legacy_scan(ctx);
                }
            }
        }
        if let Some(result) = self.export_task.as_ref().and_then(Task::poll) {
            self.export_task = None;
            match result {
                Ok(changes) => {
                    if let Some(w) = &mut self.export {
                        w.set_changes(changes);
                    }
                }
                Err(e) => {
                    self.export = None;
                    self.status = Status::Error(format!("comparing the mods folder: {e}"));
                }
            }
        }
        if let Some(read) = self.tree_task.as_ref().and_then(Task::poll) {
            self.tree_task = None;
            for (dir, entries) in read {
                self.tree_loading.remove(&dir);
                self.tree.insert(dir, entries);
            }
        }
        if let Some(listed) = self.listing_task.as_ref().and_then(|(_, t)| t.poll()) {
            let (pending, _) = self.listing_task.take().unwrap();
            self.show_listed(pending, listed, ctx);
        }
        if let Some(loaded) = self.preview_task.as_ref().and_then(Task::poll) {
            self.preview_task = None;
            // Edits made to the shown text while the other file loaded win: nobody asked about them.
            if !self.preview_dirty() {
                self.show_loaded(loaded);
            }
        }
        if let Some(found) = self.expand_task.as_ref().and_then(Task::poll) {
            self.expand_task = None;
            self.open_dirs.extend(found.into_iter().flatten());
        }
        if let Some(copies) = self.copies_task.as_ref().and_then(Task::poll) {
            self.copies_task = None;
            if let Some(check) = &mut self.mods_check {
                check.set_copies(copies);
            }
        }
        if let Some(result) = self.legacy_scan.as_ref().and_then(Task::poll) {
            self.legacy_scan = None;
            // A quiet check: a finding shows at the end of the status bar.
            if let Ok(n) = result {
                self.legacy_files = n;
            }
        }
        if let Some(task) = &self.task
            && let Some(outcome) = task.poll()
        {
            let label = task.label.clone();
            let elapsed = task.elapsed();
            let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
            let is_long = elapsed >= std::time::Duration::from_secs(2) || label.starts_with("Installing");

            if is_long || !focused {
                match &outcome {
                    Outcome::Done { message, .. } => {
                        desktop_notify(&label, message);
                    }
                    Outcome::Installed(Ok(changes)) => {
                        desktop_notify(&label, &install_summary(changes));
                    }
                    Outcome::Installed(Err((err, _))) => {
                        desktop_notify(&label, &format!("Installation failed: {err}"));
                    }
                    Outcome::Failed(err) => {
                        desktop_notify(&label, &format!("Failed: {err}"));
                    }
                    Outcome::Recovered(..) => {}
                }
            }

            self.task = None;
            match outcome {
                Outcome::Done { message, reload } => {
                    self.status = Status::Info(message);
                    if let Some((name, text)) = self.saving.take()
                        && let Some((_, saved)) = self.preview.as_mut().filter(|p| p.name == name).and_then(|p| p.text.as_mut())
                    {
                        *saved = text;
                    }
                    // The task may have added or converted Legacy files.
                    if self.legacy_scan.is_none() {
                        self.start_legacy_scan(ctx);
                    }
                    if let (Some(_), Some(game)) = (&self.mods_check, &self.game) {
                        self.mods_check = Some(ModsCheck::gather(game, self.keys.clone(), &self.ctx));
                    }
                    if let Some(window) = &mut self.script_hook {
                        window.refresh_facts();
                    }
                    if let (Some(_), Some(game)) = (&self.snapshots, &self.game) {
                        self.snapshots = Some(SnapshotsWindow::gather(game));
                    }
                    if let Some(window) = &mut self.crash {
                        window.reload(None);
                    }
                    let after = AfterListing { select: self.select_after.take(), reload_preview: self.refresh_preview.take() };
                    match reload {
                        Some(location) => {
                            self.clear_tree();
                            self.show_then(Location::Archive(location), after);
                        }
                        // The task may have changed what is shown (added packs, moved or deleted files).
                        None => self.refresh_then(after),
                    }
                }
                Outcome::Installed(result) => {
                    self.clear_tree();
                    if let Some(location) = self.current() {
                        self.show(location);
                    }
                    self.show_install_result(result);
                }
                Outcome::Recovered(result, aside) => {
                    let mut lines = Vec::new();
                    let failed;
                    match result {
                        Ok(report) => {
                            if report.rolled_back + report.finished > 0 {
                                lines.push(format!("{} undone, {} finished.", report.rolled_back, report.finished));
                            }
                            lines.extend(report.log);
                            failed = !report.failures.is_empty();
                            lines.extend(report.failures.into_iter().map(|f| format!("Not done: {f}")));
                        }
                        Err(e) => {
                            failed = true;
                            lines.push(format!("Not done: {e}"));
                        }
                    }
                    if let Some(game) = &self.game {
                        self.interrupted = paleto_core::txn::pending(game);
                    }
                    let title = match (failed, aside) {
                        (true, _) => "Something is still left from an interrupted change",
                        (false, true) => "Interrupted change set aside",
                        (false, false) => "Interrupted change settled",
                    };
                    self.status = if failed { Status::Error("An interrupted change could not be settled".into()) } else { Status::Info(title.into()) };
                    self.startup.offer_report(title, lines, failed);
                    self.startup.offer_interrupted(self.interrupted.clone());
                    self.clear_tree();
                    self.refresh_then(AfterListing::default());
                }
                Outcome::Failed(e) => {
                    self.status = Status::Error(e);
                    self.saving = None;
                    self.select_after = None;
                    self.refresh_preview = None;
                    // Do not carry on (e.g. quit) after a failed save.
                    self.after_task = None;
                }
            }
        }
    }

}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::apply(self.settings.theme, &self.system_theme, &ctx);
        #[cfg(target_os = "linux")]
        self.take_requests(&ctx);
        self.poll_tasks(&ctx);
        let mut actions = Vec::new();
        if self.task.is_none()
            && let Some(action) = self.after_task.take()
        {
            actions.push(action);
        }

        // Closing the window with unsaved edits asks first. While an
        // operation runs (an install, a restore), closing would stop it
        // halfway: the first request only warns, a second one closes.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if let Some(task) = self.task.as_ref().filter(|_| !self.close_while_busy) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.status = Status::Error(format!("{} is not finished: closing now would stop it halfway. Close again to quit anyway.", task.label));
                self.close_while_busy = true;
            } else if self.preview_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Action::Quit);
            }
        }
        if self.task.is_none() {
            self.close_while_busy = false;
        }
        let dialog = self.dialog_open();
        if !dialog && !self.busy() && self.preview_dirty() && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            actions.push(Action::SavePreview);
        }
        // Ctrl+F searches the text being previewed.
        if !dialog
            && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F))
            && let Some(p) = self.preview.as_mut().filter(|p| p.text.is_some())
        {
            match &mut p.find {
                Some(find) => find.focus = true,
                None => p.find = Some(Find::new()),
            }
        }
        if !dialog && !self.busy() && ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            actions.push(Action::Refresh);
        }

        // Mouse back/forward buttons and Alt+arrows navigate the history.
        let (back, forward) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Extra1) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
                i.pointer.button_pressed(egui::PointerButton::Extra2) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)),
            )
        });
        if back && !dialog {
            actions.push(Action::Back);
        }
        if forward && !dialog {
            actions.push(Action::Forward);
        }

        // Files dropped on the window: packages open the installer, anything
        // else is added to the archive folder being browsed.
        let mut dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        #[cfg(target_os = "linux")]
        for event in self.wayland_drops.as_ref().map(|rx| rx.try_iter().collect::<Vec<_>>()).unwrap_or_default() {
            match event {
                crate::wayland_dnd::DndEvent::Hover(on) => self.drag_hover = on,
                crate::wayland_dnd::DndEvent::Dropped(paths) => dropped.extend(paths),
                crate::wayland_dnd::DndEvent::Failed(why) => self.status = Status::Error(format!("Drop failed: {why}")),
            }
        }
        if !dropped.is_empty() {
            let (packages, files): (Vec<_>, Vec<_>) =
                dropped.into_iter().partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("oiv")));
            if self.busy() {
                self.status = Status::Error("Wait for the current operation to finish, then drop the files again.".into());
            } else if dialog {
                self.status = Status::Error("Close the open dialog, then drop the files again.".into());
            } else if let Some(pkg) = packages.first() {
                self.open_package(pkg, &ctx);
            } else if let Some(packs) = self.game.as_ref().map(|_| paleto_core::addon::find_packs(&files, self.keys.as_deref())).filter(|p| !p.is_empty()) {
                actions.push(Action::AskAddonPacks(packs));
            } else if self.dropped_textures(&files) {
                actions.push(Action::AskTextureBatch(files));
            } else if self.archive_folder().is_some() {
                actions.push(Action::Add(files));
            } else {
                self.status = Status::Error("Files can only be added inside an archive: open an .rpf file first.".into());
            }
        }

        // The menu and location bars are spaced like the toolbars below them.
        let bar = egui::Frame::side_top_panel(ui.style()).inner_margin(crate::widgets::MARGIN);
        egui::Panel::top("menu").frame(bar).show(ui, |ui| self.menu_bar(ui, &mut actions));
        if self.game.is_some() {
            egui::Panel::top("location").frame(bar).show(ui, |ui| self.location_bar(ui, &mut actions));
        }
        // What the folder shown holds, and what is selected in it.
        let counts = self.browser.as_ref().map(|b| {
            let filter = b.filter.to_ascii_lowercase();
            let shown = b.rows.iter().filter(|r| r.name.to_ascii_lowercase().contains(&filter)).count();
            let (selected, bytes) = b.rows.iter().filter(|r| b.selected.contains(&r.name)).fold((0, 0u64), |(n, t), r| (n + 1, t + r.size));
            let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
            let mut text = if shown == b.rows.len() { items(shown) } else { format!("{shown} of {}", items(b.rows.len())) };
            if selected > 0 {
                text += &format!(" · {selected} selected");
                if bytes > 0 {
                    text += &format!(" ({})", size(bytes));
                }
            }
            text
        });
        let edition = self.game.as_ref().map(|g| (format!("{:?}", g.edition), g.root.display().to_string()));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dark = ui.visuals().dark_mode;
                // The edition at the far end: the folder on hover.
                if let Some((name, path)) = &edition {
                    ui.label(RichText::new(format!("{name} edition")).weak()).on_hover_text(path);
                    ui.separator();
                }
                // Stays until the files are converted, whatever else the status says.
                if self.legacy_files > 0 {
                    let n = self.legacy_files;
                    let what = if n == 1 { "1 file in the mods folder was".to_string() } else { format!("{n} files in the mods folder were") };
                    let full = format!("⚠ {what} made for Legacy");
                    let width = egui::WidgetText::from(full.as_str()).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x;
                    // Shorter on a narrow window.
                    let label = if width <= ui.available_width() / 2.0 { full } else { format!("⚠ {n} Legacy {}", if n == 1 { "file" } else { "files" }) };
                    let text = RichText::new(label).color(theme::warning(dark));
                    if ui
                        .add(egui::Button::new(text).frame(false))
                        .on_hover_text("The Enhanced game may crash on them. Click to see them and convert them (Tools > Mods in game).")
                        .clicked()
                    {
                        actions.push(Action::CheckMods(true));
                    }
                    ui.separator();
                }
                // Counts, while the window is wide enough for them and the rest.
                if let Some(text) = counts.filter(|_| ui.available_width() > 420.0) {
                    ui.label(text);
                    ui.separator();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let running = [
                        self.task.as_ref().map(|t| (&t.label, t.cancellable())),
                        self.keys_task.as_ref().map(|t| (&t.label, t.cancellable())),
                        self.copies_task.as_ref().map(|t| (&t.label, t.cancellable())),
                        self.export_task.as_ref().map(|t| (&t.label, t.cancellable())),
                        self.expand_task.as_ref().map(|t| (&t.label, t.cancellable())),
                        // Reads that take long enough to notice.
                        self.listing_task.as_ref().map(|(_, t)| t).filter(|t| t.elapsed() >= LOADING_SHOWN).map(|t| (&t.label, t.cancellable())),
                        self.preview_task.as_ref().filter(|t| t.elapsed() >= LOADING_SHOWN).map(|t| (&t.label, t.cancellable())),
                    ];
                    if let Some((t, cancellable)) = running.into_iter().flatten().next() {
                        ui.spinner();
                        if cancellable && ui.button("✖ Cancel").on_hover_text("Stop it: nothing it found so far is used").clicked() {
                            actions.push(Action::CancelTask);
                        }
                        ui.add(egui::Label::new(format!("{t}…")).truncate());
                        ui.separator();
                    }
                    match &self.status {
                        Status::Info(s) => ui.add(egui::Label::new(s).truncate()),
                        Status::Note(s, more) => ui.add(egui::Label::new(s).truncate()).on_hover_text(more),
                        Status::Error(s) => ui.add(egui::Label::new(RichText::new(s).color(theme::error(dark))).truncate()).on_hover_text(s),
                    };
                });
            });
        });

        if self.game.is_none() {
            egui::CentralPanel::default().show(ui, |ui| self.welcome(ui, &mut actions));
        } else {
            // The central panel's frame, so the tree's buttons line up with the file toolbar.
            let frame = egui::Frame::central_panel(ui.style()).inner_margin(crate::widgets::MARGIN);
            // Side panels never squeeze the file list below a usable width.
            const LIST_MIN: f32 = 300.0;
            let width = ui.available_width();
            let tree_max = (width * 0.35).max(160.0);
            if self.settings.show_tree {
                egui::Panel::left("tree").frame(frame).resizable(true).default_size(280.0).size_range(160.0..=tree_max).show(ui, |ui| {
                    self.side_tree(ui, &mut actions);
                });
            }
            // A load shows in the panel once it takes long enough to notice.
            let loading = self.preview_task.as_ref().filter(|t| t.elapsed() >= LOADING_SHOWN).map(|t| t.label.clone());
            if let Some(t) = self.preview_task.as_ref().filter(|t| t.elapsed() < LOADING_SHOWN) {
                ctx.request_repaint_after(LOADING_SHOWN.saturating_sub(t.elapsed()));
            }
            if self.preview.is_some() || loading.is_some() {
                let preview_max = (ui.available_width() - LIST_MIN).max(240.0);
                egui::Panel::right("preview").resizable(true).default_size(460.0).size_range(240.0..=preview_max).show(ui, |ui| {
                    if let Some(label) = &loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.add(egui::Label::new(RichText::new(format!("{label}…")).weak()).truncate());
                        });
                    }
                    self.preview_ui(ui, &mut actions);
                });
            }
            egui::CentralPanel::default().frame(frame).show(ui, |ui| self.browser_ui(ui, &mut actions));
            self.follow_selection(&mut actions);
        }

        if self.drag_hover || ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            self.drop_overlay(&ctx);
        }

        self.install_window(&ctx, &mut actions);
        self.delete_window(&ctx, &mut actions);
        self.legacy_add_window(&ctx, &mut actions);
        self.addon_window(&ctx, &mut actions);
        self.name_window(&ctx, &mut actions);
        self.texture_window(&ctx, &mut actions);
        self.texture_batch_window(&ctx, &mut actions);
        self.unsaved_window(&ctx, &mut actions);
        self.info_window(&ctx, &mut actions);
        let busy = self.busy();
        if let Some(request) = self.crash.as_mut().and_then(|w| w.show(&ctx, busy)) {
            actions.push(match request {
                crash_window::Request::Close => Action::CrashDetection(false),
                crash_window::Request::UpdateScriptHook => Action::ScriptHook(true),
                crash_window::Request::ModsInGame => Action::CheckMods(true),
                crash_window::Request::TurnOff(part) => Action::TurnOff(part),
                crash_window::Request::HuntStart => Action::HuntStart,
                crash_window::Request::HuntAnswer(crashed) => Action::HuntAnswer(crashed),
                crash_window::Request::HuntStop => Action::HuntStop,
                crash_window::Request::HuntInside(part) => Action::HuntInside(part),
            });
        }
        if let Some(request) = self.export.as_mut().and_then(|w| w.show(&ctx, busy)) {
            actions.push(match request {
                export_window::Request::Close => Action::Export(false),
                export_window::Request::Write(path, changes, meta) => Action::WriteExport(path, changes, meta),
            });
        }
        if let Some(request) = self.snapshots.as_mut().and_then(|w| w.show(&ctx, busy)) {
            actions.push(match request {
                snapshots_window::Request::Close => Action::Snapshots(false),
                snapshots_window::Request::Save(label) => Action::SaveSnapshot(label),
                snapshots_window::Request::Restore(id, save_first) => Action::RestoreSnapshot(id, save_first),
                snapshots_window::Request::Delete(id) => Action::DeleteSnapshot(id),
            });
        }
        if self.prefix_tweaks.as_mut().is_some_and(|w| w.show(&ctx)) {
            actions.push(Action::PrefixTweaks(false));
        }
        if let Some(request) = self.mods_check.as_mut().and_then(|m| m.show(&ctx)) {
            actions.push(match request {
                mods_check::Request::Close => Action::CheckMods(false),
                mods_check::Request::Refresh => Action::CheckMods(true),
                mods_check::Request::AddDlcPacks(packs) => Action::AddDlcPacks(packs),
                mods_check::Request::Convert(paths) => Action::ConvertMods(paths),
                mods_check::Request::UndoConversion(time) => Action::UndoConversion(time),
                mods_check::Request::FindCopies => Action::FindCopies,
                mods_check::Request::DeleteCopies(copies) => Action::DeleteCopies(copies),
                mods_check::Request::Toggle(part, enabled) => Action::ToggleMod(part, enabled),
                mods_check::Request::OpenScriptHook => Action::ScriptHook(true),
                mods_check::Request::PrefixTweaks => Action::PrefixTweaks(true),
            });
        }
        if let Some(request) = self.script_hook.as_ref().and_then(|w| w.show(&ctx)) {
            match request {
                scripthook_window::Request::Close => actions.push(Action::ScriptHook(false)),
                scripthook_window::Request::Retry => {
                    if let Some(window) = &mut self.script_hook {
                        window.retry(&ctx);
                    }
                }
                scripthook_window::Request::Update(release) => actions.push(Action::ScriptHookUpdate(release)),
            }
        }
        if let Some(reply) = self.startup.show(&ctx) {
            match reply {
                notices::Reply::AskAbandon => self.startup.confirm_abandon(true),
                notices::Reply::Back => self.startup.confirm_abandon(false),
                notices::Reply::Undo | notices::Reply::Abandon(_) if self.busy() => {
                    // Kept on screen: the choice is made again once the operation is done.
                    self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
                }
                reply => {
                    self.startup.dismiss();
                    match reply {
                        notices::Reply::Update(release) => actions.push(Action::ScriptHookUpdate(release)),
                        notices::Reply::OpenScriptHook => actions.push(Action::ScriptHook(true)),
                        notices::Reply::OpenCrash => actions.push(Action::CrashDetection(true)),
                        notices::Reply::Undo => actions.push(Action::RecoverInterrupted),
                        notices::Reply::Abandon(ids) => actions.push(Action::AbandonInterrupted(ids)),
                        notices::Reply::AskAbandon | notices::Reply::Back | notices::Reply::Dismiss => {}
                    }
                }
            }
        }
        for action in actions {
            self.apply(action, &ctx);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(game) = &self.game {
            storage.set_string("game", game.root.display().to_string());
        }
        storage.set_string("settings", self.settings.to_storage());
        if let Some(time) = self.startup.last_seen_crash {
            storage.set_string("last_seen_crash", time.to_string());
        }
        if self.settings.remember_open_folders {
            let open_dirs: Vec<String> = self.open_dirs.iter().map(|p| p.display().to_string()).collect();
            storage.set_string("open_dirs", open_dirs.join("\n"));
        } else {
            storage.remove_string("open_dirs");
        }
    }
}

/// How long a preview load runs before the panel shows that it is loading.
pub(crate) const LOADING_SHOWN: std::time::Duration = std::time::Duration::from_millis(150);
pub(crate) fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::extract;

    #[test]
    fn install_summary_counts_changes_apart_from_warnings() {
        let log: Vec<String> = ["add mods/x.rpf\\a.ytd", "edit mods/x.rpf\\b.meta", "warning: refusing to delete c", "saved mods/x.rpf", "converted d.ydr", "warning: e: not converted: bad"].map(String::from).into();
        assert_eq!(install_summary(&log), "Installed successfully (3 changes, 2 warnings)");
        assert_eq!(install_summary(&log[..1]), "Installed successfully (1 change)");
        assert_eq!(install_summary(&log[2..4]), "Installed successfully (0 changes, 1 warning)");
    }

    /// A folder without a name would make "dest.join(/file)" drop the destination: such an archive is damaged and extracts nothing.
    #[test]
    fn extracting_a_hostile_archive_writes_nothing_outside_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, out) = (dir.path().join("a.rpf"), dir.path().join("out"));
        std::fs::create_dir(&out).unwrap();
        let mut ed = paleto_rpf::ArchiveEditor::new("a.rpf");
        ed.add_file("dd/paleto-escape-probe", b"x".to_vec()).unwrap();
        ed.add_file("ok.txt", b"y".to_vec()).unwrap();
        ed.save(&archive).unwrap();
        let location = ArchivePath::new(archive.clone());
        assert_eq!(extract(&location, None, &["dd".into(), "ok.txt".into()], &out), Ok((2, Vec::new())));
        std::fs::remove_dir_all(out.join("dd")).unwrap();
        std::fs::remove_file(out.join("ok.txt")).unwrap();
        // Empty the folder's name in the names table (the root's is empty already).
        let mut data = std::fs::read(&archive).unwrap();
        let names = 16 + 16 * u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let at = names + data[names..].windows(3).position(|w| w == b"dd\0").unwrap();
        data[at] = 0;
        std::fs::write(&archive, data).unwrap();
        assert!(extract(&location, None, &[String::new(), "ok.txt".into()], &out).is_err());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
        assert!(!Path::new("/paleto-escape-probe").exists());
        // Whatever the parser lets through, a path that is not plain relative is refused before a write.
        assert!(vfs::extract_path(&out, "/paleto-escape-probe").is_err());
    }
}
