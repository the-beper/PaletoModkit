use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use paleto_core::oiv::Metadata;
use paleto_core::shared_textures::SharedTextures;
use paleto_core::texture_batch::{self, TextureBatch};
use paleto_core::{ArchivePath, Game, Package, game, vfs};
use paleto_formats::{Format, MetaFile, TextureDictionary};
use paleto_rpf::{Encryption, EntryKind, Keys};

use crate::audio::{self, AudioRequest, AudioView};
use crate::highlight::{self, Syntax};
use crate::model::ModelView;
use crate::mods_check::{self, ModsCheck};
use crate::prefix_window::PrefixWindow;
use crate::notices::{self, Startup};
use crate::scripthook_window::{self, ScriptHookWindow};
use crate::snapshots_window::{self, SnapshotsWindow};
use crate::crash_window::{self, CrashWindow};
use crate::export_window::{self, ExportWindow};
use crate::settings::{Density, FileClass, Grouping, Settings, SortBy, TextSize, Theme};
use crate::task::Task;
use crate::theme::{self, SystemTheme};
use crate::textures::{self, Export, Request, TextureEdit, TextureView};

/// Text files up to this size can be edited in the preview panel.
const MAX_TEXT: usize = 8 << 20;
/// The buttons at the right end of the menu and location bars, one width
/// for all their labels so they line up and do not change size.
const INSTALL: &str = "📦 Install package…";
const INSTALL_SHORT: &str = "📦 Install…";
const TWEAKS: &str = "🛠 Prefix tweaks…";
const TWEAKS_SHORT: &str = "🛠 Tweaks…";
const ORIGINAL: &str = "↩ Show original";
const MODS_COPY: &str = "↪ Show mods copy";

fn right_width(ui: &egui::Ui) -> f32 {
    let padding = 2.0 * ui.ctx().global_style().spacing.button_padding.x;
    [INSTALL, TWEAKS, ORIGINAL, MODS_COPY]
        .iter()
        .map(|t| egui::WidgetText::from(*t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + padding)
        .fold(0.0, f32::max)
}

/// Marks a game file that has a modified copy in the mods folder.
const MODS_LINK: &str = "↪";
const MODS_LINK_HOVER: &str = "The game loads the modified copy of this file in the mods folder";

const REPO_URL: &str = "https://github.com/the-beper/PaletoModkit";

/// What the main view shows: a folder on disk or a folder inside an archive.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    Folder(PathBuf),
    Archive(ArchivePath),
}

/// The entries of a folder that differ from the original game files, by lowercase name.
type Changes = HashMap<String, vfs::Change>;

pub struct App {
    game: Option<Game>,
    installs: Vec<Game>,
    keys: Option<Arc<Keys>>,
    keys_task: Option<Task<Result<(Keys, SharedTextures), String>>>,
    /// The check for files made for Legacy in the mods folder, run once the
    /// keys are ready (Enhanced edition): how many it found.
    legacy_scan: Option<Task<Result<usize, String>>>,
    /// Files of the mods folder made for Legacy, as the last scan found them.
    legacy_files: usize,
    /// The search for copies of game files left unchanged in the mods folder.
    copies_task: Option<Task<Vec<paleto_core::cleanup::Copy>>>,
    /// Where the dictionaries that models share are (vehshare.ytd...).
    shared: Arc<SharedTextures>,
    settings: Settings,
    /// Folder listings for the side tree, cached until the next refresh.
    tree: HashMap<PathBuf, Vec<FsEntry>>,
    /// Expand the side tree down to the current location on the next frame.
    reveal: bool,
    /// Folders expanded in the side tree.
    open_dirs: BTreeSet<PathBuf>,
    /// Help or About window being shown.
    info: Option<Info>,
    /// Comparing the folder being shown (in a mods copy) with the original.
    /// For background work started outside a frame.
    ctx: egui::Context,
    /// Files handed over by later launches of the app.
    #[cfg(target_os = "linux")]
    instance: Option<crate::single_instance::Instance>,
    /// Raises the window when a later launch asks for it.
    #[cfg(target_os = "linux")]
    activator: Option<crate::wayland_activation::Activator>,
    changes_task: Option<(Location, Task<Result<Changes, String>>)>,
    /// Comparisons done, by folder and the archive's modification time.
    changes_cache: Vec<(Location, Option<std::time::SystemTime>, Arc<Changes>)>,
    /// The desktop's light or dark preference, for the "same as the system" theme.
    system_theme: SystemTheme,
    mods_check: Option<ModsCheck>,
    script_hook: Option<ScriptHookWindow>,
    /// The checks made at start and the notices they bring.
    startup: Startup,
    snapshots: Option<SnapshotsWindow>,
    prefix_tweaks: Option<PrefixWindow>,
    crash: Option<CrashWindow>,
    export: Option<ExportWindow>,
    /// The comparison of the mods folder for the export window.
    export_task: Option<Task<Result<Vec<paleto_core::export::Changed>, String>>>,
    browser: Option<Browser>,
    back: Vec<Location>,
    forward: Vec<Location>,
    show_originals: bool,
    /// An original archive shown as it is although it has a mods copy (the
    /// location bar's Show original), until the browser leaves it.
    peek_original: Option<PathBuf>,
    preview: Option<Preview>,
    /// Reload the preview after the running edit, selecting this texture if present.
    refresh_preview: Option<String>,
    install: Option<InstallDialog>,
    confirm_delete: Option<Vec<String>>,
    /// Files about to be added, some of them made for Legacy.
    legacy_add: Option<LegacyAdd>,
    /// Add-on packs about to be installed.
    addon_dialog: Option<AddonDialog>,
    /// Asking for a name: renaming an item or creating a folder.
    name_dialog: Option<NameDialog>,
    /// Confirming a change to the previewed textures, or completing it (name, format).
    texture_dialog: Option<TextureDialog>,
    texture_batch: Option<TextureBatchDialog>,
    /// The single selected file last seen, for the preview to follow.
    followed: Option<String>,
    /// Select this item once the running edit has reloaded the folder.
    select_after: Option<String>,
    task: Option<Task<Outcome>>,
    /// Runs when the current task finishes (e.g. navigating after a save).
    after_task: Option<Action>,
    /// An action waiting for the user to save or discard the edited file.
    pending: Option<Action>,
    /// The window may close without asking (unsaved changes were handled).
    allow_close: bool,
    /// Drops reported by the Wayland connection (winit has none on Wayland).
    #[cfg(target_os = "linux")]
    wayland_drops: Option<std::sync::mpsc::Receiver<crate::wayland_dnd::DndEvent>>,
    /// Files are being dragged over the window (Wayland).
    drag_hover: bool,
    status: Status,
}

struct FsEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    /// An original archive with a modified copy in the mods folder.
    modded: bool,
}

struct Browser {
    location: Location,
    from_mods: bool,
    /// Opened as the original archive and shown as its mods copy: the side
    /// tree and the history keep the original.
    via_original: bool,
    encryption: Option<Encryption>,
    rows: Vec<Row>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    filter: String,
    focus_filter: bool,
    /// Drop the filter's keyboard focus (after moving to another folder).
    blur_filter: bool,
    /// The last row clicked and when, to detect double clicks ourselves:
    /// egui counts a quick second double click as a triple click.
    last_click: Option<(String, f64)>,
    scroll_to: Option<usize>,
}

struct Row {
    name: String,
    kind: RowKind,
    size: u64,
    /// A mods copy of an archive on disk, or (inside a mods copy) differs
    /// from the original game file.
    modded: bool,
    /// Inside a mods copy: not in the original game files.
    added: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum RowKind {
    Folder,
    Archive,
    File,
    Resource(u32),
}

impl Row {
    fn class(&self) -> FileClass {
        match self.kind {
            RowKind::Folder => FileClass::Folder,
            RowKind::Archive => FileClass::Archive,
            _ => FileClass::of(&self.name),
        }
    }

    fn is_container(&self) -> bool {
        matches!(self.kind, RowKind::Folder | RowKind::Archive)
    }
}

enum PreviewSource {
    Disk(PathBuf),
    Archive(ArchivePath),
}

struct Preview {
    source: PreviewSource,
    name: String,
    syntax: Syntax,
    info: String,
    /// Text content (edited in place) and the saved version.
    text: Option<(String, String)>,
    /// How the text is turned back into the file.
    codec: Codec,
    textures: Option<TextureView>,
    model: Option<ModelView>,
    audio: Option<AudioView>,
    /// The search bar of a text preview, while it is open.
    find: Option<Find>,
}

/// The search of a text preview.
#[derive(Default)]
struct Find {
    query: String,
    matches: Vec<std::ops::Range<usize>>,
    /// The match shown: an index of `matches`.
    current: usize,
    /// What `matches` was found with: the query and the length of the text.
    found_with: (String, usize),
    /// The text was edited since: look again.
    stale: bool,
    /// Give the search field the keyboard on the next frame.
    focus: bool,
    /// Scroll the current match into view on the next frame.
    jump: bool,
}

impl Find {
    fn new() -> Self {
        Find { focus: true, ..Find::default() }
    }

    /// Looks again when the query or the text changed.
    fn refresh(&mut self, text: &str) {
        if self.stale || self.found_with.0 != self.query || self.found_with.1 != text.len() {
            self.matches = highlight::find_all(text, &self.query);
            self.found_with = (self.query.clone(), text.len());
            self.stale = false;
            self.current = self.current.min(self.matches.len().saturating_sub(1));
        }
    }

    /// Moves to the next (or previous) match, wrapping round.
    fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n > 0 {
            self.current = if forward { (self.current + 1) % n } else { (self.current + n - 1) % n };
            self.jump = true;
        }
    }
}

/// What the text of a preview is: the file itself, or a conversion of it.
enum Codec {
    Plain,
    /// A text table (`.gxt2`) as "0xHASH = text" lines.
    Gxt2,
    /// Metadata (PSO or resource) as XML.
    Meta(Arc<MetaFile>),
}

impl Codec {
    fn encode(&self, text: &str) -> Result<Vec<u8>, String> {
        match self {
            Codec::Plain => Ok(text.as_bytes().to_vec()),
            Codec::Gxt2 => paleto_formats::gxt2::from_text(text).map_err(|e| e.to_string()),
            Codec::Meta(meta) => meta.rebuild_from_xml(text).map_err(|e| e.to_string()),
        }
    }
}

/// Add-on packs (dlc.rpf) dropped or chosen, to install into the mods folder.
struct AddonDialog {
    /// Each pack and the name it goes under (editable).
    packs: Vec<(PathBuf, String)>,
    /// How many files made for Legacy the packs hold (Enhanced edition).
    legacy: usize,
    convert: bool,
}

/// Files dropped or chosen to add to an archive, some made for Legacy.
struct LegacyAdd {
    paths: Vec<PathBuf>,
    /// The Legacy files, relative to what was chosen.
    legacy: Vec<String>,
    convert: bool,
}

struct InstallDialog {
    path: PathBuf,
    package: Option<Package>,
    metadata: Metadata,
    icon: Option<egui::TextureHandle>,
    /// The package's files made for Legacy (Enhanced edition only).
    legacy: Vec<String>,
    /// Convert them before installing.
    convert: bool,
    result: Option<Result<Vec<String>, String>>,
}

enum Outcome {
    /// An operation finished; show the message and reopen `reload` if given.
    Done { message: String, reload: Option<ArchivePath> },
    Installed(Result<Vec<String>, String>),
    Failed(String),
}

struct NameDialog {
    /// The item being renamed, or None for a new folder.
    renaming: Option<String>,
    name: String,
    /// Focus the text field (and select the name) on the next frame.
    focus: bool,
}

struct TextureDialog {
    edit: TextureEdit,
    /// Focus the name field on the next frame.
    focus: bool,
}

/// Replacing many textures of the previewed dictionary from files.
struct TextureBatchDialog {
    batch: TextureBatch,
    /// Also add the files that match no texture, as new textures.
    add_unmatched: bool,
}

enum Status {
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

#[derive(Clone, Copy)]
enum Unsaved {
    Save,
    Discard,
    Cancel,
}

enum Action {
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
    Extract(Vec<String>),
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
    ShowInfo(Option<Info>),
    Settings(Settings),
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
            shared: Arc::default(),
            settings: Settings::default(),
            tree: HashMap::new(),
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
            && !self.startup.running()
            && !self.mods_check.as_ref().is_some_and(ModsCheck::checking)
            && !self.crash.as_ref().is_some_and(CrashWindow::loading)
    }

    /// The names marked as differing from the original game files, and
    /// whether each is new.
    pub fn changed_names(&self) -> Vec<(String, bool)> {
        self.browser.iter().flat_map(|b| b.rows.iter().filter(|r| r.modded).map(|r| (r.name.clone(), r.added))).collect()
    }

    /// Goes to a folder or archive, recording it in the history.
    pub fn go(&mut self, location: Location) {
        if let Some(b) = &self.browser
            && self.opened(b) != location
        {
            self.back.push(self.opened(b));
            self.forward.clear();
        }
        self.reveal = true;
        self.show(location);
    }

    /// Opens an archive location.
    pub fn open(&mut self, location: ArchivePath) {
        self.go(Location::Archive(location));
    }

    pub fn location(&self) -> Option<&Location> {
        self.browser.as_ref().map(|b| &b.location)
    }

    /// The folder or archive file highlighted in the side tree.
    pub fn tree_selection(&self) -> Option<PathBuf> {
        self.browser.as_ref().map(|b| match self.opened(b) {
            Location::Folder(p) => p,
            Location::Archive(a) => a.disk,
        })
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
    fn take_requests(&mut self, ctx: &egui::Context) {
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

    /// The text shown in the preview editor.
    /// Opens the search bar of the text preview with `query`, as typing it would; the
    /// number of matches found (for tests).
    pub fn search_preview(&mut self, query: &str) -> Option<usize> {
        let p = self.preview.as_mut()?;
        let text = &p.text.as_ref()?.0;
        let mut find = Find::new();
        find.focus = false;
        find.jump = true;
        find.query = query.to_string();
        find.refresh(text);
        let n = find.matches.len();
        p.find = Some(find);
        Some(n)
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

    /// The text of the rename / new folder dialog, if it is open.
    pub fn name_dialog(&self) -> Option<&str> {
        self.name_dialog.as_ref().map(|d| d.name.as_str())
    }

    /// Whether the "unsaved changes" prompt is showing.
    pub fn asking_unsaved(&self) -> bool {
        self.pending.is_some()
    }

    /// Textures of the previewed dictionary, and the selected one.
    pub fn preview_textures(&self) -> Option<(Vec<String>, usize)> {
        let view = self.preview.as_ref()?.textures.as_ref()?;
        Some((view.dict.textures.iter().map(|t| t.name.clone()).collect(), view.selected))
    }

    pub fn edit_texture(&mut self, edit: TextureEdit, ctx: &egui::Context) {
        self.apply(Action::EditTexture(edit), ctx);
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

    pub fn export_textures(&mut self, export: Export, dest: &Path, ctx: &egui::Context) {
        self.apply(Action::ExportTextures(export, Some(dest.to_path_buf())), ctx);
    }

    /// Replaces a sound of the previewed audio container with a WAV or MP3 file.
    pub fn replace_sound(&mut self, id: u32, file: &Path, ctx: &egui::Context) {
        self.apply(Action::ReplaceSound(id, Some(file.to_path_buf())), ctx);
    }

    /// Exports every sound of the previewed audio container into `dir`.
    pub fn export_sounds(&mut self, dir: &Path, ctx: &egui::Context) {
        self.apply(Action::ExportSounds(AudioRequest::ExportAll, Some(dir.to_path_buf())), ctx);
    }

    /// The details line of the preview panel.
    pub fn preview_info(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.info.as_str())
    }

    /// Opens a help window (or closes it with `None`), as the menus do.
    pub fn show_info(&mut self, info: Option<Info>, ctx: &egui::Context) {
        self.apply(Action::ShowInfo(info), ctx);
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

    /// Changes listed and selected in the export window, if open.
    pub fn export_counts(&self) -> Option<(usize, usize)> {
        self.export.as_ref().map(ExportWindow::counts)
    }

    pub fn open_export(&mut self, ctx: &egui::Context) {
        self.apply(Action::Export(true), ctx);
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

    pub fn mods_check_summary(&self) -> Option<String> {
        self.mods_check.as_ref().map(ModsCheck::summary)
    }

    /// Adds DLC packs to the mods copy's DLC list, as the "Mods in game" window does.
    /// Asks to install the add-on packs found in `paths` (as when dropped).
    pub fn ask_addon_packs(&mut self, paths: &[PathBuf], ctx: &egui::Context) {
        let packs = paleto_core::addon::find_packs(paths, self.keys.as_deref());
        self.apply(Action::AskAddonPacks(packs), ctx);
    }

    pub fn add_dlc_packs(&mut self, packs: &[&str], ctx: &egui::Context) {
        self.apply(Action::AddDlcPacks(packs.iter().map(|p| p.to_string()).collect()), ctx);
    }

    /// The error shown in the status bar, if any.
    pub fn status_error(&self) -> Option<&str> {
        match &self.status {
            Status::Error(e) => Some(e),
            Status::Info(_) | Status::Note(..) => None,
        }
    }

    pub fn previewing(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.name.as_str())
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

    fn select_game(&mut self, game: Game, ctx: &egui::Context) {
        self.tree.clear();
        self.browser = None;
        self.preview = None;
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
        let root = game.root.clone();
        self.game = Some(game);
        self.startup.begin(self.game.as_ref(), ctx);
        self.go(Location::Folder(root));
    }

    /// Whether a dialog covers the window: the rest of it ignores the keyboard.
    fn dialog_open(&self) -> bool {
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

    fn busy(&self) -> bool {
        self.task.is_some()
    }

    /// Shows a location without touching the history. Archives show their
    /// mods copy when there is one.
    fn show(&mut self, location: Location) {
        if self.peek_original.as_ref().is_some_and(|d| !matches!(&location, Location::Archive(a) if &a.disk == d)) {
            self.peek_original = None;
        }
        let peeking = self.peek_original.is_some();
        let mut redirected = false;
        let shown = match location {
            Location::Folder(path) => {
                let in_mods = self.game.as_ref().is_some_and(|g| path.starts_with(g.mods_dir()));
                self.folder_rows(&path).map(|rows| (Location::Folder(path), in_mods, None, rows))
            }
            Location::Archive(path) => {
                let (path, from_mods) = match &self.game {
                    Some(game) if !self.show_originals && !peeking => match path.in_mods(game) {
                        Some(m) => {
                            redirected = true;
                            (m, true)
                        }
                        None => {
                            let in_mods = path.disk.starts_with(game.mods_dir());
                            (path, in_mods)
                        }
                    },
                    Some(game) => {
                        let in_mods = path.disk.starts_with(game.mods_dir());
                        (path, in_mods)
                    }
                    None => (path, false),
                };
                self.archive_rows(&path).map(|(enc, rows)| (Location::Archive(path), from_mods, Some(enc), rows))
            }
        };
        let (location, from_mods, encryption, mut rows) = match shown {
            Ok(s) => s,
            Err(e) => {
                self.status = Status::Error(e);
                return;
            }
        };
        sort_rows(&mut rows, &self.settings);
        // In a mods copy, mark what differs from the original game files.
        if from_mods && let Location::Archive(path) = &location {
            let modified = std::fs::metadata(&path.disk).and_then(|m| m.modified()).ok();
            match self.changes_cache.iter().find(|(l, t, _)| l == &location && *t == modified) {
                Some((_, _, changes)) => mark_changes(&mut rows, changes),
                None => {
                    let (modded, original, keys) = (path.clone(), self.original_archive(path), self.keys.clone());
                    let task = Task::spawn("Comparing with the original", &self.ctx, move || {
                        vfs::compare_folder(&modded, &original, keys.as_deref()).map_err(|e| e.to_string())
                    });
                    self.changes_task = Some((location.clone(), task));
                }
            }
        }

        // Keep the preview while it belongs to this folder (or its mods copy).
        let here = Some(self.original_of(&location));
        let same = self.preview.as_ref().map(|p| match &p.source {
            PreviewSource::Archive(a) => a.parent().map(|d| self.original_of(&Location::Archive(d))) == here,
            PreviewSource::Disk(d) => d.parent().map(|d| Location::Folder(d.to_path_buf())) == here,
        });
        match (same, &mut self.preview, &location) {
            (Some(false), _, _) => self.preview = None,
            (Some(true), Some(p), Location::Archive(a)) => p.source = PreviewSource::Archive(a.join(&p.name)),
            _ => {}
        }

        let previous = self.browser.take().filter(|b| b.location == location);
        let moved = previous.is_none();
        let (filter, selected) = previous.map(|b| (b.filter, b.selected)).unwrap_or_default();
        let selected = selected.into_iter().filter(|s| rows.iter().any(|r| &r.name == s)).collect();
        self.browser = Some(Browser {
            location,
            from_mods,
            via_original: redirected,
            encryption,
            rows,
            selected,
            anchor: None,
            filter,
            focus_filter: false,
            blur_filter: moved,
            last_click: None,
            scroll_to: None,
        });
    }

    fn folder_rows(&self, dir: &Path) -> Result<Vec<Row>, String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut rows = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let meta = std::fs::metadata(&path).ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let is_archive = !is_dir && name.to_ascii_lowercase().ends_with(".rpf");
            let kind = if is_dir {
                RowKind::Folder
            } else if is_archive {
                RowKind::Archive
            } else {
                RowKind::File
            };
            let modded = !is_dir && self.has_mods_copy(&path);
            rows.push(Row { name, kind, size: meta.map_or(0, |m| m.len()), modded, added: false });
        }
        Ok(rows)
    }

    fn archive_rows(&self, path: &ArchivePath) -> Result<(Encryption, Vec<Row>), String> {
        let (archive, dir) = path.open(self.keys.as_deref()).map_err(|e| format!("{}: {e}", path.disk.display()))?;
        let rows = archive
            .children(dir)
            .map(|i| {
                let e = &archive.entries[i];
                let kind = match e.kind {
                    EntryKind::Directory { .. } => RowKind::Folder,
                    EntryKind::Binary { .. } if e.is_archive() => RowKind::Archive,
                    EntryKind::Binary { .. } => RowKind::File,
                    EntryKind::Resource { .. } => RowKind::Resource(e.resource_version().unwrap()),
                };
                Row { name: e.name.clone(), kind, size: e.size(), modded: false, added: false }
            })
            .collect();
        Ok((archive.encryption, rows))
    }

    fn has_mods_copy(&self, path: &Path) -> bool {
        let Some(game) = &self.game else { return false };
        if self.show_originals {
            return false;
        }
        match path.strip_prefix(&game.root) {
            Ok(rel) if !rel.starts_with("mods") => game::resolve_ci(&game.mods_dir(), &rel.to_string_lossy()).is_file(),
            _ => false,
        }
    }

    /// The same location in the game folder when it points into the mods folder.
    fn original_of(&self, location: &Location) -> Location {
        let Some(game) = &self.game else { return location.clone() };
        match location {
            // Packs only in the mods folder (add-ons) have no original.
            Location::Archive(a) => match a.disk.strip_prefix(game.mods_dir()) {
                Ok(rel) if game.root.join(rel).is_file() => Location::Archive(ArchivePath { disk: game.root.join(rel), inner: a.inner.clone() }),
                _ => location.clone(),
            },
            Location::Folder(_) => location.clone(),
        }
    }

    /// The location as the user opened it: the original archive when the
    /// browser shows its mods copy in its place.
    fn opened(&self, b: &Browser) -> Location {
        if b.via_original { self.original_of(&b.location) } else { b.location.clone() }
    }

    /// A place inside the archive shown, as the user would reach it: inside
    /// the original when that is what was opened.
    fn within(&self, b: &Browser, target: Location) -> Location {
        match (&b.location, &target) {
            (Location::Archive(here), Location::Archive(there)) if b.via_original && here.disk == there.disk => self.original_of(&target),
            _ => target,
        }
    }

    fn original_archive(&self, path: &ArchivePath) -> ArchivePath {
        match self.original_of(&Location::Archive(path.clone())) {
            Location::Archive(a) => a,
            Location::Folder(_) => unreachable!(),
        }
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
        if let (Some(name), Some(p)) = (file, &self.preview)
            && p.name != name
        {
            actions.push(Action::Preview(name));
        }
    }

    /// Texture dictionaries next to a model, named after it (`name.ytd`, and
    /// `name+hi.ytd` as vehicles have).
    /// The texture dictionaries a model draws from: those of its name next to
    /// it, then the ones its dictionary inherits from (up to vehshare.ytd).
    /// Tuning parts (in "<vehicle>_mods.rpf") use their vehicle's.
    fn sibling_textures(&self, model: &str) -> Vec<TextureDictionary> {
        let Some(b) = &self.browser else { return Vec::new() };
        let stem = model.rsplit_once('.').map_or(model, |(s, _)| s).to_ascii_lowercase();
        let read_here = |name: &str| match &b.location {
            Location::Folder(dir) => std::fs::read(dir.join(name)).ok(),
            Location::Archive(a) => a.join(name).read(self.keys.as_deref()).ok(),
        };
        let here = |name: &str| b.rows.iter().find(|r| r.name.eq_ignore_ascii_case(&format!("{name}.ytd"))).map(|r| r.name.clone());
        let mut names = vec![stem.clone(), format!("{stem}+hi")];
        if let Location::Archive(a) = &b.location {
            let archive = a.inner.iter().rev().chain(a.disk.file_name().map(|n| n.to_str().unwrap_or_default().to_string()).iter()).find(|p| p.to_ascii_lowercase().ends_with("_mods.rpf")).cloned();
            if let Some(vehicle) = archive.map(|n| n.to_ascii_lowercase().trim_end_matches("_mods.rpf").to_string()) {
                names.extend([vehicle.clone(), format!("{vehicle}+hi")]);
            }
        }
        // The model's own dictionaries come from next to it; shared ones from
        // where the game loads them last (a DLC may patch them), else next to it.
        let own = names.len();
        for name in names.clone() {
            for parent in self.shared.parents_of(&name) {
                if !names.contains(&parent) {
                    names.push(parent);
                }
            }
        }
        let shared = |name: &str| self.shared.place(name).and_then(|p| p.read(self.keys.as_deref()).ok());
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

    fn preview_dirty(&self) -> bool {
        self.preview.as_ref().and_then(|p| p.text.as_ref()).is_some_and(|(text, saved)| text != saved)
    }

    /// Whether `action` would close the preview and lose its edits.
    fn discards_preview(&self, action: &Action) -> bool {
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
            _ => false,
        }
    }

    /// Runs an action, first asking about unsaved edits it would throw away.
    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        if self.preview_dirty() && self.discards_preview(&action) {
            self.pending = Some(action);
            return;
        }
        self.run(action, ctx);
    }

    fn run(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::Unsaved(choice) => {
                let pending = self.pending.take();
                match choice {
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
                    let mut run = || -> Result<Vec<String>, paleto_core::Error> {
                        let mut log = match &tool {
                            Some(tool) => package.convert_legacy(tool, Some(&keys))?,
                            None => Vec::new(),
                        };
                        log.extend(package.install(&game, &keys)?);
                        Ok(log)
                    };
                    Outcome::Installed(run().map_err(|e| e.to_string()))
                }));
            }
            Action::CloseInstall => self.install = None,
            Action::Go(location) => self.go(location),
            Action::Reveal(path) => {
                let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
                self.go(Location::Folder(dir.to_path_buf()));
                if let Some(b) = &mut self.browser {
                    let name = name.to_string_lossy().into_owned();
                    if let Some(pos) = b.rows.iter().position(|r| r.name == name) {
                        b.selected = BTreeSet::from([name]);
                        b.anchor = Some(pos);
                        b.scroll_to = Some(pos);
                    }
                }
            }
            Action::Back => {
                if let Some(location) = self.back.pop() {
                    if let Some(b) = &self.browser {
                        self.forward.push(self.opened(b));
                    }
                    self.reveal = true;
                    self.show(location);
                }
            }
            Action::Forward => {
                if let Some(location) = self.forward.pop() {
                    if let Some(b) = &self.browser {
                        self.back.push(self.opened(b));
                    }
                    self.reveal = true;
                    self.show(location);
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
                let Some(b) = &self.browser else { return };
                // Only what the file list does not say: its type and size are there.
                let mut info = String::new();
                let (source, data) = match &b.location {
                    Location::Folder(dir) => {
                        let path = dir.join(&name);
                        let data = std::fs::metadata(&path)
                            .ok()
                            .filter(|m| m.len() as usize <= MAX_TEXT || binary_preview(&name))
                            .map(|_| std::fs::read(&path).map_err(|e| e.to_string()));
                        add_info(&mut info, "read-only");
                        (PreviewSource::Disk(path), data.unwrap_or(Ok(Vec::new())))
                    }
                    Location::Archive(a) => {
                        let path = a.join(&name);
                        let data = path.read(self.keys.as_deref()).map_err(|e| e.to_string());
                        (PreviewSource::Archive(path), data)
                    }
                };
                let (mut text, mut textures, mut model, mut audio) = (None, None, None, None);
                let mut codec = Codec::Plain;
                let mut syntax = Syntax::for_file(&name);
                let ext = extension(&name).to_ascii_lowercase();
                let fail = |e: paleto_formats::Error| Status::Error(format!("{name}: {e}"));
                match data {
                    Ok(data) if is_ytd(&name) => match TextureDictionary::parse(&data) {
                        Ok(dict) => {
                            let view = TextureView::new(dict);
                            add_info(&mut info, view.summary());
                            textures = Some(view);
                        }
                        Err(e) => self.status = fail(e),
                    },
                    Ok(data) if matches!(ext.as_str(), "ydr" | "yft" | "ydd") => match paleto_formats::drawable::parse(&data, &ext, &self.sibling_textures(&name)) {
                        Ok(drawables) => {
                            let view = ModelView::new(drawables);
                            add_info(&mut info, view.summary());
                            model = Some(view);
                        }
                        Err(e) => self.status = fail(e),
                    },
                    Ok(data) if ext == "awc" => {
                        let key = self.keys.as_ref().and_then(|k| k.awc);
                        match paleto_formats::awc::parse(&data, key.as_ref()) {
                            Ok(sounds) => {
                                let view = AudioView::new(sounds);
                                add_info(&mut info, view.summary());
                                audio = Some(view);
                            }
                            Err(e) => self.status = fail(e),
                        }
                    }
                    Ok(data) if ext == "gxt2" => match paleto_formats::gxt2::parse(&data) {
                        Ok(entries) => {
                            let t = paleto_formats::gxt2::to_text(&entries);
                            add_info(&mut info, format!("{} texts, shown as 0xHASH = text", entries.len()));
                            text = Some((t.clone(), t));
                            codec = Codec::Gxt2;
                        }
                        Err(e) => self.status = fail(e),
                    },
                    Ok(data) if MetaFile::is_meta(&data) => match MetaFile::parse(&data) {
                        Ok(meta) => {
                            let t = meta.to_xml();
                            add_info(&mut info, format!("{}, shown as XML", meta.kind()));
                            text = Some((t.clone(), t));
                            codec = Codec::Meta(Arc::new(meta));
                            syntax = Syntax::Xml;
                        }
                        Err(e) => self.status = fail(e),
                    },
                    Ok(data) => text = as_text(&data).map(|t| (t.clone(), t)),
                    Err(e) => self.status = Status::Error(e),
                }
                self.preview = Some(Preview { source, name, syntax, info, text, codec, textures, model, audio, find: None });
            }
            Action::ClosePreview => self.preview = None,
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
                        for &i in &indices {
                            let path = match &dest {
                                ExportDest::File(path) => path.clone(),
                                ExportDest::Folder(dir) => dir.join(format!("{}.wav", audio::file_stem(&sounds[i]))),
                            };
                            std::fs::write(&path, sounds[i].to_wav()).map_err(|e| format!("{}: {e}", path.display()))?;
                        }
                        Ok(match &dest {
                            ExportDest::File(path) => format!("exported {}", path.display()),
                            ExportDest::Folder(dir) => format!("{} sounds exported to {}", indices.len(), dir.display()),
                        })
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
                        let name = textures::file_name(&dict.textures[selected], png);
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
                        for &i in &indices {
                            let tex = &dict.textures[i];
                            let path = match &dest {
                                ExportDest::File(path) => path.clone(),
                                ExportDest::Folder(dir) => dir.join(textures::file_name(tex, png)),
                            };
                            textures::write(tex, png, &path)?;
                        }
                        Ok(match &dest {
                            ExportDest::File(path) => format!("exported {}", path.display()),
                            ExportDest::Folder(dir) => format!("{} textures exported to {}", indices.len(), dir.display()),
                        })
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
                let Some(p) = &mut self.preview else { return };
                let PreviewSource::Archive(path) = &p.source else { return };
                let Some((text, saved)) = &mut p.text else { return };
                // Converted text (XML, text tables) is checked before anything is saved.
                let data = match p.codec.encode(text) {
                    Ok(data) => data,
                    Err(e) => {
                        self.status = Status::Error(format!("not saved: {e}"));
                        self.after_task = None;
                        return;
                    }
                };
                *saved = text.clone();
                let (path, name) = (path.clone(), p.name.clone());
                self.spawn_edit("Saving", ctx, path.parent().unwrap(), move |editor, folder| {
                    editor.add_file(&vfs::join_inner(folder, &name), data)?;
                    Ok(format!("saved {name}"))
                });
            }
            Action::Extract(names) => {
                let Some(Location::Archive(location)) = self.browser.as_ref().map(|b| b.location.clone()) else { return };
                let Some(dest) = rfd::FileDialog::new().set_title("Extract to").pick_folder() else { return };
                let keys = self.keys.clone();
                self.task = Some(Task::spawn("Extracting", ctx, move || match extract(&location, keys.as_deref(), &names, &dest) {
                    Ok(n) => Outcome::Done { message: format!("{n} files extracted to {}", dest.display()), reload: None },
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
                    let mut converted = 0;
                    for (dlc, name) in &dialog.packs {
                        match paleto_core::addon::install_pack(&game, keys.as_deref(), dlc, name, tool.as_deref()) {
                            Ok(log) => converted += log.iter().filter(|l| l.starts_with("converted ")).count(),
                            Err(e) => return Outcome::Failed(format!("{name}: {e}")),
                        }
                    }
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
                self.preview = None;
                self.task = Some(Task::spawn("Deleting unchanged copies", ctx, move || match paleto_core::cleanup::delete(&game, &copies) {
                    Ok(n) => Outcome::Done { message: format!("deleted {n} copies of game files from the mods folder"), reload: None },
                    Err(e) => Outcome::Failed(e.to_string()),
                }));
                self.after_task = Some(Action::Refresh);
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
            Action::AskRename(name) => self.name_dialog = Some(NameDialog { renaming: Some(name.clone()), name, focus: true }),
            Action::AskNewFolder => self.name_dialog = Some(NameDialog { renaming: None, name: "new folder".into(), focus: true }),
            Action::Rename { from, to } => {
                if self.preview.as_ref().is_some_and(|p| p.name == from) {
                    self.preview = None;
                }
                if let Some(dir) = self.disk_folder() {
                    self.select_after = Some(to.clone());
                    self.task = Some(Task::spawn("Renaming", ctx, move || {
                        let target = dir.join(&to);
                        // Names differing only in case are the same item for the game.
                        let clash = !from.eq_ignore_ascii_case(&to) && std::fs::symlink_metadata(&target).is_ok();
                        match if clash { Err(std::io::ErrorKind::AlreadyExists.into()) } else { std::fs::rename(dir.join(&from), &target) } {
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
                    self.task = Some(Task::spawn("Creating folder", ctx, move || match std::fs::create_dir(dir.join(&name)) {
                        Ok(()) => Outcome::Done { message: format!("created folder {name}"), reload: None },
                        Err(e) => Outcome::Failed(format!("creating {name}: {e}")),
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
                if self.preview.as_ref().is_some_and(|p| names.contains(&p.name)) {
                    self.preview = None;
                }
                if let Some(dir) = self.disk_folder() {
                    self.task = Some(Task::spawn("Deleting", ctx, move || {
                        for name in &names {
                            let path = dir.join(name);
                            let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                            if let Err(e) = if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) } {
                                return Outcome::Failed(format!("deleting {name}: {e}"));
                            }
                        }
                        Outcome::Done { message: format!("{} deleted", names.len()), reload: None }
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
                self.tree.clear();
                if let Some(b) = &self.browser {
                    let location = self.original_of(&b.location);
                    self.show(location);
                }
            }
            Action::ExpandAll => {
                if let Some(game) = &self.game {
                    self.open_dirs.extend(subfolders(&game.root));
                }
            }
            Action::CollapseAll => self.open_dirs.clear(),
            Action::ShowInfo(info) => self.info = info,
            Action::CheckMods(open) => {
                self.mods_check = match (&self.game, open) {
                    (Some(game), true) => Some(ModsCheck::gather(game, self.keys.clone(), &self.ctx)),
                    _ => None,
                };
            }
            Action::ToggleMod(part, enabled) => {
                if self.busy() {
                    self.status = Status::Error("Wait for the current operation to finish, then try again.".into());
                    return;
                }
                let Some(game) = self.game.clone() else { return };
                // Files are about to move: an open preview may point at one.
                self.preview = None;
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
                self.export_task = Some(Task::spawn("Comparing the mods folder with the game's files", ctx, move || {
                    paleto_core::export::changed_files_cached_at(&game, keys.as_deref(), &paleto_core::export::ChangesCache::default_path()).map_err(|e| e.to_string())
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
                self.preview = None;
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
                self.preview = None;
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
                let Some(tool) = paleto_core::convert::converter() else { return };
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
                    self.tree.clear();
                }
                settings.apply(ctx);
                self.settings = settings;
                if let Some(b) = &mut self.browser {
                    sort_rows(&mut b.rows, &self.settings);
                }
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
    fn spawn_add(&mut self, ctx: &egui::Context, location: ArchivePath, paths: Vec<PathBuf>, tool: Option<PathBuf>) {
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

    fn spawn_edit(
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
    fn refresh(&mut self) {
        self.tree.clear();
        let Some(b) = &self.browser else { return };
        let location = match self.opened(b) {
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
        self.show(location);
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
        if let Some((location, task)) = &self.changes_task
            && let Some(result) = task.poll()
        {
            let location = location.clone();
            self.changes_task = None;
            if let (Ok(changes), Location::Archive(path)) = (result, &location) {
                let modified = std::fs::metadata(&path.disk).and_then(|m| m.modified()).ok();
                let changes = Arc::new(changes);
                self.changes_cache.retain(|(l, _, _)| l != &location);
                self.changes_cache.push((location.clone(), modified, changes.clone()));
                if self.changes_cache.len() > 64 {
                    self.changes_cache.remove(0);
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
                        let msg = format!("Installed successfully ({} changes)", changes.len());
                        desktop_notify(&label, &msg);
                    }
                    Outcome::Installed(Err(err)) => {
                        desktop_notify(&label, &format!("Installation failed: {err}"));
                    }
                    Outcome::Failed(err) => {
                        desktop_notify(&label, &format!("Failed: {err}"));
                    }
                }
            }

            self.task = None;
            match outcome {
                Outcome::Done { message, reload } => {
                    self.status = Status::Info(message);
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
                    match reload {
                        Some(location) => {
                            self.tree.clear();
                            self.show(Location::Archive(location));
                        }
                        // The task may have changed what is shown (added packs, moved or deleted files).
                        None => self.refresh(),
                    }
                    if let (Some(name), Some(b)) = (self.select_after.take(), &mut self.browser)
                        && let Some(pos) = b.rows.iter().position(|r| r.name == name)
                    {
                        b.selected = BTreeSet::from([name]);
                        b.anchor = Some(pos);
                        b.scroll_to = Some(pos);
                    }
                    if let (Some(selected), Some(name)) = (self.refresh_preview.take(), self.previewing().map(str::to_string)) {
                        let status = std::mem::replace(&mut self.status, Status::Info(String::new()));
                        self.apply(Action::Preview(name), ctx);
                        if let Some(view) = self.preview.as_mut().and_then(|p| p.textures.as_mut())
                            && let Some(i) = view.dict.textures.iter().position(|t| t.name.eq_ignore_ascii_case(&selected))
                        {
                            view.select(i);
                        }
                        if let Some(view) = self.preview.as_mut().and_then(|p| p.audio.as_mut())
                            && let Ok(id) = u32::from_str_radix(&selected, 16)
                        {
                            view.select_id(id);
                        }
                        if matches!(self.status, Status::Info(_) | Status::Note(..)) {
                            self.status = status;
                        }
                    }
                }
                Outcome::Installed(result) => {
                    self.tree.clear();
                    if let Some(b) = &self.browser {
                        let location = self.opened(b);
                        self.show(location);
                    }
                    if let Some(d) = &mut self.install {
                        d.result = Some(result);
                    }
                }
                Outcome::Failed(e) => {
                    self.status = Status::Error(e);
                    self.select_after = None;
                    self.refresh_preview = None;
                    // Do not carry on (e.g. quit) after a failed save.
                    self.after_task = None;
                }
            }
        }
    }

    fn listing(&mut self, dir: &Path) -> &[FsEntry] {
        if !self.tree.contains_key(dir) {
            let all_files = self.settings.tree_all_files;
            let mut entries: Vec<FsEntry> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let path = e.path();
                    let name = e.file_name().to_string_lossy().into_owned();
                    let is_dir = path.is_dir();
                    let is_archive = name.to_ascii_lowercase().ends_with(".rpf");
                    if name.starts_with('.') || (!is_dir && !is_archive && !all_files) {
                        return None;
                    }
                    let modded = !is_dir && self.has_mods_copy(&path);
                    Some(FsEntry { path, name, is_dir, modded })
                })
                .collect();
            entries.sort_by_key(|e| (!e.is_dir, e.name.to_ascii_lowercase()));
            self.tree.insert(dir.to_path_buf(), entries);
        }
        &self.tree[dir]
    }

    // ---- UI ----------------------------------------------------------------

    fn icon(&self, class: FileClass, dark: bool) -> RichText {
        let text = RichText::new(class.icon());
        if self.settings.colored_icons { text.color(class.color(dark)) } else { text }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let can_install = self.keys.is_some();
        // Menu titles as tall as the bar's buttons, their text centred.
        let height = crate::widgets::bar_height(ui);
        let text = ui.text_style_height(&egui::TextStyle::Button);
        let padding = egui::vec2(8.0, ((height - text) / 2.0).max(0.0));
        let style = move |s: &mut egui::Style| {
            egui::containers::menu::menu_style(s);
            s.spacing.button_padding = padding;
        };
        egui::MenuBar::new().style(style).ui(ui, |ui| {
            ui.set_min_height(height);
            let has_game = self.game.is_some();
            let need_game = "Choose the game folder first (Game menu)";
            let need_keys = if has_game { "Reading the game's encryption keys…" } else { need_game };
            let file = menu_title(ui, "File", |ui| {
                if ui.button("Open archive…").on_hover_text("Browse an .rpf archive, also outside the game folder").clicked() {
                    actions.push(Action::PickArchive);
                }
                ui.separator();
                if ui
                    .add_enabled(can_install, egui::Button::new("Export mods as an OIV package…"))
                    .on_hover_text("Pack your changes to the game's files into one .oiv, to share them or install them again later")
                    .on_disabled_hover_text(need_keys)
                    .clicked()
                {
                    actions.push(Action::Export(true));
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    actions.push(Action::Quit);
                }
            });
            let game = menu_title(ui, "Game", |ui| {
                if !self.installs.is_empty() {
                    ui.label(RichText::new("Installed games").weak());
                }
                for game in &self.installs {
                    let current = self.game.as_ref().is_some_and(|g| g.root == game.root);
                    let folder = game.root.file_name().map_or_else(|| game.root.display().to_string(), |n| n.to_string_lossy().into_owned());
                    if ui.selectable_label(current, format!("{:?}: {folder}", game.edition)).on_hover_text(game.root.display().to_string()).clicked() {
                        actions.push(Action::SelectGame(game.root.clone()));
                    }
                }
                if ui.button("Choose the game folder…").on_hover_text("The folder holding GTA5.exe or GTA5_Enhanced.exe").clicked() {
                    actions.push(Action::PickGame);
                }
            });
            let tools = menu_title(ui, "Tools", |ui| {
                if ui
                    .add_enabled(has_game, egui::Button::new("Mods in game…"))
                    .on_hover_text("Every mod the game folder holds, whether the game loads it, and what is wrong with the setup")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::CheckMods(true));
                }
                if ui
                    .add_enabled(has_game, egui::Button::new("Snapshots of the mods folder…"))
                    .on_hover_text("Save the mods folder as it is now, and go back to it if a change breaks the game")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::Snapshots(true));
                }
                ui.separator();
                if ui
                    .add_enabled(has_game, egui::Button::new("Crash detection…"))
                    .on_hover_text("Why the game crashed, which mod is likely to blame, and a search that finds it")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::CrashDetection(true));
                }
                ui.separator();
                if ui
                    .add_enabled(has_game, egui::Button::new("Script Hook V…"))
                    .on_hover_text("The installed and the latest Script Hook V, whether it suits the game, and the update")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::ScriptHook(true));
                }
            });
            let view = menu_title(ui, "View", |ui| {
                let mut s = self.settings.clone();
                ui.label(RichText::new("Theme").weak());
                for (value, label) in Theme::ALL {
                    ui.radio_value(&mut s.theme, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Text and icon size").weak());
                for (value, label) in TextSize::ALL {
                    ui.radio_value(&mut s.text_size, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Density").weak());
                for (value, label) in Density::ALL {
                    ui.radio_value(&mut s.density, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Folders and files").weak());
                for (value, label) in Grouping::ALL {
                    ui.radio_value(&mut s.grouping, value, label);
                }
                ui.separator();
                ui.checkbox(&mut s.colored_icons, "Coloured icons");
                ui.checkbox(&mut s.show_tree, "Side tree");
                ui.add_enabled(s.show_tree, egui::Checkbox::new(&mut s.tree_all_files, "Show all files in the side tree"));
                ui.checkbox(&mut s.remember_open_folders, "Remember open folders");
                if s != self.settings {
                    actions.push(Action::Settings(s));
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.show_originals.clone(), "Show original files (ignore mods folder)")
                    .on_hover_text("Show the game's own files where the mods folder has a modified copy")
                    .clicked()
                {
                    actions.push(Action::ToggleOriginals);
                }
                ui.separator();
                if ui.add_enabled(self.settings.show_tree, egui::Button::new("Expand all folders")).clicked() {
                    actions.push(Action::ExpandAll);
                }
                if ui.add_enabled(self.settings.show_tree, egui::Button::new("Collapse all folders")).clicked() {
                    actions.push(Action::CollapseAll);
                }
                if ui.add(egui::Button::new("Refresh").shortcut_text("F5")).clicked() {
                    actions.push(Action::Refresh);
                }
            });
            let help = menu_title(ui, "Help", |ui| {
                if ui.button("Getting started").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::GettingStarted)));
                }
                if ui.button("Keyboard and mouse shortcuts").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::Shortcuts)));
                }
                ui.separator();
                ui.hyperlink_to("Online documentation", format!("{REPO_URL}/tree/main/docs"));
                ui.hyperlink_to("Report a problem", format!("{REPO_URL}/issues"));
                ui.separator();
                if ui.button("About").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::About)));
                }
            });
            switch_menus(ui.ctx(), &[file, game, tools, view, help]);
            let narrow = ui.ctx().content_rect().width() < 760.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Like the other bars, not flat as menu titles.
                ui.reset_style();
                // The main action, at the far right.
                if can_install {
                    let label = if narrow { INSTALL_SHORT } else { INSTALL };
                    // In the accent colour, to stand out.
                    let selection = ui.visuals().selection;
                    let install = egui::Button::new(RichText::new(label).color(selection.stroke.color)).fill(selection.bg_fill);
                    // Short on a narrow window, where it saves room.
                    let width = if narrow { 0.0 } else { right_width(ui) };
                    if crate::widgets::bar_button(ui, install, width).on_hover_text("Install an OpenIV package (.oiv) into the mods folder").clicked() {
                        actions.push(Action::PickPackage);
                    }
                }
                // The Wine prefix, beside it.
                if self.game.is_some() {
                    let label = if narrow { TWEAKS_SHORT } else { TWEAKS };
                    let width = if narrow { 0.0 } else { right_width(ui) };
                    // Wine red: not the accent of Install, but not a plain button either.
                    let wine = if ui.visuals().dark_mode { egui::Color32::from_rgb(0x7A, 0x2B, 0x38) } else { egui::Color32::from_rgb(0xA4, 0x3A, 0x48) };
                    let tweaks = egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(wine);
                    if crate::widgets::bar_button(ui, tweaks, width)
                        .on_hover_text("Changes to the game's Wine prefix that mods need, e.g. .NET for ScriptHookVDotNet mods")
                        .clicked()
                    {
                        actions.push(Action::PrefixTweaks(true));
                    }
                }
                // The filter, centred in the window when there is room for it.
                let Some(b) = self.browser.as_mut() else { return };
                let gap = 2.0 * ui.spacing().item_spacing.x;
                // This layout starts where the menus end.
                let (left, right) = (ui.max_rect().left() + gap, ui.available_rect_before_wrap().right() - gap);
                let width = (right - left).min(260.0);
                if width < 80.0 {
                    return;
                }
                let x = (ui.ctx().content_rect().center().x - width / 2.0).clamp(left, right - width);
                let rect = egui::Rect::from_min_size(egui::pos2(x, ui.max_rect().center().y - height / 2.0), egui::vec2(width, height));
                filter_box(ui, b, rect);
            });
        });
    }

    fn side_tree(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(root) = self.game.as_ref().map(|g| g.root.clone()) else { return };
        let current = self.tree_selection();
        let root_selected = current.as_deref() == Some(root.as_path());
        // Two buttons sharing the width, as tall as the file toolbar beside them.
        ui.horizontal(|ui| {
            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            if crate::widgets::bar_button(ui, egui::Button::new("⏷ Expand all"), width).on_hover_text("Expand every folder").clicked() {
                actions.push(Action::ExpandAll);
            }
            if crate::widgets::bar_button(ui, egui::Button::new("⏶ Collapse all"), width).on_hover_text("Collapse every folder").clicked() {
                actions.push(Action::CollapseAll);
            }
        });
        ui.separator();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.icon(FileClass::Folder, ui.visuals().dark_mode));
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.selectable_label(root_selected, RichText::new(name).strong()).clicked() {
                    actions.push(Action::Go(Location::Folder(root.clone())));
                }
            });
            self.tree_dir(ui, &root, current.as_deref(), actions);
        });
        self.reveal = false;
    }

    /// One folder of the side tree: clicking a name opens it, the arrow
    /// expands it.
    fn tree_dir(&mut self, ui: &mut egui::Ui, dir: &Path, current: Option<&Path>, actions: &mut Vec<Action>) {
        let entries: Vec<(PathBuf, String, bool, bool)> =
            self.listing(dir).iter().map(|e| (e.path.clone(), e.name.clone(), e.is_dir, e.modded)).collect();
        for (path, name, is_dir, modded) in entries {
            let selected = current == Some(path.as_path());
            if is_dir {
                // Open/closed state lives in `open_dirs`, so it can be expanded,
                // collapsed and remembered as a whole.
                let id = egui::Id::unique(("tree", &path));
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                if self.reveal && current.is_some_and(|c| c.starts_with(&path) && c != path) {
                    self.open_dirs.insert(path.clone());
                }
                state.set_open(self.open_dirs.contains(&path));
                let icon = self.icon(FileClass::Folder, ui.visuals().dark_mode);
                state
                    .show_header(ui, |ui| {
                        ui.label(icon);
                        if ui.selectable_label(selected, &name).clicked() {
                            actions.push(Action::Go(Location::Folder(path.clone())));
                        }
                    })
                    .body(|ui| self.tree_dir(ui, &path, current, actions));
                if egui::collapsing_header::CollapsingState::load(ui.ctx(), id).is_some_and(|s| s.is_open()) {
                    self.open_dirs.insert(path);
                } else {
                    self.open_dirs.remove(&path);
                }
            } else {
                let class = FileClass::of(&name);
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    ui.label(self.icon(class, ui.visuals().dark_mode));
                    let text = if modded { RichText::new(&name).color(theme::modded(ui.visuals().dark_mode)) } else { RichText::new(&name) };
                    let response = ui.selectable_label(selected, text);
                    let response = if modded {
                        let link = ui.label(RichText::new(MODS_LINK).color(theme::modded(ui.visuals().dark_mode)));
                        link.on_hover_text(MODS_LINK_HOVER);
                        response.on_hover_text(MODS_LINK_HOVER)
                    } else {
                        response
                    };
                    if response.clicked() {
                        actions.push(if class == FileClass::Archive {
                            Action::Go(Location::Archive(ArchivePath::new(path.clone())))
                        } else {
                            Action::Reveal(path.clone())
                        });
                    }
                    if selected && self.reveal {
                        response.scroll_to_me(None);
                    }
                });
            }
        }
    }

    /// The clickable segments of the location bar.
    fn breadcrumbs(&self, b: &Browser) -> Vec<(String, Location)> {
        let location = &b.location;
        let root = self.game.as_ref().map(|g| g.root.clone());
        let (disk, inner) = match location {
            Location::Folder(p) => (p.clone(), None),
            Location::Archive(a) => (a.disk.clone(), Some(a)),
        };
        let mut out = Vec::new();
        match root.as_ref().and_then(|r| disk.strip_prefix(r).ok().map(|rel| (r, rel))) {
            Some((root, rel)) => {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                out.push((name, Location::Folder(root.clone())));
                let mut acc = root.clone();
                let parts: Vec<_> = rel.components().collect();
                for (i, part) in parts.iter().enumerate() {
                    acc.push(part);
                    let is_archive = inner.is_some() && i + 1 == parts.len();
                    let target = if is_archive { Location::Archive(ArchivePath::new(acc.clone())) } else { Location::Folder(acc.clone()) };
                    out.push((part.as_os_str().to_string_lossy().into_owned(), target));
                }
            }
            None => {
                let target = if inner.is_some() { Location::Archive(ArchivePath::new(disk.clone())) } else { Location::Folder(disk.clone()) };
                out.push((disk.display().to_string(), target));
            }
        }
        if let Some(a) = inner {
            for i in 0..a.inner.len() {
                let mut target = ArchivePath::new(a.disk.clone());
                target.inner = a.inner[..=i].to_vec();
                out.push((a.inner[i].clone(), Location::Archive(target)));
            }
        }
        out.into_iter().map(|(label, target)| (label, self.within(b, target))).collect()
    }

    /// The location bar across the window, with the package installer on the right.
    fn location_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(b) = &self.browser else { return };
        let crumbs = self.breadcrumbs(b);
        let (can_back, can_forward) = (!self.back.is_empty(), !self.forward.is_empty());
        let peeking = self.peek_original.is_some();
        // A mods copy of a game archive, opened through either.
        let has_original = matches!(&b.location, Location::Archive(_)) && self.original_of(&b.location) != b.location;
        let square = crate::widgets::bar_height(ui);
        // Each piece of text is as tall as the buttons, its text centred, so a
        // row it wraps onto is centred like the first.
        let width = |ui: &egui::Ui, text: egui::WidgetText| text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x + 2.0;
        // The switch at the right end, on the first row; the path wraps in the room left.
        ui.horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if has_original || peeking {
                let (label, hover) = if peeking {
                    (MODS_COPY, "Show the modified copy in the mods folder, which the game loads")
                } else {
                    (ORIGINAL, "Show the game's own archive, without the changes in the mods folder")
                };
                if crate::widgets::centred_bar_button(ui, label, right_width(ui)).on_hover_text(hover).clicked() {
                    actions.push(Action::PeekOriginal(!peeking));
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
            ui.set_min_height(square);
            let nav = |ui: &mut egui::Ui, enabled: bool, icon: &str| ui.add_enabled_ui(enabled, |ui| crate::widgets::bar_button(ui, egui::Button::new(icon), square)).inner;
            if nav(ui, can_back, "⏴").on_hover_text("Back (mouse back button, Alt+Left)").clicked() {
                actions.push(Action::Back);
            }
            if nav(ui, can_forward, "⏵").on_hover_text("Forward (mouse forward button, Alt+Right)").clicked() {
                actions.push(Action::Forward);
            }
            if nav(ui, crumbs.len() > 1, "⏶").on_hover_text("Up (Backspace)").clicked() {
                actions.push(Action::Go(crumbs[crumbs.len() - 2].1.clone()));
            }
            for (i, (label, target)) in crumbs.iter().enumerate() {
                if i > 0 {
                    ui.add_sized([width(ui, "/".into()), square], egui::Label::new("/"));
                }
                if ui.add_sized([width(ui, label.as_str().into()), square], egui::Link::new(label)).clicked() {
                    actions.push(Action::Go(target.clone()));
                }
            }
            if b.from_mods {
                let text = RichText::new("MODS").strong().color(theme::modded(ui.visuals().dark_mode));
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text)).on_hover_text("Showing the modified copy in the mods folder");
            }
            if peeking {
                let text = RichText::new("ORIGINAL").strong();
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text)).on_hover_text("Showing the game's own archive, not its copy in the mods folder");
            }
            if let Some(enc) = b.encryption {
                let text = RichText::new(format!("{enc:?}")).weak();
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text));
            }
            });
        }));
    }

    fn browser_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let Some(b) = &self.browser else {
            ui.centered_and_justified(|ui| ui.label("Select a folder or an archive on the left."));
            return;
        };
        let crumbs = self.breadcrumbs(b);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + self.settings.density.row_padding();
        let settings = self.settings.clone();
        let colored = settings.colored_icons;
        let modal_open = self.dialog_open();
        let b = self.browser.as_mut().unwrap();
        let in_archive = matches!(b.location, Location::Archive(_));

        // Toolbar.
        let selected: Vec<String> = b.selected.iter().cloned().collect();
        let filter_id = egui::Id::unique(FILTER_ID);
        // Icons only when the full labels do not fit; the names show on hover.
        const TOOLS: [&str; 6] = ["➕ Add files…", "📁 Add folder…", "🗀 New folder…", "✏ Rename…", "⬇ Extract…", "🗑 Delete"];
        let spacing = ui.spacing().item_spacing.x + 2.0 * ui.spacing().button_padding.x;
        let needed: f32 = TOOLS
            .iter()
            .map(|t| egui::WidgetText::from(*t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + spacing)
            .sum();
        let compact = needed > ui.available_width();
        // Wraps onto a second row when even the icons do not fit, so none is cut off.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let height = crate::widgets::bar_height(ui);
                let button = |icon: &str, name: &str| {
                    egui::Button::new(if compact { icon.to_string() } else { format!("{icon} {name}") }).min_size(egui::vec2(if compact { height } else { 0.0 }, height))
                };
                let only_archives = "Open an .rpf archive to add files to it";
                if ui.add_enabled(in_archive, button("➕", "Add files…")).on_hover_text("Add files").on_disabled_hover_text(only_archives).clicked() {
                    actions.push(Action::PickAdd { folders: false });
                }
                if ui.add_enabled(in_archive, button("📁", "Add folder…")).on_hover_text("Add a folder").on_disabled_hover_text(only_archives).clicked() {
                    actions.push(Action::PickAdd { folders: true });
                }
                if ui.add(button("🗀", "New folder…")).on_hover_text("New folder (Ctrl+Shift+N)").clicked() {
                    actions.push(Action::AskNewFolder);
                }
                if ui.add_enabled(selected.len() == 1, button("✏", "Rename…")).on_hover_text("Rename (F2)").clicked() {
                    actions.push(Action::AskRename(selected[0].clone()));
                }
                if ui
                    .add_enabled(in_archive && !selected.is_empty(), button("⬇", "Extract…"))
                    .on_hover_text("Extract the selection")
                    .on_disabled_hover_text("Select files inside an .rpf archive to extract them")
                    .clicked()
                {
                    actions.push(Action::Extract(selected.clone()));
                }
                if ui.add_enabled(!selected.is_empty(), button("🗑", "Delete")).on_hover_text("Delete the selection (Delete)").clicked() {
                    actions.push(Action::AskDelete(selected.clone()));
                }
            });
        });
        ui.separator();

        let filter = b.filter.to_ascii_lowercase();
        let visible: Vec<usize> = (0..b.rows.len()).filter(|&i| b.rows[i].name.to_ascii_lowercase().contains(&filter)).collect();

        // Keyboard: typing filters, arrows move, Enter opens.
        let filter_focused = ui.ctx().memory(|m| m.has_focus(filter_id));
        if !modal_open && (filter_focused || !ui.ctx().egui_wants_keyboard_input()) {
            let (typed, keys) = ui.input(|i| {
                let typed: String = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) if !i.modifiers.command => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                let k = |key| i.key_pressed(key);
                let alt = i.modifiers.alt;
                (
                    typed,
                    (k(egui::Key::ArrowDown), k(egui::Key::ArrowUp), k(egui::Key::Enter), k(egui::Key::Escape), k(egui::Key::Backspace) && !alt, k(egui::Key::Delete)),
                )
            });
            let (down, up, enter, escape, backspace, delete) = keys;
            if !filter_focused && !typed.is_empty() {
                b.filter.push_str(&typed);
                b.focus_filter = true;
                b.selected.clear();
            }
            let pos = b.anchor.filter(|&p| p < visible.len());
            if down || up {
                let next = match pos {
                    None => 0,
                    Some(p) if down => (p + 1).min(visible.len().saturating_sub(1)),
                    Some(p) => p.saturating_sub(1),
                };
                if let Some(&i) = visible.get(next) {
                    b.selected = BTreeSet::from([b.rows[i].name.clone()]);
                    b.anchor = Some(next);
                    b.scroll_to = Some(next);
                }
            }
            if enter {
                let target = if b.selected.len() == 1 { b.selected.first().cloned() } else { visible.first().map(|&i| b.rows[i].name.clone()) };
                if let Some(name) = target {
                    actions.push(Action::Enter(name));
                }
            }
            if escape && !b.filter.is_empty() {
                b.filter.clear();
                ui.ctx().memory_mut(|m| m.surrender_focus(filter_id));
            }
            if !filter_focused && backspace && crumbs.len() > 1 {
                actions.push(Action::Go(crumbs[crumbs.len() - 2].1.clone()));
            }
            if !filter_focused && delete && !b.selected.is_empty() && !busy {
                actions.push(Action::AskDelete(b.selected.iter().cloned().collect()));
            }
            let (rename, new_folder) =
                ui.input(|i| (i.key_pressed(egui::Key::F2), i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::N)));
            if !busy && rename && b.selected.len() == 1 {
                actions.push(Action::AskRename(b.selected.first().unwrap().clone()));
            }
            if !busy && new_folder {
                actions.push(Action::AskNewFolder);
            }
        }
        let filter = b.filter.to_ascii_lowercase();
        let visible: Vec<usize> = (0..b.rows.len()).filter(|&i| b.rows[i].name.to_ascii_lowercase().contains(&filter)).collect();
        if !filter.is_empty() && b.selected.is_empty() && !visible.is_empty() {
            b.selected.insert(b.rows[visible[0]].name.clone());
            b.anchor = Some(0);
            b.scroll_to = Some(0);
        }

        let header = |ui: &mut egui::Ui, label: &str, column: SortBy, actions: &mut Vec<Action>| {
            let arrow = match (settings.sort_by == column, settings.descending) {
                (true, false) => " ⏶",
                (true, true) => " ⏷",
                _ => "",
            };
            let response = ui.add(egui::Label::new(RichText::new(format!("{label}{arrow}")).strong()).sense(egui::Sense::click()));
            if response.clicked() {
                let mut s = settings.clone();
                s.descending = s.sort_by == column && !s.descending;
                s.sort_by = column;
                actions.push(Action::Settings(s));
            }
        };
        // Type and size give up room first when the list is narrow; the name keeps the rest.
        let width = ui.available_width();
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(110.0).clip(true))
            .column(Column::exact((width * 0.3).clamp(70.0, 130.0)).clip(true))
            .column(Column::exact((width * 0.2).clamp(62.0, 100.0)).clip(true));
        if let Some(row) = b.scroll_to.take() {
            table = table.scroll_to_row(row, None);
        }
        table
            .header(row_height, |mut h| {
                h.col(|ui| header(ui, "Name", SortBy::Name, actions));
                h.col(|ui| header(ui, "Type", SortBy::Type, actions));
                h.col(|ui| header(ui, "Size", SortBy::Size, actions));
            })
            .body(|body| {
                body.rows(row_height, visible.len(), |mut row| {
                    let pos = row.index();
                    let r = &b.rows[visible[pos]];
                    row.set_selected(b.selected.contains(&r.name));
                    let class = r.class();
                    let kind = match r.kind {
                        RowKind::Folder => "Folder".to_string(),
                        RowKind::Archive => "Archive".to_string(),
                        RowKind::File => extension(&r.name),
                        RowKind::Resource(v) => format!("{} (v{v})", extension(&r.name)),
                    };
                    row.col(|ui| {
                        let icon = RichText::new(class.icon());
                        ui.label(if colored { icon.color(class.color(ui.visuals().dark_mode)) } else { icon });
                        let name = RichText::new(&r.name);
                        let response = ui.label(if r.modded { name.color(theme::modded(ui.visuals().dark_mode)) } else { name });
                        if r.modded {
                            let hover = match (b.from_mods, r.added) {
                                (true, true) => "Not in the original game files",
                                (true, false) => "Different from the original game file",
                                (false, _) => MODS_LINK_HOVER,
                            };
                            // A game file the game reads from the mods folder instead.
                            if !b.from_mods {
                                ui.label(RichText::new(MODS_LINK).color(theme::modded(ui.visuals().dark_mode))).on_hover_text(hover);
                            }
                            response.on_hover_text(hover);
                        }
                    });
                    row.col(|ui| {
                        ui.label(kind);
                    });
                    row.col(|ui| {
                        if r.kind != RowKind::Folder {
                            ui.label(size(r.size));
                        }
                    });
                    let response = row.response();
                    if response.clicked() {
                        let (modifiers, now) = response.ctx.input(|i| (i.modifiers, i.time));
                        let delay = response.ctx.options(|o| o.input_options.max_double_click_delay);
                        let double = b.last_click.as_ref().is_some_and(|(name, t)| *name == r.name && now - t <= delay);
                        b.last_click = if double { None } else { Some((r.name.clone(), now)) };
                        if double {
                            actions.push(Action::Enter(r.name.clone()));
                        }
                        if modifiers.shift {
                            let anchor = b.anchor.unwrap_or(pos);
                            let (lo, hi) = (anchor.min(pos), anchor.max(pos));
                            if !modifiers.command {
                                b.selected.clear();
                            }
                            b.selected.extend((lo..=hi).map(|p| b.rows[visible[p]].name.clone()));
                        } else if modifiers.command {
                            if !b.selected.remove(&r.name) {
                                b.selected.insert(r.name.clone());
                            }
                            b.anchor = Some(pos);
                        } else {
                            b.selected = BTreeSet::from([r.name.clone()]);
                            b.anchor = Some(pos);
                        }
                    }
                    let name = r.name.clone();
                    let targets = if b.selected.contains(&name) { b.selected.iter().cloned().collect() } else { vec![name.clone()] };
                    let container = r.is_container();
                    response.context_menu(|ui| {
                        if container && ui.button("Open").clicked() {
                            actions.push(Action::Enter(name.clone()));
                        }
                        if !container && ui.button("Preview").clicked() {
                            actions.push(Action::Preview(name.clone()));
                        }
                        if in_archive && ui.add_enabled(!busy, egui::Button::new("Extract…")).clicked() {
                            actions.push(Action::Extract(targets.clone()));
                        }
                        if ui.add_enabled(!busy && targets.len() == 1, egui::Button::new("Rename…")).clicked() {
                            actions.push(Action::AskRename(name.clone()));
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Delete…")).clicked() {
                            actions.push(Action::AskDelete(targets.clone()));
                        }
                    });
                });
            });
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        let Some(p) = &mut self.preview else { return };
        let editable = matches!(p.source, PreviewSource::Archive(_));
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
                        view.ui(ui);
                    } else if let Some(view) = &mut p.audio {
                        if let Some(request) = view.ui(ui, busy, editable) {
                            actions.push(match request {
                                AudioRequest::Replace(id) => Action::ReplaceSound(id, None),
                                request => Action::ExportSounds(request, None),
                            });
                        }
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

    /// The archive folder being browsed, where dropped files go.
    /// The folder on disk being browsed, when not inside an archive.
    fn disk_folder(&self) -> Option<PathBuf> {
        match &self.browser.as_ref()?.location {
            Location::Folder(dir) => Some(dir.clone()),
            Location::Archive(_) => None,
        }
    }

    fn archive_folder(&self) -> Option<&ArchivePath> {
        match &self.browser.as_ref()?.location {
            Location::Archive(a) => Some(a),
            Location::Folder(_) => None,
        }
    }

    /// Shown while files are dragged over the window: what dropping them does here.
    fn drop_overlay(&self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::unique("drop-dim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
        });
        let ytd = self.preview.as_ref().filter(|p| p.textures.is_some() && matches!(p.source, PreviewSource::Archive(_))).map(|p| p.name.clone());
        let (title, detail) = match self.archive_folder() {
            Some(_) if let Some(ytd) = ytd => (
                format!("Replace textures in {ytd}"),
                "Dropping only DDS or PNG files (or folders of them) replaces the textures they are named after; other files are added to this archive folder.".to_string(),
            ),
            Some(folder) => {
                let name = folder.inner.last().cloned().unwrap_or_else(|| folder.disk.file_name().unwrap_or_default().to_string_lossy().into_owned());
                (format!("Add to {name}"), "Files are added to this archive folder; .oiv packages open the installer.".to_string())
            }
            None => (
                "Install an .oiv package".to_string(),
                "To add other files, first open a folder inside an archive (.rpf).".to_string(),
            ),
        };
        egui::Area::new(egui::Id::unique("drop-card"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(24.0).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("⬇").size(36.0).color(ui.visuals().hyperlink_color));
                        ui.heading(title);
                        ui.label(RichText::new(detail).weak());
                    });
                });
            });
    }

    fn welcome(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.add(egui::Image::from_bytes("bytes://paleto-modkit-icon.png", crate::ICON_PNG).fit_to_exact_size(egui::vec2(96.0, 96.0)));
            ui.heading(crate::NAME);
            ui.label("Choose your GTA V installation.");
            ui.add_space(16.0);
            for game in &self.installs {
                if ui.button(format!("{:?} — {}", game.edition, game.root.display())).clicked() {
                    actions.push(Action::SelectGame(game.root.clone()));
                }
            }
            if ui.button("Choose folder…").clicked() {
                actions.push(Action::PickGame);
            }
            if crate::sandboxed() {
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Flatpak: Steam libraries outside ~/.local/share/Steam, ~/Games, /mnt, /media and /run/media\n\
                         are not visible. Grant access to yours with\n\
                         flatpak override --user --filesystem=/path/to/SteamLibrary {}",
                        crate::APP_ID
                    ))
                    .weak(),
                );
            }
        });
    }

    fn install_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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
                    ui.label(format!("Version {}   ·   by {}", d.metadata.version, d.metadata.author));
                    ui.label(RichText::new(d.path.display().to_string()).weak().small());
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
                        Err(e) => ui.label(RichText::new(format!("✖ {e}")).color(theme::error(ui.visuals().dark_mode))),
                    };
                    if let Ok(log) = result {
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

    fn unsaved_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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

    fn info_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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
    fn name_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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
    fn texture_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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
            let (name, old) = match &mut d.edit {
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
    fn dropped_textures(&self, files: &[PathBuf]) -> bool {
        let editable_ytd = self.preview.as_ref().is_some_and(|p| p.textures.is_some() && matches!(p.source, PreviewSource::Archive(_)));
        editable_ytd
            && !files.is_empty()
            && files.iter().all(|f| texture_batch::is_texture_file(f) || f.is_dir())
            && !texture_batch::texture_files(files).is_empty()
    }

    fn texture_batch_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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

    fn addon_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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

    fn legacy_add_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
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

    fn delete_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(names) = &self.confirm_delete else { return };
        let dir = self.disk_folder();
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
                let what = if names.len() == 1 { "It is" } else { "They are" };
                ui.label(RichText::new(format!("{what} deleted from the disk for good, folders with all they hold.")).color(theme::warning(dark)));
                if game_files {
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
                if ui.add(crate::widgets::danger(ui, "Delete")).clicked() {
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

        // Closing the window with unsaved edits asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.preview_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Action::Quit);
        }
        let dialog = self.dialog_open();
        if !dialog && self.preview_dirty() && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
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
                        self.task.as_ref().map(|t| &t.label),
                        self.keys_task.as_ref().map(|t| &t.label),
                        self.copies_task.as_ref().map(|t| &t.label),
                        self.export_task.as_ref().map(|t| &t.label),
                    ];
                    if let Some(t) = running.into_iter().flatten().next() {
                        ui.spinner();
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
            if self.preview.is_some() {
                let preview_max = (ui.available_width() - LIST_MIN).max(240.0);
                egui::Panel::right("preview").resizable(true).default_size(460.0).size_range(240.0..=preview_max).show(ui, |ui| {
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
            self.startup.dismiss();
            match reply {
                notices::Reply::Update(release) => actions.push(Action::ScriptHookUpdate(release)),
                notices::Reply::OpenScriptHook => actions.push(Action::ScriptHook(true)),
                notices::Reply::OpenCrash => actions.push(Action::CrashDetection(true)),
                notices::Reply::Dismiss => {}
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

/// A menu title that stays highlighted while its menu is open, as the hovered one is.
fn menu_title<R>(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let key = egui::Id::unique(("menu-title-open", title));
    let was_open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let (response, open) = ui
        .scope(|ui| {
            if was_open {
                let hovered = ui.visuals().widgets.hovered.weak_bg_fill;
                ui.visuals_mut().widgets.inactive.weak_bg_fill = hovered;
            }
            let (response, inner) = egui::containers::menu::MenuButton::new(title).ui(ui, contents);
            (response, inner.is_some())
        })
        .inner;
    ui.data_mut(|d| d.insert_temp(key, open));
    if open != was_open {
        ui.ctx().request_repaint();
    }
    response
}

/// Every folder below `root` (hidden ones excluded), for "Expand all".
/// With a menu of the bar open, pointing at another menu's title opens that
/// one, as in desktop menu bars (egui only opens them on click).
fn switch_menus(ctx: &egui::Context, titles: &[egui::Response]) {
    let ids: Vec<egui::Id> = titles.iter().map(egui::Popup::default_response_id).collect();
    if !ids.iter().any(|&id| egui::Popup::is_id_open(ctx, id)) {
        return;
    }
    if let Some((_, &id)) = titles.iter().zip(&ids).find(|(title, id)| title.hovered() && !egui::Popup::is_id_open(ctx, **id)) {
        egui::Popup::open_id(ctx, id);
        ctx.request_repaint();
    }
}

/// Adds a detail to the preview's info line.
fn add_info(info: &mut String, part: impl std::fmt::Display) {
    if !info.is_empty() {
        info.push_str(" · ");
    }
    info.push_str(&part.to_string());
}

fn subfolders(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
                out.push(path.clone());
                stack.push(path);
            }
        }
    }
    out
}

fn getting_started(ui: &mut egui::Ui) {
    let section = |ui: &mut egui::Ui, title: &str, paragraphs: &[&str]| {
        ui.strong(title);
        for (i, text) in paragraphs.iter().enumerate() {
            if i > 0 {
                ui.add_space(3.0);
            }
            ui.label(*text);
        }
        ui.add_space(8.0);
    };
    section(
        ui,
        "1. Choose the game",
        &["Paleto Modkit finds GTA V in your Steam libraries; the Game menu picks another install. \
           The first time, the encryption keys are read from your game executable."],
    );
    section(
        ui,
        "2. Browse",
        &["The side tree shows the game folder: click a name to open it, the arrow to expand it. \
           Double-click archives (.rpf) to look inside them, nested archives included."],
    );
    section(
        ui,
        "3. Edit",
        &[
            "Inside an archive, add files with the toolbar or by dropping them on the window; extract, rename or delete them, \
             and create folders. The preview panel edits text files, and views, exports or replaces the textures of .ytd files.",
            "Original game archives are never modified: they are copied to the mods folder first. Archives with a modified copy \
             show in orange, and so do the files inside them that differ from the game's.",
            "Outside archives, rename and delete work on the files on disk.",
        ],
    );
    section(
        ui,
        "4. Install mods",
        &[
            "Install .oiv packages with Install package… (top right) or by dropping them on the window; add-on packs (dlc.rpf) \
             the same way, or from the File menu. Game data goes to the mods folder, plugins and scripts to the game folder.",
            "On the Enhanced edition, files made for Legacy are found and can be converted while they are installed.",
            "Prefix tweaks (beside Install) prepares the game's Wine prefix for ScriptHookVDotNet mods: it installs .NET Framework 4.8, \
             after a backup of the prefix. You add ScriptHookVDotNet and the mods yourself.",
        ],
    );
    section(
        ui,
        "5. Play",
        &["The game loads the mods folder through an ASI loader with OpenIV.asi (Legacy) or OpenRPF.asi (Enhanced), which you \
           install separately. Tools > Mods in game checks them, and shows which modded archives the last game session read."],
    );
    section(
        ui,
        "6. When the game breaks",
        &["Game > Snapshots saves the mods folder before a change and puts it back after. Tools > Crash detection reads the game's \
           crash reports, names the likely causes and narrows the mod down by halves."],
    );
}

fn shortcuts(ui: &mut egui::Ui) {
    let rows = [
        ("Type letters", "Filter the current folder"),
        ("⬆ / ⬇", "Move the selection"),
        ("Enter / double-click", "Open folder or archive, preview file"),
        ("Backspace", "Go up one level"),
        ("Alt+Left / Alt+Right, mouse side buttons", "Back / forward"),
        ("Ctrl+click, Shift+click", "Select several items"),
        ("Delete", "Delete the selection"),
        ("F2", "Rename the selected item"),
        ("Ctrl+Shift+N", "New folder"),
        ("F5", "Read the folders again"),
        ("Esc", "Clear the filter, close dialogs"),
        ("Ctrl+S", "Save the file being edited"),
        ("Ctrl+F", "Search in the text being previewed (Enter: next, Shift+Enter: previous, Esc: close)"),
        ("Right click", "Actions for the item"),
    ];
    egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 6.0]).striped(true).show(ui, |ui| {
        for (keys, what) in rows {
            ui.strong(keys);
            ui.label(what);
            ui.end_row();
        }
    });
}

fn about(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add(egui::Image::from_bytes("bytes://paleto-modkit-icon.png", crate::ICON_PNG).fit_to_exact_size(egui::vec2(96.0, 96.0)));
        ui.heading(crate::NAME);
        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.add_space(4.0);
        ui.label("GTA V modding for Linux: archives, mods folder and OIV packages.");
        ui.hyperlink_to("github.com/the-beper/PaletoModkit", REPO_URL);
    });
    ui.separator();
    ui.label("Licence: GNU General Public License v3.0 or later.");
    ui.add_space(4.0);
    ui.strong("Thanks to");
    ui.horizontal_wrapped(|ui| {
        ui.hyperlink_to("CodeWalker", "https://github.com/dexyfex/CodeWalker");
        ui.label("and");
        ui.hyperlink_to("gta-toolkit", "https://github.com/Neodymium146/gta-toolkit");
        ui.label("for the RPF format and keys,");
        ui.hyperlink_to("OpenIV", "https://openiv.com");
        ui.label("for the package format,");
        ui.hyperlink_to("egui", "https://github.com/emilk/egui");
        ui.label("for the interface.");
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Not affiliated with Rockstar Games or the OpenIV team.").weak());
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
fn edit_message(edits: &[TextureEdit], ytd: &str) -> String {
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

/// The filter box of the file list (in the menu bar).
const FILTER_ID: &str = "browser filter";

/// The box that filters the file list by name, at `rect`; typing in the list goes there.
fn filter_box(ui: &mut egui::Ui, b: &mut Browser, rect: egui::Rect) {
    let id = egui::Id::unique(FILTER_ID);
    let field = egui::TextEdit::singleline(&mut b.filter)
        .id(id)
        .hint_text("🔍 Filter this folder…")
        .desired_width(rect.width())
        .min_size(rect.size())
        .align(egui::Align2::LEFT_CENTER);
    let response = ui.put(rect, field);
    if b.blur_filter {
        b.blur_filter = false;
        response.surrender_focus();
    }
    if b.focus_filter {
        b.focus_filter = false;
        response.request_focus();
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            let end = egui::text::CCursor::new(b.filter.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        }
    }
    if response.changed() {
        b.selected.clear();
        b.anchor = None;
    }
}

/// Marks the rows that differ from the original game files.
fn mark_changes(rows: &mut [Row], changes: &Changes) {
    for row in rows {
        let change = changes.get(&row.name.to_ascii_lowercase());
        row.modded = change.is_some();
        row.added = change == Some(&vfs::Change::Added);
    }
}

/// Sorts rows within their group (see [`Grouping`]) by the chosen column.
fn sort_rows(rows: &mut [Row], settings: &Settings) {
    let group = |r: &Row| -> (bool, String) {
        match settings.grouping {
            Grouping::FoldersFirst => (r.kind != RowKind::Folder, String::new()),
            Grouping::ByType => (r.kind != RowKind::Folder, extension(&r.name)),
            Grouping::None => (false, String::new()),
        }
    };
    rows.sort_by(|a, b| {
        let by_name = || a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase());
        let within = match settings.sort_by {
            SortBy::Name => by_name(),
            SortBy::Type => extension(&a.name).cmp(&extension(&b.name)).then_with(by_name),
            SortBy::Size => a.size.cmp(&b.size).then_with(by_name),
        };
        let within = if settings.descending { within.reverse() } else { within };
        group(a).cmp(&group(b)).then(within)
    });
}

fn extract(location: &ArchivePath, keys: Option<&Keys>, names: &[String], dest: &Path) -> Result<usize, String> {
    let (archive, dir) = location.open(keys).map_err(|e| e.to_string())?;
    let mut count = 0;
    for name in names {
        let idx = archive.children(dir).find(|&i| &archive.entries[i].name == name).ok_or(format!("{name} not found"))?;
        let files: Vec<(String, usize)> = if archive.entries[idx].is_dir() {
            archive.walk(idx).into_iter().filter(|(_, i)| !archive.entries[*i].is_dir()).map(|(p, i)| (format!("{name}/{p}"), i)).collect()
        } else {
            vec![(name.clone(), idx)]
        };
        for (rel, i) in files {
            let out = dest.join(&rel);
            std::fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
            let data = archive.read(i, keys).map_err(|e| format!("{rel}: {e}"))?;
            std::fs::write(&out, data).map_err(|e| format!("{}: {e}", out.display()))?;
            count += 1;
        }
    }
    Ok(count)
}

/// The file as editable text, if it looks like text.
enum ExportDest {
    File(PathBuf),
    Folder(PathBuf),
}

fn is_ytd(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ytd")
}

/// Binary files with a preview of their own, read whatever their size.
fn binary_preview(name: &str) -> bool {
    matches!(extension(name).as_str(), "YTD" | "YDR" | "YFT" | "YDD" | "AWC" | "GXT2" | "YMT" | "YTYP" | "YMAP" | "PSO")
}

fn as_text(data: &[u8]) -> Option<String> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    if data.len() > MAX_TEXT || data.iter().take(4096).any(|&b| b == 0) {
        return None;
    }
    String::from_utf8(data.to_vec()).ok()
}

fn load_icon(ctx: &egui::Context, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture("package-icon", color, Default::default()))
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()).unwrap_or_default()
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Sends a desktop notification via `notify-send` (fire-and-forget).
/// Does nothing if `notify-send` is not available.
fn desktop_notify(summary: &str, body: &str) {
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

/// Compression for an imported picture; None is the automatic choice.
fn format_combo(ui: &mut egui::Ui, format: &mut Option<Format>, auto: String) {
    let selected = format.map_or(auto.clone(), |f| f.name().to_string());
    egui::ComboBox::from_label("Format").selected_text(selected).show_ui(ui, |ui| {
        ui.selectable_value(format, None, auto);
        for f in Format::ALL.into_iter().filter(|f| f.can_encode()) {
            ui.selectable_value(format, Some(f), f.name());
        }
    });
}
