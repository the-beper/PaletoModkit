//! Whether the game will load the mods folder, and what it loaded last time.
//!
//! The game itself never reads the mods folder: an ASI plugin does, OpenIV.asi
//! (Legacy) or OpenRPF.asi (Enhanced), loaded into the game by an ASI loader
//! (a proxy DLL such as dinput8.dll). Both plugins log the files they read from
//! the mods folder, which tells whether an edit reached the game.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use paleto_rpf::{Archive, Keys};

use crate::game::{Edition, Game, resolve_ci};
use crate::vfs::ArchivePath;
use crate::Error;

/// DLL names the game loads from its folder, which ASI loaders take.
const PROXY_DLLS: &[&str] = &[
    "dinput8.dll",
    "xinput1_4.dll",
    "xinput1_3.dll",
    "version.dll",
    "winmm.dll",
    "dsound.dll",
    "winhttp.dll",
    "dxgi.dll",
    "d3d11.dll",
    "d3d12.dll",
];

/// How the game folder is set up to load the mods folder.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The plugin that loads the mods folder for this edition.
    pub plugin: &'static str,
    pub plugin_installed: bool,
    /// The ASI loader's file name, if there is one.
    pub asi_loader: Option<String>,
}

impl Setup {
    pub fn ready(&self) -> bool {
        self.plugin_installed && self.asi_loader.is_some()
    }

    /// What is missing, in a sentence; `None` when the setup is complete.
    pub fn problem(&self) -> Option<String> {
        let plugin = self.plugin;
        match (self.asi_loader.is_some(), self.plugin_installed) {
            (true, true) => None,
            (true, false) => Some(format!("{plugin} is not in the game folder: the game will ignore the mods folder")),
            (false, true) => Some(format!("no ASI loader (such as dinput8.dll) in the game folder: {plugin} will not load")),
            (false, false) => Some(format!("no ASI loader or {plugin} in the game folder: the game will ignore the mods folder")),
        }
    }
}

/// What the last game session read from the mods folder, from the plugin's log.
#[derive(Debug, Clone)]
pub struct Session {
    pub log: PathBuf,
    pub time: Option<SystemTime>,
    /// Paths read from the mods folder, relative to it, lowercase with '/'.
    pub loaded: BTreeSet<String>,
    /// Error lines of the log.
    pub errors: Vec<String>,
}

/// A file of the mods folder, and whether the last session read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModdedFile {
    /// Relative to the mods folder, with '/'.
    pub path: String,
    /// `None` when the log cannot tell: the plugins log archives, not loose files.
    pub loaded: Option<bool>,
}

pub fn plugin_name(edition: Edition) -> &'static str {
    match edition {
        Edition::Legacy => "OpenIV.asi",
        Edition::Enhanced => "OpenRPF.asi",
    }
}

pub fn setup(game: &Game) -> Setup {
    let plugin = plugin_name(game.edition);
    let names: Vec<String> = std::fs::read_dir(&game.root)
        .map(|entries| entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let plugin_installed = names.iter().any(|n| n.eq_ignore_ascii_case(plugin));
    let asi_loader = PROXY_DLLS
        .iter()
        .filter_map(|p| names.iter().find(|n| n.eq_ignore_ascii_case(p)))
        .find(|n| std::fs::read(game.root.join(n)).is_ok_and(|data| loads_asi(&data)))
        .cloned();
    Setup { plugin, plugin_installed, asi_loader }
}

/// Script Hook V, which runs the game's script mods (most `.asi` plugins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptHook {
    /// The game versions it supports, from its log's first line ("v3889.0/1158.13").
    pub supports: Option<String>,
    pub status: HookStatus,
    /// The log predates the game's executable: the game was updated since
    /// Script Hook V last ran, so the status says nothing about this version.
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    /// It started, with this game version ("VER_EN_1_0_1158_13").
    Ok(String),
    /// It did not start: the log line saying why.
    Failed(String),
    /// No log yet: the game has not run with it.
    NotRun,
}

/// Script Hook V's state, or `None` when it is not installed.
pub fn script_hook(game: &Game) -> Option<ScriptHook> {
    resolve_ci(&game.root, "ScriptHookV.dll").is_file().then_some(())?;
    let log = resolve_ci(&game.root, "ScriptHookV.log");
    let Ok(text) = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        return Some(ScriptHook { supports: None, status: HookStatus::NotRun, stale: false });
    };
    let supports = text.lines().next().and_then(|l| l.rsplit(", ").next()).map(|v| v.trim_end_matches(')').to_string()).filter(|v| v.starts_with('v'));
    let status = if let Some(version) = text.lines().find_map(|l| l.split("INIT: Success, game version is ").nth(1)) {
        HookStatus::Ok(version.trim().to_string())
    } else {
        let failure = text.lines().find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("fatal") || l.contains("error") || l.contains("unknown game version") || l.contains("unsupported")
        });
        HookStatus::Failed(failure.map_or_else(|| "it did not report a successful start".into(), |l| strip_time(l).to_string()))
    };
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let stale = modified(&log).zip(modified(&game.root.join(game.edition.exe_name()))).is_some_and(|(log, exe)| log < exe);
    Some(ScriptHook { supports, status, stale })
}

/// "[19:49:14] INIT: x" → "INIT: x".
pub(crate) fn strip_time(line: &str) -> &str {
    line.strip_prefix('[').and_then(|l| l.split_once("] ")).map_or(line, |(_, rest)| rest)
}

/// An `.asi` plugin of the game folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsiPlugin {
    pub name: String,
    pub modified: Option<SystemTime>,
    /// Older than the game's executable: made before the last game update,
    /// so it may need an update of its own.
    pub older_than_game: bool,
}

/// The `.asi` plugins of the game folder, by name.
pub fn asi_plugins(game: &Game) -> Vec<AsiPlugin> {
    let exe = std::fs::metadata(game.root.join(game.edition.exe_name())).and_then(|m| m.modified()).ok();
    let mut out: Vec<AsiPlugin> = std::fs::read_dir(&game.root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("asi")))
        .map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok();
            let older_than_game = modified.zip(exe).is_some_and(|(m, x)| m < x);
            AsiPlugin { name: e.file_name().to_string_lossy().into_owned(), modified, older_than_game }
        })
        .collect();
    out.sort_by_key(|p| p.name.to_ascii_lowercase());
    out
}

/// Whether a DLL mentions ".asi" files, as ASI loaders do (and system DLLs do not).
fn loads_asi(dll: &[u8]) -> bool {
    let utf16: Vec<u8> = ".asi".bytes().flat_map(|b| [b, 0]).collect();
    let lower = dll.to_ascii_lowercase();
    lower.windows(4).any(|w| w == b".asi") || lower.windows(8).any(|w| w == utf16)
}

/// The plugin's log of the last game session, where it is found.
pub fn log_path(game: &Game) -> Option<PathBuf> {
    let candidates = match game.edition {
        Edition::Legacy => vec![game.root.join("OpenIV.log")],
        Edition::Enhanced => {
            let relative = Path::new("Los Santos Multiplayer").join("OpenRPF.log");
            let mut list = vec![game.root.join("OpenRPF.log"), game.root.join(&relative)];
            // Under Proton the log is in the user folder of the game's prefix:
            // <library>/steamapps/compatdata/<app id>/pfx/drive_c/users/steamuser.
            if let Some(steamapps) = std::fs::canonicalize(&game.root).ok().and_then(|r| r.parent()?.parent().map(Path::to_path_buf)) {
                let user = steamapps.join("compatdata/3240220/pfx/drive_c/users/steamuser");
                for dir in ["Documents", "AppData/Roaming", "AppData/Local"] {
                    list.push(user.join(dir).join(&relative));
                }
            }
            list
        }
    };
    candidates.into_iter().find(|p| p.is_file())
}

/// Reads the last session's log, if the game has run with the plugin.
pub fn last_session(game: &Game) -> Option<Session> {
    let log = log_path(game)?;
    let text = String::from_utf8_lossy(&std::fs::read(&log).ok()?).into_owned();
    let time = std::fs::metadata(&log).and_then(|m| m.modified()).ok();
    let (loaded, errors) = parse_log(game.edition, &text);
    Some(Session { log, time, loaded, errors })
}

/// The mods-folder paths a log says the game read, and its error lines.
pub fn parse_log(edition: Edition, text: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut loaded = BTreeSet::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        let read = match edition {
            // "[fiDeviceLocal::openBulk] 'mods/update/update.rpf' {M}"
            Edition::Legacy => line.contains("{M}"),
            // "[info] redirect '…' (…)", "[info] successfully loaded modified root RPF '…'"
            Edition::Enhanced => line.contains("redirect '") || line.contains("loaded modified root RPF"),
        };
        if read && let Some(path) = line.split('\'').nth(1) {
            loaded.insert(mods_relative(path));
        }
        let lower = line.to_ascii_lowercase();
        if (lower.contains("[error]") || lower.contains(" error:") || lower.contains("failed")) && errors.len() < 100 {
            errors.push(line.trim().to_string());
        }
    }
    (loaded, errors)
}

/// "mods/update/update.rpf", "C:\…\mods\update\update.rpf" or "update/update.rpf"
/// → "update/update.rpf".
fn mods_relative(path: &str) -> String {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let path = match path.rfind("mods/") {
        Some(i) if i == 0 || path.as_bytes()[i - 1] == b'/' => &path[i + 5..],
        _ => &path,
    };
    path.trim_start_matches('/').to_string()
}

/// The files of the mods folder, with whether the session read them.
pub fn modded_files(game: &Game, session: Option<&Session>) -> Vec<ModdedFile> {
    let mods = game.mods_dir();
    let mut files = Vec::new();
    let mut stack = vec![mods.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(&mods) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                let archive = rel.to_ascii_lowercase().ends_with(".rpf");
                let loaded = session.and_then(|s| {
                    let read = s.loaded.contains(&rel.to_ascii_lowercase());
                    (read || archive).then_some(read)
                });
                files.push(ModdedFile { path: rel, loaded });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// A file of the mods folder that the Enhanced game cannot read: one made for Legacy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignFile {
    /// Relative to the mods folder, with '/' between the archives and folders.
    pub path: String,
    /// What the file is, and what Enhanced uses, in a few words.
    pub problem: String,
}

/// The resource version the Enhanced game reads for a file name, if its type has one.
pub fn enhanced_version(name: &str) -> Option<u32> {
    match name.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "ytd" => Some(5),
        "ydr" | "ydd" => Some(159),
        "yft" => Some(171),
        "ypt" => Some(71),
        _ => None,
    }
}

/// The files of the mods folder's archives (nested ones too) that are not in
/// the Enhanced format: models, textures and particles of another resource
/// version. The game crashes, or ignores the file, when it meets one. Videos
/// are not checked: Enhanced plays Bink 1 too (its own movies use it). Files the original game archive
/// already has in that same format are left out: Rockstar's own leftovers.
/// Nothing is reported for the Legacy edition.
pub fn foreign_files(game: &Game, keys: Option<&Keys>) -> Result<Vec<ForeignFile>, Error> {
    foreign_files_cached(game, keys, &mut ForeignCache::default())
}

/// [`foreign_files`], reading again only the archives that changed since
/// `cache` saw them (by size and time, of the mods copy and its original).
pub fn foreign_files_cached(game: &Game, keys: Option<&Keys>, cache: &mut ForeignCache) -> Result<Vec<ForeignFile>, Error> {
    let mut found = Vec::new();
    if game.edition != Edition::Enhanced {
        return Ok(found);
    }
    let mods = game.mods_dir();
    let mut archives = Vec::new();
    let mut stack = vec![mods.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpf"))
                && let Ok(rel) = path.strip_prefix(&mods)
            {
                archives.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    archives.sort();
    let mut seen = HashMap::new();
    for rel in archives {
        let modded_path = mods.join(&rel);
        let original_path = game.original(&rel);
        let stamp = format!("{}|{}", file_stamp(&modded_path), file_stamp(&original_path));
        let key = modded_path.to_string_lossy().into_owned();
        let files = match cache.entries.get(&key).filter(|(s, _)| *s == stamp) {
            Some((_, files)) => files.clone(),
            None => {
                let modded = Archive::open(&modded_path, keys)?;
                let original = if original_path.is_file() { Some(Archive::open(&original_path, keys)?) } else { None };
                let mut files = Vec::new();
                scan_foreign(&rel, &modded, original.as_ref(), keys, &mut files);
                files
            }
        };
        found.extend(files.iter().cloned());
        seen.insert(key, (stamp, files));
    }
    // Archives gone from the mods folder are forgotten.
    cache.entries.retain(|k, _| !k.starts_with(&mods.to_string_lossy().into_owned()) || seen.contains_key(k));
    cache.entries.extend(seen);
    Ok(found)
}

/// Size and modification time of a file, "-" when there is none.
pub(crate) fn file_stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let time = m.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            format!("{}:{time}", m.len())
        }
        Err(_) => "-".into(),
    }
}

/// What [`foreign_files_cached`] found in each archive, by its path on disk.
#[derive(Debug, Default)]
pub struct ForeignCache {
    entries: HashMap<String, (String, Vec<ForeignFile>)>,
}

impl ForeignCache {
    /// Where the GUI and the CLI keep it.
    pub fn default_path() -> PathBuf {
        crate::game::data_dir().join("legacy-scan.tsv")
    }

    /// Reads a cache saved by [`ForeignCache::save`]; empty if there is none.
    /// Lines: "archive\tstamp", then "\tpath\tproblem" for each file found in it.
    pub fn load(path: &Path) -> Self {
        let mut entries: HashMap<String, (String, Vec<ForeignFile>)> = HashMap::new();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut current: Option<String> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix('\t') {
                if let (Some(key), Some((path, problem))) = (&current, rest.split_once('\t')) {
                    entries.get_mut(key).unwrap().1.push(ForeignFile { path: path.into(), problem: problem.into() });
                }
            } else if let Some((key, stamp)) = line.split_once('\t') {
                entries.insert(key.into(), (stamp.into(), Vec::new()));
                current = Some(key.into());
            }
        }
        ForeignCache { entries }
    }

    /// Saves it (atomically: a new file renamed over the old one).
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        let mut text = String::new();
        for key in keys {
            let (stamp, files) = &self.entries[key];
            text += &format!("{key}\t{stamp}\n");
            for f in files {
                text += &format!("\t{}\t{}\n", f.path, f.problem);
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        static SAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// [`foreign_files_cached`] with the cache kept in `cache_path`.
pub fn foreign_files_cached_at(game: &Game, keys: Option<&Keys>, cache_path: &Path) -> Result<Vec<ForeignFile>, Error> {
    let mut cache = ForeignCache::load(cache_path);
    let found = foreign_files_cached(game, keys, &mut cache)?;
    // Archives of other game folders that are gone.
    cache.entries.retain(|k, _| Path::new(k).is_file());
    // A cache that cannot be written only costs time.
    let _ = cache.save(cache_path);
    Ok(found)
}

fn scan_foreign(prefix: &str, modded: &Archive, original: Option<&Archive>, keys: Option<&Keys>, found: &mut Vec<ForeignFile>) {
    let twins: HashMap<String, usize> =
        original.map(|o| o.walk(0).into_iter().map(|(path, i)| (path.to_ascii_lowercase(), i)).collect()).unwrap_or_default();
    for (path, i) in modded.walk(0) {
        let entry = &modded.entries[i];
        if entry.is_dir() {
            continue;
        }
        let twin = twins.get(&path.to_ascii_lowercase()).zip(original).map(|(&j, o)| (o, j));
        let full = format!("{prefix}/{path}");
        if entry.is_archive() {
            // Nested archives that cannot be opened are not this check's business.
            if let Ok(child) = modded.open_child(i, keys) {
                let twin_child = twin.filter(|(o, j)| o.entries[*j].is_archive()).and_then(|(o, j)| o.open_child(j, keys).ok());
                scan_foreign(&full, &child, twin_child.as_ref(), keys, found);
            }
        } else if let Some(wanted) = enhanced_version(&entry.name) {
            let Some(version) = entry.resource_version().filter(|&v| v != wanted) else { continue };
            if twin.and_then(|(o, j)| o.entries[j].resource_version()) != Some(version) {
                found.push(ForeignFile { path: full, problem: format!("resource version {version} (Enhanced uses {wanted})") });
            }
        }
    }
}

/// DLC packs that the game's dlclist.xml lists and the mods folder's copy of
/// update.rpf does not. A copy made before a game update keeps the old list,
/// so the game skips the packs added since.
pub fn missing_dlc_packs(game: &Game, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
    let (rel, inner) = DLC_LIST;
    let modded = resolve_ci(&game.mods_dir(), rel);
    if !modded.is_file() {
        return Ok(Vec::new());
    }
    let list = |disk: PathBuf| -> Result<BTreeSet<String>, Error> {
        let xml = inner.split('/').fold(ArchivePath::new(disk), |p, part| p.join(part)).read(keys)?;
        Ok(dlc_packs(&String::from_utf8_lossy(&xml)))
    };
    let modded = list(modded)?;
    Ok(list(game.original(rel))?.into_iter().filter(|p| !modded.contains(p)).collect())
}

/// Add-on packs of the mods folder (`update/x64/dlcpacks/<name>/dlc.rpf`, with
/// no such pack in the game folder) that the DLC list the game reads does not
/// name, so the game never loads them. The list read is the mods copy's, or
/// the game's when there is no copy. Names in lower case, sorted.
pub fn unlisted_dlc_packs(game: &Game, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
    let dir = resolve_ci(&game.mods_dir(), "update/x64/dlcpacks");
    let mut addons = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let has_pack = std::fs::read_dir(entry.path())
            .into_iter()
            .flatten()
            .flatten()
            .any(|f| f.file_name().to_string_lossy().eq_ignore_ascii_case("dlc.rpf"));
        let original = resolve_ci(&game.root, &format!("update/x64/dlcpacks/{name}"));
        if has_pack && !original.is_dir() {
            addons.push(name.to_ascii_lowercase());
        }
    }
    if addons.is_empty() {
        return Ok(addons);
    }
    let (rel, inner) = DLC_LIST;
    let modded = resolve_ci(&game.mods_dir(), rel);
    let disk = if modded.is_file() { modded } else { game.original(rel) };
    let xml = inner.split('/').fold(ArchivePath::new(disk), |p, part| p.join(part)).read(keys)?;
    let listed = dlc_packs(&String::from_utf8_lossy(&xml));
    addons.retain(|p| !listed.contains(p));
    addons.sort();
    Ok(addons)
}

/// Where the DLC list is, inside update.rpf.
pub const DLC_LIST: (&str, &str) = ("update/update.rpf", "common/data/dlclist.xml");

/// Adds packs at the end of a dlclist.xml's paths, indented like its last item.
pub fn add_dlc_packs(xml: &str, packs: &[String]) -> Result<String, Error> {
    let end = xml.rfind("</Paths>").ok_or_else(|| Error::Xml("dlclist.xml has no </Paths>".into()))?;
    let (before, after) = xml.split_at(end);
    let newline = if xml.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = |at: usize| xml[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = before
        .rfind("<Item>")
        .map(|i| &xml[line_start(i)..i])
        .filter(|s| s.trim().is_empty())
        .unwrap_or("\t\t");
    // </Paths> keeps its own line and indentation.
    let (head, closing_indent) = match &before[line_start(end)..] {
        blank if blank.trim().is_empty() => (&before[..line_start(end)], blank),
        _ => (before, ""),
    };
    let mut out = head.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out += newline;
    }
    for p in packs {
        out += &format!("{indent}<Item>dlcpacks:/{p}/</Item>{newline}");
    }
    out += closing_indent;
    out += after;
    Ok(out)
}

/// The pack names of a dlclist.xml: "dlcpacks:/mp2026_01/" (or the older
/// "platform:\\dlcPacks\\mpbeach\\") → "mp2026_01".
/// Packs commented out do not count.
fn dlc_packs(xml: &str) -> BTreeSet<String> {
    dlc_pack_order(xml).into_iter().collect()
}

/// The pack names of a dlclist.xml in its order, which is the game's loading order.
pub(crate) fn dlc_pack_order(xml: &str) -> Vec<String> {
    let mut lower = xml.to_ascii_lowercase();
    while let Some(start) = lower.find("<!--") {
        let end = lower[start..].find("-->").map_or(lower.len(), |e| start + e + 3);
        lower.replace_range(start..end, "");
    }
    lower = lower.replace('\\', "/");
    lower
        .split("dlcpacks")
        .skip(1)
        .filter_map(|rest| rest.trim_start_matches([':', '/']).split(['/', '<']).next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// What a [`ModEntry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModKind {
    /// An `.asi` plugin of the game folder.
    Plugin,
    /// A Script Hook V .NET script (`scripts/*.dll`, `.cs`, `.vb`).
    Script,
    /// An add-on pack of the mods folder (`update/x64/dlcpacks/<pack>`).
    AddonPack,
    /// Any other archive of the mods folder.
    Archive,
}

/// What is known about a mod: the good (`Read`, `TurnedOff`) and the bad news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModStatus {
    /// The last session read it.
    Read,
    /// The last session did not read it.
    NotRead,
    /// An add-on pack the DLC list does not name: the game never loads it.
    NotInDlcList,
    /// A plugin older than the game's executable.
    OlderThanGame,
    /// Moved to `mods_disabled/` (see [`crate::crash_hunt::set_enabled`]).
    TurnedOff,
    /// It holds a file the Enhanced game cannot read: the problem, as [`ForeignFile`] says it.
    NotEnhanced(String),
}

impl ModStatus {
    fn is_problem(&self) -> bool {
        !matches!(self, ModStatus::Read | ModStatus::TurnedOff)
    }
}

/// A mod of the setup, as the "Mods in game" list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModEntry {
    pub kind: ModKind,
    /// What to show: the file or pack name, the archive's path in the mods folder.
    pub name: String,
    /// The part path for [`crate::crash_hunt::set_enabled`] ("Trainer.asi",
    /// "scripts/Mod.dll", "mods/update/x64/dlcpacks/car", "mods/x64e.rpf").
    pub part: String,
    pub enabled: bool,
    /// Empty for a mod with nothing to say. A turned off mod has only `TurnedOff`.
    pub status: Vec<ModStatus>,
}

/// The mods of the setup: plugins, scripts, add-on packs and archives of the
/// mods folder (turned off ones too), with what is known about each. Loose
/// files are not entries. Entries with a problem come first, then by kind and name.
///
/// `session` is the last session (archives and packs are `Read` or `NotRead`
/// only when there is one); `unlisted_dlc` is [`unlisted_dlc_packs`] and
/// `foreign` is [`foreign_files`], which are slow, so the caller computes them.
pub fn inventory(game: &Game, session: Option<&Session>, unlisted_dlc: &[String], foreign: &[ForeignFile]) -> Vec<ModEntry> {
    const PACKS: &str = "mods/update/x64/dlcpacks/";
    let off = crate::crash_hunt::turned_off(game);
    let mut parts = crate::crash_hunt::parts(game);
    // Parts turned off are not in the folders any more; files turned off inside an archive are not parts.
    for p in &off {
        let lower = p.to_ascii_lowercase();
        let known = if let Some(pack) = lower.strip_prefix(PACKS) {
            !pack.contains('/')
        } else {
            !lower.contains('/') && lower.ends_with(".asi") || lower.starts_with("scripts/") || lower.starts_with("mods/") && lower.ends_with(".rpf")
        };
        if known && !parts.contains(p) {
            parts.push(p.clone());
        }
    }
    let older: HashMap<String, bool> = asi_plugins(game).into_iter().map(|p| (p.name, p.older_than_game)).collect();
    let files: HashMap<String, Option<bool>> = modded_files(game, session).into_iter().map(|f| (f.path.to_ascii_lowercase(), f.loaded)).collect();
    let mut out: Vec<ModEntry> = parts
        .into_iter()
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            let enabled = !off.contains(&part);
            // Where the mod is, in the mods folder's terms (what the log and the foreign files use).
            let (kind, name, inside) = if let Some(pack) = lower.strip_prefix(PACKS) {
                (ModKind::AddonPack, part[PACKS.len()..].to_string(), Some(format!("update/x64/dlcpacks/{pack}")))
            } else if lower.starts_with("scripts/") {
                (ModKind::Script, part[8..].to_string(), None)
            } else if let Some(rel) = lower.strip_prefix("mods/") {
                (ModKind::Archive, part[5..].to_string(), Some(rel.to_string()))
            } else {
                (ModKind::Plugin, part.clone(), None)
            };
            let mut status = Vec::new();
            if !enabled {
                status.push(ModStatus::TurnedOff);
            } else {
                if kind == ModKind::Plugin && older.get(&part) == Some(&true) {
                    status.push(ModStatus::OlderThanGame);
                }
                let archive = match (&kind, &inside) {
                    (ModKind::AddonPack, Some(dir)) => format!("{dir}/dlc.rpf"),
                    (_, Some(path)) => path.clone(),
                    _ => String::new(),
                };
                if let Some(read) = files.get(&archive).copied().flatten() {
                    status.push(if read { ModStatus::Read } else { ModStatus::NotRead });
                }
                if kind == ModKind::AddonPack && unlisted_dlc.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
                    status.push(ModStatus::NotInDlcList);
                }
                if let Some(dir) = &inside {
                    let prefix = format!("{dir}/");
                    for f in foreign.iter().filter(|f| {
                        let path = f.path.to_ascii_lowercase();
                        path == *dir || path.starts_with(&prefix)
                    }) {
                        let problem = ModStatus::NotEnhanced(f.problem.clone());
                        if !status.contains(&problem) {
                            status.push(problem);
                        }
                    }
                }
            }
            ModEntry { kind, name, part, enabled, status }
        })
        .collect();
    out.sort_by_cached_key(|e| (!e.status.iter().any(ModStatus::is_problem), e.kind, e.name.to_ascii_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openiv_log() {
        let log = "08/10/2026 18:37:29.912 [1976] TRACE: Init started (v. 1.2.0.1)\n\
            08/10/2026 18:37:30.903 [1976] TRACE: [fiDeviceLocal::openBulk] 'x64a.rpf', RPF = true\n\
            08/10/2026 18:37:30.902 [1976] TRACE: [fiDeviceLocal::openBulk] 'mods/update/update.rpf' {M}\n\
            08/10/2026 18:37:30.902 [1976] TRACE: [fiDeviceLocal::getFileAttr] 'mods/common.rpf' {M}\n";
        let (loaded, errors) = parse_log(Edition::Legacy, log);
        assert_eq!(loaded.into_iter().collect::<Vec<_>>(), ["common.rpf", "update/update.rpf"]);
        assert!(errors.is_empty());
    }

    #[test]
    fn openrpf_log() {
        let log = "[info] successfully loaded modified root RPF 'mods/x64e.rpf' (at 0x1234)\n\
            [info] redirect 'update/update.rpf' (C:\\Games\\GTAV\\mods\\update\\update.rpf)\n\
            [info] redirect 'C:\\Games\\GTAV\\mods\\Update\\x64\\data\\file.txt' (1)\n\
            [error] rage failed to open the file 'mods/bad.rpf'\n";
        let (loaded, errors) = parse_log(Edition::Enhanced, log);
        assert_eq!(loaded.into_iter().collect::<Vec<_>>(), ["update/update.rpf", "update/x64/data/file.txt", "x64e.rpf"]);
        assert_eq!(errors, ["[error] rage failed to open the file 'mods/bad.rpf'"]);
    }

    #[test]
    fn dlc_list_additions() {
        let xml = "<SMandatoryPacksData>\r\n\t<Paths>\r\n\t\t<Item>dlcpacks:/mpheist/</Item>\r\n\t</Paths>\r\n</SMandatoryPacksData>\r\n";
        let added = add_dlc_packs(xml, &["mp2026_01".into(), "patch2026_01".into()]).unwrap();
        assert_eq!(added, "<SMandatoryPacksData>\r\n\t<Paths>\r\n\t\t<Item>dlcpacks:/mpheist/</Item>\r\n\t\t<Item>dlcpacks:/mp2026_01/</Item>\r\n\t\t<Item>dlcpacks:/patch2026_01/</Item>\r\n\t</Paths>\r\n</SMandatoryPacksData>\r\n");
        assert_eq!(dlc_packs(&added).len(), 3);
        let inline = add_dlc_packs("<Paths><Item>dlcpacks:/a/</Item></Paths>", &["b".into()]).unwrap();
        assert_eq!(inline, "<Paths><Item>dlcpacks:/a/</Item>\n\t\t<Item>dlcpacks:/b/</Item>\n</Paths>");
    }

    #[test]
    fn dlc_list() {
        let xml = "<Paths>\n<Item>dlcpacks:/mpHeist/</Item>\n<Item>dlcpacks:/mp2026_01/</Item>\n<!--<Item>dlcpacks:/old/</Item>-->\n<Item>platform:\\dlcPacks\\mpBeach\\</Item>\n</Paths>";
        assert_eq!(dlc_packs(xml).into_iter().collect::<Vec<_>>(), ["mp2026_01", "mpbeach", "mpheist"]);
    }

    #[test]
    fn game_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("GTA5_Enhanced.exe"), b"").unwrap();
        let game = Game::open(dir.path()).unwrap();
        let s = setup(&game);
        assert!(!s.ready() && s.problem().unwrap().contains("no ASI loader or OpenRPF.asi"));

        std::fs::write(dir.path().join("OpenRPF.asi"), b"").unwrap();
        std::fs::write(dir.path().join("dxgi.dll"), b"MZ system dll").unwrap();
        std::fs::write(dir.path().join("DINPUT8.dll"), b"MZ ... %s\\*.ASI ...").unwrap();
        let s = setup(&game);
        assert_eq!(s.asi_loader.as_deref(), Some("DINPUT8.dll"));
        assert!(s.ready() && s.problem().is_none());

        std::fs::create_dir_all(dir.path().join("mods/update/x64")).unwrap();
        std::fs::write(dir.path().join("mods/update/update.rpf"), b"").unwrap();
        std::fs::write(dir.path().join("mods/x64e.rpf"), b"").unwrap();
        std::fs::write(dir.path().join("mods/update/x64/loose.txt"), b"").unwrap();
        std::fs::write(dir.path().join("OpenRPF.log"), "[info] redirect 'mods/update/update.rpf' (x)\n").unwrap();
        let session = last_session(&game).unwrap();
        let files = modded_files(&game, Some(&session));
        let status: Vec<(&str, Option<bool>)> = files.iter().map(|f| (f.path.as_str(), f.loaded)).collect();
        assert_eq!(status, [("update/update.rpf", Some(true)), ("update/x64/loose.txt", None), ("x64e.rpf", Some(false))]);
    }

    /// A resource file of this version (the payload is never read).
    fn resource(version: u32) -> Vec<u8> {
        let mut data = b"RSC7".to_vec();
        for word in [version, (version >> 4) << 28, (version & 15) << 28] {
            data.extend(word.to_le_bytes());
        }
        data.extend([0u8; 32]);
        data
    }

    #[test]
    fn foreign_formats() {
        use paleto_rpf::ArchiveEditor;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        // The game's x64e.rpf: a Legacy leftover of Rockstar's, loose and nested.
        // Bink 1 videos are fine: Enhanced plays them.
        let build = |extra: bool| {
            let mut ed = ArchiveEditor::new("x64e.rpf");
            ed.add_file("props/old.ydr", resource(165)).unwrap();
            ed.add_file("props/fine.ydr", resource(159)).unwrap();
            ed.add_file("movies/logo.bik", b"KB2g....original....".to_vec()).unwrap();
            let nested = ed.archive("levels/icons.rpf", true, None).unwrap();
            nested.add_file("arrow.ydr", resource(154)).unwrap();
            if extra {
                ed.add_file("props/new.ydr", resource(165)).unwrap();
                ed.add_file("props/Fine2.YDD", resource(159)).unwrap();
                ed.add_file("props/tex.ytd", resource(13)).unwrap();
                ed.add_file("props/notes.txt", b"BIKi".to_vec()).unwrap();
                ed.add_file("movies/logo.bik", b"BIKi".to_vec()).unwrap();
                ed.archive("levels/icons.rpf", false, None).unwrap().add_file("mod.ypt", resource(68)).unwrap();
            }
            ed
        };
        build(false).save(&root.join("x64e.rpf")).unwrap();
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/mine")).unwrap();
        build(true).save(&root.join("mods/x64e.rpf")).unwrap();
        // A mod's own pack has no original to compare with.
        let mut pack = ArchiveEditor::new("dlc.rpf");
        pack.add_file("x64/levels/gta5/vehicles/car.yft", resource(162)).unwrap();
        pack.add_file("x64/levels/gta5/vehicles/car.ytd", resource(5)).unwrap();
        pack.save(&root.join("mods/update/x64/dlcpacks/mine/dlc.rpf")).unwrap();

        let game = Game::open(root).unwrap();
        let found = foreign_files(&game, None).unwrap();
        let list: Vec<(&str, &str)> = found.iter().map(|f| (f.path.as_str(), f.problem.as_str())).collect();
        assert_eq!(
            list,
            [
                ("update/x64/dlcpacks/mine/dlc.rpf/x64/levels/gta5/vehicles/car.yft", "resource version 162 (Enhanced uses 171)"),
                ("x64e.rpf/levels/icons.rpf/mod.ypt", "resource version 68 (Enhanced uses 71)"),
                ("x64e.rpf/props/new.ydr", "resource version 165 (Enhanced uses 159)"),
                ("x64e.rpf/props/tex.ytd", "resource version 13 (Enhanced uses 5)"),
            ]
        );

        // Nothing to say about the Legacy edition.
        std::fs::remove_file(root.join("GTA5_Enhanced.exe")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        assert!(foreign_files(&Game::open(root).unwrap(), None).unwrap().is_empty());
    }

    #[test]
    fn foreign_scan_cache() {
        use paleto_rpf::ArchiveEditor;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let pack_path = root.join("mods/update/x64/dlcpacks/car/dlc.rpf");
        let mut pack = ArchiveEditor::new("dlc.rpf");
        pack.add_file("car.yft", resource(162)).unwrap();
        pack.save(&pack_path).unwrap();
        let game = Game::open(&root).unwrap();
        let cache_path = dir.path().join("cache.tsv");

        let first = foreign_files_cached_at(&game, None, &cache_path).unwrap();
        assert_eq!(first.len(), 1);
        // An unchanged archive is not read again: what the cache says is what comes back.
        let text = std::fs::read_to_string(&cache_path).unwrap();
        std::fs::write(&cache_path, text.replace("car.yft", "cached.yft")).unwrap();
        let second = foreign_files_cached_at(&game, None, &cache_path).unwrap();
        assert_eq!(second[0].path, "update/x64/dlcpacks/car/dlc.rpf/cached.yft");
        assert_eq!(second[0].problem, first[0].problem);

        // A changed archive is read again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut pack = ArchiveEditor::open(&pack_path, None).unwrap();
        pack.add_file("car.yft", resource(171)).unwrap();
        pack.save(&pack_path).unwrap();
        assert!(foreign_files_cached_at(&game, None, &cache_path).unwrap().is_empty());

        // A removed archive leaves the cache.
        std::fs::remove_file(&pack_path).unwrap();
        assert!(foreign_files_cached_at(&game, None, &cache_path).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&cache_path).unwrap(), "");
    }

    #[test]
    fn unlisted_addon_packs() {
        use paleto_rpf::ArchiveEditor;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let update = |path: &Path, packs: &[&str]| {
            let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
            let mut ed = ArchiveEditor::new("update.rpf");
            ed.add_file("common/data/dlclist.xml", format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n").into_bytes()).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            ed.save(path).unwrap();
        };
        update(&root.join("update/update.rpf"), &["mpbeach"]);
        // A game pack copied to mods, a listed add-on, two unlisted ones and a folder without a pack.
        for (pack, file) in [("mpBeach", "dlc.rpf"), ("listed", "dlc.rpf"), ("Piccolo", "DLC.rpf"), ("bus", "dlc.rpf"), ("empty", "readme.txt")] {
            std::fs::create_dir_all(root.join(format!("mods/update/x64/dlcpacks/{pack}"))).unwrap();
            std::fs::write(root.join(format!("mods/update/x64/dlcpacks/{pack}/{file}")), b"").unwrap();
        }
        std::fs::create_dir_all(root.join("update/x64/dlcpacks/mpbeach")).unwrap();
        let game = Game::open(root).unwrap();
        // Without a mods copy of update.rpf, the game's own list counts.
        assert_eq!(unlisted_dlc_packs(&game, None).unwrap(), ["bus", "listed", "piccolo"]);
        update(&root.join("mods/update/update.rpf"), &["mpbeach", "listed"]);
        assert_eq!(unlisted_dlc_packs(&game, None).unwrap(), ["bus", "piccolo"]);
    }

    #[test]
    fn script_hook_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let game = Game::open(root).unwrap();
        assert_eq!(script_hook(&game), None);
        std::fs::write(root.join("ScriptHookV.dll"), b"").unwrap();
        assert_eq!(script_hook(&game).unwrap().status, HookStatus::NotRun);

        let header = "// GTA V SCRIPT HOOK (build Jul 15 2026, v3889.0/1158.13)\n//          (C) Alexander Blade 2015-2026\n";
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("ScriptHookV.log"), format!("{header}[19:49:14] INIT: Started\n[19:49:14] INIT: Success, game version is VER_EN_1_0_1158_13\n")).unwrap();
        let hook = script_hook(&game).unwrap();
        assert_eq!(hook.supports.as_deref(), Some("v3889.0/1158.13"));
        assert_eq!(hook.status, HookStatus::Ok("VER_EN_1_0_1158_13".into()));
        assert!(!hook.stale);

        std::fs::write(root.join("ScriptHookV.log"), format!("{header}[19:49:14] INIT: Started\n[19:49:15] FATAL: Unknown game version, check http://dev-c.com for updates\n")).unwrap();
        assert_eq!(script_hook(&game).unwrap().status, HookStatus::Failed("FATAL: Unknown game version, check http://dev-c.com for updates".into()));

        // The game updated after Script Hook V last ran; the plugins made before are flagged too.
        std::fs::write(root.join("Old.asi"), b"").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"new").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("new.ASI"), b"").unwrap();
        assert!(script_hook(&game).unwrap().stale);
        let plugins: Vec<(String, bool)> = asi_plugins(&game).into_iter().map(|p| (p.name, p.older_than_game)).collect();
        assert_eq!(plugins, [("new.ASI".to_string(), false), ("Old.asi".to_string(), true)]);
    }

    #[test]
    fn mods_in_game() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        for f in [
            "GTA5_Enhanced.exe",
            "OpenRPF.asi",
            "Old.asi",
            "scripts/Mod.dll",
            "mods/x64e.rpf",
            "mods/update/update.rpf",
            "mods/update/x64/dlcpacks/car/dlc.rpf",
            "mods/update/x64/dlcpacks/Bus/dlc.rpf",
            "mods/update/x64/dlcpacks/Old/dlc.rpf",
            "mods/update/x64/data/loose.txt",
        ] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), b"").unwrap();
        }
        // Old.asi predates the game's executable; the others are made after it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"new").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("New.asi"), b"").unwrap();
        std::fs::write(root.join("OpenRPF.log"), "[info] redirect 'update/update.rpf' (x)\n[info] redirect 'update/x64/dlcpacks/car/dlc.rpf' (x)\n").unwrap();
        let game = Game::open(&root).unwrap();
        let session = last_session(&game);
        let foreign = [ForeignFile { path: "x64e.rpf/props/new.ydr".into(), problem: "resource version 165 (Enhanced uses 159)".into() }];
        let unlisted = ["old".to_string()];

        let list = |entries: &[ModEntry]| entries.iter().map(|e| format!("{:?} {} {}", e.kind, e.name, e.enabled)).collect::<Vec<_>>();
        let entries = inventory(&game, session.as_ref(), &unlisted, &foreign);
        assert_eq!(
            list(&entries),
            [
                "Plugin Old.asi true",
                "AddonPack Bus true",
                "AddonPack Old true",
                "Archive x64e.rpf true",
                "Plugin New.asi true",
                "Script Mod.dll true",
                "AddonPack car true",
                "Archive update/update.rpf true"
            ]
        );
        let status = |name: &str| entries.iter().find(|e| e.name == name).unwrap().status.clone();
        assert_eq!(status("Old.asi"), [ModStatus::OlderThanGame]);
        assert_eq!(status("New.asi"), []);
        assert_eq!(status("Mod.dll"), []);
        assert_eq!(status("car"), [ModStatus::Read]);
        assert_eq!(status("Bus"), [ModStatus::NotRead]);
        assert_eq!(status("Old"), [ModStatus::NotRead, ModStatus::NotInDlcList]);
        assert_eq!(status("update/update.rpf"), [ModStatus::Read]);
        assert_eq!(status("x64e.rpf"), [ModStatus::NotRead, ModStatus::NotEnhanced("resource version 165 (Enhanced uses 159)".into())]);
        assert_eq!(entries.iter().find(|e| e.name == "car").unwrap().part, "mods/update/x64/dlcpacks/car");
        assert_eq!(entries.iter().find(|e| e.name == "Mod.dll").unwrap().part, "scripts/Mod.dll");

        // Without a session nothing is read or not read; turned off mods stay in the list.
        let plain = inventory(&game, None, &[], &[]);
        assert!(plain.iter().all(|e| e.status.iter().all(|s| *s != ModStatus::Read && *s != ModStatus::NotRead)));
        for part in ["Old.asi", "scripts/Mod.dll", "mods/update/x64/dlcpacks/car", "mods/x64e.rpf"] {
            crate::crash_hunt::set_enabled(&game, part, false).unwrap();
        }
        let entries = inventory(&game, session.as_ref(), &unlisted, &foreign);
        assert_eq!(entries.len(), 8);
        for part in ["Old.asi", "scripts/Mod.dll", "mods/update/x64/dlcpacks/car", "mods/x64e.rpf"] {
            let e = entries.iter().find(|e| e.part == part).unwrap();
            assert!(!e.enabled && e.status == [ModStatus::TurnedOff], "{e:?}");
        }
        assert!(entries.iter().find(|e| e.part == "New.asi").unwrap().enabled);
    }
}
