//! The game's Wine prefix (Proton): finding it, backing it up, and preparing
//! it for .NET mods.
//!
//! ScriptHookVDotNet needs the real .NET Framework 4.8 in the prefix, not the
//! stubs Wine puts there. Installing it through winetricks fails in a Proton
//! prefix for a reason that is not in any message: Proton fills the prefix with
//! symbolic links to its own files, and the .NET installer cannot overwrite
//! them (access denied). [`install_dotnet`] turns into real files the links that
//! stand in the way, runs winetricks, and puts back the Windows version
//! winetricks changes (the game stops with "Support for Windows XP has ended"
//! otherwise).
//!
//! Everything runs headless (no window on the desktop), after a backup of the
//! prefix made like the mods snapshots: a reflink copy where the filesystem
//! allows it.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::game::{data_dir, resolve_ci};
use crate::{Edition, Error, Game};

/// winetricks, pinned: the commit and the script's checksum.
const WINETRICKS_URL: &str = "https://raw.githubusercontent.com/Winetricks/winetricks/f3890f670867b5ffbc3938726db45c0f7d16c8ba/src/winetricks";

/// A file the installation downloads, with the checksum winetricks itself
/// expects (it finds the file in its cache and does not download it again).
struct Download {
    name: &'static str,
    /// The winetricks cache folder it goes in.
    verb: &'static str,
    url: &'static str,
    sha256: &'static str,
    size: u64,
}

const DOWNLOADS: [Download; 3] = [
    Download {
        name: "winetricks",
        verb: "",
        url: WINETRICKS_URL,
        sha256: "672a1ff4442e8691a3ffc0e6860137e201a1d1227e9b3044245f1731e9e9837e",
        size: 873_846,
    },
    Download {
        name: "dotNetFx40_Full_x86_x64.exe",
        verb: "dotnet40",
        url: "https://download.microsoft.com/download/9/5/A/95A9616B-7A37-4AF6-BC36-D6EA96C8DAAE/dotNetFx40_Full_x86_x64.exe",
        sha256: "65e064258f2e418816b304f646ff9e87af101e4c9552ab064bb74d281c38659f",
        size: 50_449_456,
    },
    Download {
        name: "ndp48-x86-x64-allos-enu.exe",
        verb: "dotnet48",
        url: "https://download.visualstudio.microsoft.com/download/pr/7afca223-55d2-470a-8edc-6a1739ae3252/abd170b4b0ec15ad0222a809b761a036/ndp48-x86-x64-allos-enu.exe",
        sha256: "95889d6de3f2070c07790ad6cf2000d33d9a1bdfc6a381725ab82ab1c314fd53",
        size: 72_721_568,
    },
];

/// The `Release` value of .NET Framework 4.8 in the registry.
const NET48_RELEASE: u32 = 528_040;
/// Wine's stub `mscorlib.dll` is 0.7 MB; the real one is 5.4 MB.
const REAL_MSCORLIB: u64 = 2_000_000;
/// The first build of Windows 10; the game refuses older ones.
const WINDOWS_10_BUILD: u32 = 10_240;
/// How many times a failed installation is retried after unlinking what blocked it.
const MAX_ATTEMPTS: usize = 40;

#[derive(Debug, Clone)]
pub struct Proton {
    /// Its folder's name, e.g. "Proton-CachyOS Latest-x86_64_v3".
    pub name: String,
    pub dir: PathBuf,
    pub wine: PathBuf,
    pub wineserver: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Prefix {
    pub app_id: &'static str,
    /// `steamapps/compatdata/<app id>`.
    pub compat: PathBuf,
    /// The prefix itself: `<compat>/pfx`.
    pub path: PathBuf,
    /// The Proton that last used it.
    pub proton: Proton,
}

/// Finds the Proton prefix of a game installed through Steam, and the Proton
/// that last used it.
pub fn find(game: &Game) -> Result<Prefix, Error> {
    let root = std::fs::canonicalize(&game.root)?;
    let steamapps = root
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "steamapps"))
        .ok_or_else(|| {
            Error::Prefix("the game is not in a Steam library, so it has no Proton prefix".into())
        })?;
    let app_id = match game.edition {
        Edition::Legacy => "271590",
        Edition::Enhanced => "3240220",
    };
    let compat = steamapps.join("compatdata").join(app_id);
    let path = compat.join("pfx");
    if !path.join("drive_c").is_dir() {
        return Err(Error::Prefix(
            "Steam has not made the game's prefix yet: start the game once".into(),
        ));
    }
    let proton = proton_of(&compat)?;
    Ok(Prefix {
        app_id,
        compat,
        path,
        proton,
    })
}

/// The Proton that last used a prefix: Steam lists its folders in `config_info`.
fn proton_of(compat: &Path) -> Result<Proton, Error> {
    let text = std::fs::read_to_string(compat.join("config_info")).map_err(|_| {
        Error::Prefix("Steam has not set the prefix up yet: start the game once".into())
    })?;
    for line in text.lines().skip(1) {
        for sub in ["files", "dist"] {
            let marker = format!("/{sub}/");
            let Some(i) = line.find(&marker) else {
                continue;
            };
            let dir = PathBuf::from(&line[..i]);
            let wine = dir.join(sub).join("bin/wine");
            if wine.is_file() {
                return Ok(Proton {
                    name: dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    wineserver: dir.join(sub).join("bin/wineserver"),
                    wine,
                    dir,
                });
            }
        }
    }
    Err(Error::Prefix(
        "cannot find the Proton the game last ran with: start the game once".into(),
    ))
}

/// What the registry and the files of a prefix say about .NET.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DotnetState {
    /// The `Release` of .NET Framework 4 (Wine's stub claims one too).
    pub release: Option<u32>,
    /// Size of the `mscorlib.dll` of .NET 4.
    pub mscorlib: u64,
    /// The Windows build the prefix is set to (Windows 11 is 22000).
    pub windows_build: Option<u32>,
}

impl DotnetState {
    /// The real .NET Framework 4.8 is there, not Wine's stub.
    pub fn installed(&self) -> bool {
        self.release.is_some_and(|r| r >= NET48_RELEASE) && self.mscorlib > REAL_MSCORLIB
    }

    /// The prefix is set to a Windows the game accepts (10 or later).
    pub fn windows_ok(&self) -> bool {
        self.windows_build.is_none_or(|b| b >= WINDOWS_10_BUILD)
    }

    /// The Windows version as winecfg names it, for the build the prefix is set to.
    fn winecfg_name(&self) -> &'static str {
        match self.windows_build {
            Some(b) if b >= 22_000 => "win11",
            Some(b) if b >= WINDOWS_10_BUILD => "win10",
            Some(9_600) => "win81",
            Some(9_200) => "win8",
            Some(7_601) => "win7",
            _ => "win11",
        }
    }

    /// The Windows version in words.
    pub fn windows_name(&self) -> String {
        match self.windows_build {
            Some(b) if b >= 22_000 => "Windows 11".into(),
            Some(b) if b >= WINDOWS_10_BUILD => "Windows 10".into(),
            Some(9_600) => "Windows 8.1".into(),
            Some(9_200) => "Windows 8".into(),
            Some(7_601) => "Windows 7".into(),
            Some(3_790) => "Windows XP".into(),
            Some(b) => format!("Windows build {b}"),
            None => "an unknown Windows".into(),
        }
    }

    /// One line for the user.
    pub fn describe(&self) -> String {
        match (self.installed(), self.windows_ok()) {
            (true, true) => ".NET Framework 4.8 is installed".into(),
            (true, false) => format!(
                ".NET Framework 4.8 is installed, but the prefix is set to {} and the game will not start",
                self.windows_name()
            ),
            (false, _) => ".NET Framework 4.8 is not installed (only Wine's stub)".into(),
        }
    }
}

pub fn dotnet_state(prefix: &Prefix) -> DotnetState {
    let reg = std::fs::read_to_string(prefix.path.join("system.reg")).unwrap_or_default();
    let release = reg_value(
        &reg,
        "Software\\\\Microsoft\\\\NET Framework Setup\\\\NDP\\\\v4\\\\Full",
        "Release",
    )
    .and_then(|v| reg_dword(&v));
    let build = reg_value(
        &reg,
        "Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion",
        "CurrentBuild",
    )
    .and_then(|v| v.trim_matches('"').parse().ok());
    let mscorlib = std::fs::metadata(mscorlib(prefix)).map_or(0, |m| m.len());
    DotnetState {
        release,
        mscorlib,
        windows_build: build,
    }
}

fn mscorlib(prefix: &Prefix) -> PathBuf {
    resolve_ci(
        &prefix.path.join("drive_c"),
        "windows/Microsoft.NET/Framework64/v4.0.30319/mscorlib.dll",
    )
}

/// The raw value of `name` in a section of a Wine registry file (sections
/// are written with doubled backslashes: `Software\\Microsoft\\...`).
fn reg_value(text: &str, section: &str, name: &str) -> Option<String> {
    let header = format!("[{section}]");
    let key = format!("\"{name}\"=");
    let mut lines = text.lines();
    lines.find(|l| l.starts_with(&header))?;
    lines
        .take_while(|l| !l.trim().is_empty())
        .find_map(|l| l.strip_prefix(&key).map(str::to_string))
}

fn reg_dword(value: &str) -> Option<u32> {
    u32::from_str_radix(value.strip_prefix("dword:")?, 16).ok()
}

/// What the prefix means for ScriptHookVDotNet, for the checks of the game setup.
#[derive(Debug, Clone)]
pub struct Report {
    /// ScriptHookVDotNet is in the game folder.
    pub shvdn: bool,
    pub state: DotnetState,
}

/// The state of the game's prefix; `None` outside Steam/Proton.
pub fn report(game: &Game) -> Option<Report> {
    let prefix = find(game).ok()?;
    Some(Report {
        shvdn: resolve_ci(&game.root, "ScriptHookVDotNet.asi").is_file(),
        state: dotnet_state(&prefix),
    })
}

impl Report {
    /// What is wrong, if anything.
    pub fn problem(&self) -> Option<String> {
        if !self.state.windows_ok() {
            return Some(format!(
                "the game's Wine prefix is set to {}, and the game will not start: Prefix tweaks sets it back",
                self.state.windows_name()
            ));
        }
        (self.shvdn && !self.state.installed())
            .then(|| "ScriptHookVDotNet is installed but the game's Wine prefix has no .NET Framework 4.8, so its scripts do not run: Prefix tweaks installs it".into())
    }

    /// Good news worth a line: ScriptHookVDotNet can run.
    pub fn ok_line(&self) -> Option<String> {
        (self.shvdn && self.problem().is_none()).then(|| ".NET Framework 4.8 is installed in the game's Wine prefix: ScriptHookVDotNet scripts can run".into())
    }
}

/// The shell of [`is_running`] on the host: succeeds when some process has
/// the prefix (`$1`) or its Steam data folder (`$2`) in its environment.
const RUNNING_ON_HOST: &str = "for e in /proc/[0-9]*/environ; do { tr '\\0' '\\n' < \"$e\"; } 2>/dev/null; done | grep -qxF -e \"WINEPREFIX=$1\" -e \"WINEPREFIX=$1/\" -e \"STEAM_COMPAT_DATA_PATH=$2\" -e \"STEAM_COMPAT_DATA_PATH=$2/\"";

/// Whether a process is using the prefix (the game, or a wineserver of it).
/// Steam gives the processes of a game the prefix in their environment.
pub fn is_running(prefix: &Prefix) -> bool {
    if let Some(spawn) = host_spawn() {
        // The processes of the host are not in this sandbox's /proc.
        return Command::new(spawn)
            .args(["--host", "sh", "-c", RUNNING_ON_HOST, "sh"])
            .arg(&prefix.path)
            .arg(&prefix.compat)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    }
    let wanted = [
        format!("WINEPREFIX={}", prefix.path.display()),
        format!("STEAM_COMPAT_DATA_PATH={}", prefix.compat.display()),
    ];
    let trim = |s: &[u8]| String::from_utf8_lossy(s).trim_end_matches('/').to_string();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
                && std::fs::read(e.path().join("environ")).is_ok_and(|env| {
                    env.split(|b| *b == 0)
                        .any(|var| wanted.iter().any(|w| trim(var) == *w))
                })
        })
}

// ---- backups ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// "<app id>-<seconds since 1970>".
    pub id: String,
    pub label: String,
    /// Holds the copy, in `pfx/`.
    pub dir: PathBuf,
}

impl Backup {
    pub fn time(&self) -> Option<SystemTime> {
        let secs = self.id.rsplit('-').next()?.parse().ok()?;
        Some(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

pub fn backups_dir() -> PathBuf {
    data_dir().join("prefix-backups")
}

/// The backups of this game's prefix, newest first.
pub fn backups(prefix: &Prefix) -> Vec<Backup> {
    backups_in(&backups_dir(), prefix)
}

fn backups_in(base: &Path, prefix: &Prefix) -> Vec<Backup> {
    let start = format!("{}-", prefix.app_id);
    let mut out: Vec<(u64, Backup)> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let secs = id.strip_prefix(&start)?.parse::<u64>().ok()?;
            let dir = e.path();
            dir.join("pfx").is_dir().then_some(())?;
            let label = std::fs::read_to_string(dir.join("label.txt"))
                .unwrap_or_default()
                .trim()
                .to_string();
            Some((secs, Backup { id, label, dir }))
        })
        .collect();
    out.sort_by_key(|(secs, _)| std::cmp::Reverse(*secs));
    out.into_iter().map(|(_, b)| b).collect()
}

/// Copies the prefix to the backups folder.
pub fn backup(prefix: &Prefix, label: &str) -> Result<Backup, Error> {
    backup_in(&backups_dir(), prefix, label)
}

fn backup_in(base: &Path, prefix: &Prefix, label: &str) -> Result<Backup, Error> {
    let mut secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    while base.join(format!("{}-{secs}", prefix.app_id)).exists() {
        secs += 1;
    }
    let id = format!("{}-{secs}", prefix.app_id);
    let dir = base.join(&id);
    let partial = base.join(format!(".{id}.partial"));
    let _ = std::fs::remove_dir_all(&partial);
    let result = crate::snapshots::copy_tree(&prefix.path, &partial.join("pfx")).and_then(|_| {
        std::fs::write(partial.join("label.txt"), label.trim())?;
        std::fs::rename(&partial, &dir)?;
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    result?;
    Ok(Backup {
        id,
        label: label.trim().to_string(),
        dir,
    })
}

/// Puts a backup in place of the prefix, after a backup of the current one.
/// The current prefix is replaced only once the copy is complete.
pub fn restore(prefix: &Prefix, id: &str) -> Result<(), Error> {
    restore_in(&backups_dir(), prefix, id)
}

fn restore_in(base: &Path, prefix: &Prefix, id: &str) -> Result<(), Error> {
    let chosen = backups_in(base, prefix)
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| Error::Prefix(format!("no backup {id}")))?;
    if is_running(prefix) {
        return Err(Error::Prefix("the game is running: close it first".into()));
    }
    backup_in(base, prefix, "before restoring a backup")?;
    let incoming = prefix.compat.join(".pfx.restoring");
    let outgoing = prefix.compat.join(".pfx.replaced");
    for dir in [&incoming, &outgoing] {
        let _ = std::fs::remove_dir_all(dir);
    }
    if let Err(e) = crate::snapshots::copy_tree(&chosen.dir.join("pfx"), &incoming) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(e);
    }
    std::fs::rename(&prefix.path, &outgoing)?;
    if let Err(e) = std::fs::rename(&incoming, &prefix.path) {
        let _ = std::fs::rename(&outgoing, &prefix.path);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&outgoing);
    Ok(())
}

pub fn delete_backup(prefix: &Prefix, id: &str) -> Result<(), Error> {
    let chosen = backups(prefix)
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| Error::Prefix(format!("no backup {id}")))?;
    std::fs::remove_dir_all(chosen.dir)?;
    Ok(())
}

// ---- installing .NET ---------------------------------------------------------

/// Where an operation reports to, and how it is told to stop.
pub struct Run<'a> {
    pub log: &'a mut dyn FnMut(&str),
    pub cancel: &'a AtomicBool,
}

impl Run<'_> {
    pub fn say(&mut self, line: &str) {
        (self.log)(line);
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Prefix("cancelled".into()))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Back the prefix up first.
    pub backup: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { backup: true }
    }
}

/// A file the installation needs and whether it is already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needed {
    pub name: &'static str,
    pub size: u64,
    pub cached: bool,
}

fn cache_dir() -> PathBuf {
    data_dir().join("winetricks")
}

fn download_path(d: &Download) -> PathBuf {
    if d.verb.is_empty() {
        cache_dir().join(d.name)
    } else {
        cache_dir().join("cache").join(d.verb).join(d.name)
    }
}

fn is_cached(d: &Download) -> bool {
    let path = download_path(d);
    std::fs::metadata(&path).is_ok_and(|m| m.len() == d.size)
        && file_sha256(&path).is_ok_and(|h| h == d.sha256)
}

/// What the installation downloads, and what is already in the cache.
pub fn needed_downloads() -> Vec<Needed> {
    DOWNLOADS
        .iter()
        .map(|d| Needed {
            name: d.name,
            size: d.size,
            cached: is_cached(d),
        })
        .collect()
}

fn file_sha256(path: &Path) -> Result<String, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fetch(d: &Download, run: &mut Run) -> Result<(), Error> {
    if is_cached(d) {
        return Ok(());
    }
    run.say(&format!(
        "Downloading {} ({} MB)",
        d.name,
        d.size.div_ceil(1 << 20)
    ));
    let path = download_path(d);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let partial = path.with_extension("partial");
    let response = ureq::get(d.url)
        .call()
        .map_err(|e| Error::Download(e.to_string()))?;
    let mut body = response.into_body().into_reader();
    let mut file = std::fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 << 10];
    let (mut done, mut reported) = (0u64, 0u64);
    loop {
        if let Err(e) = run.check() {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        let n = body
            .read(&mut buf)
            .map_err(|e| Error::Download(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        if done - reported >= (16 << 20) {
            reported = done;
            run.say(&format!(
                "  {} of {} MB",
                done >> 20,
                d.size.div_ceil(1 << 20)
            ));
        }
    }
    drop(file);
    if hex(&hasher.finalize()) != d.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Download(format!(
            "{} does not match its checksum",
            d.name
        )));
    }
    std::fs::rename(&partial, &path)?;
    Ok(())
}

/// A private runtime folder for the processes of Wine, instead of the user's.
fn runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("paleto-wine-{}", std::process::id()))
}

/// Cuts a command off from the desktop. Without a Wayland display set, a
/// program connects to `wayland-0` in the user's runtime folder, so that folder
/// is replaced too. Whatever Wine runs (it opens message boxes for failures it
/// ignores) can then not open a window on the desktop.
fn isolate(cmd: &mut Command) {
    let runtime = runtime_dir();
    let _ = std::fs::create_dir_all(&runtime);
    let _ = std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700));
    cmd.env("XDG_RUNTIME_DIR", runtime)
        .env("WAYLAND_DISPLAY", "paleto-no-display")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
}

/// The prefix's wineserver with one option (`-k` stops it, `-w` waits for it).
fn wineserver(prefix: &Prefix, option: &str) {
    let mut cmd = Command::new(&prefix.proton.wineserver);
    cmd.arg(option).env("WINEPREFIX", &prefix.path);
    isolate(&mut cmd);
    let _ = finish(cmd).status();
}

/// A command with Wine's environment for this prefix. `headless` cuts it off
/// from the desktop (for what needs no window); otherwise it keeps the user's
/// session, which the installers need.
fn wine_command(prefix: &Prefix, program: &Path, headless: bool) -> Command {
    let mut cmd = Command::new(program);
    if headless {
        isolate(&mut cmd);
    }
    cmd.env("WINEPREFIX", &prefix.path)
        .env("WINE", &prefix.proton.wine)
        .env("WINESERVER", &prefix.proton.wineserver)
        .env("WINEARCH", "win64")
        .env("W_CACHE", cache_dir().join("cache"))
        .env("WINEDEBUG", "-all")
        .current_dir(&prefix.path)
        .stdin(Stdio::null());
    cmd
}

/// Runs a command to its end, passing its output lines on (all of them to
/// the log file, the useful ones to `run`). Returns whether it succeeded and
/// all the lines. Cancelling kills it and stops the prefix's wineserver.
fn run_command(
    prefix: &Prefix,
    cmd: Command,
    run: &mut Run,
    file: &mut std::fs::File,
) -> Result<(bool, Vec<String>), Error> {
    let mut child = finish(cmd).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            BufReader::new(out)
                .lines()
                .map_while(Result::ok)
                .for_each(|l| drop(tx.send(l)))
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            BufReader::new(err)
                .lines()
                .map_while(Result::ok)
                .for_each(|l| drop(tx.send(l)))
        }));
    }
    drop(tx);
    let mut lines = Vec::new();
    let mut take = |line: String, run: &mut Run, lines: &mut Vec<String>| {
        let _ = writeln!(file, "{line}");
        if is_interesting(&line) {
            run.say(&line);
        }
        lines.push(line);
    };
    let mut cancelled = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => take(line, run, &mut lines),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if run.cancel.load(Ordering::Relaxed) && !cancelled {
            cancelled = true;
            stop(&mut child);
            wineserver(prefix, "-k");
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child.wait()?;
    if cancelled {
        return Err(Error::Prefix("cancelled".into()));
    }
    Ok((status.success(), lines))
}

/// winetricks' own lines, not Wine's debug output ("01dc:err:ole:...") or the
/// installer's file list.
fn is_interesting(line: &str) -> bool {
    let t = line.trim();
    let debug = t.len() > 5 && t.as_bytes()[4] == b':' && t.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit);
    !t.is_empty() && !debug && !t.starts_with("Preparing:") && !t.starts_with("ntsync:")
}

/// The files Wine says it could not create (error 5, access denied), as
/// Windows paths.
fn blocked_files(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let Some(i) = line.find("failed to create L\"") else {
            continue;
        };
        let rest = &line[i + "failed to create L\"".len()..];
        let Some(j) = rest.find("\" (error 5)") else {
            continue;
        };
        let path = rest[..j].replace("\\\\", "\\");
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Turns a symbolic link into a real copy of what it points to.
fn unlink(path: &Path) -> Result<(), Error> {
    let target = std::fs::canonicalize(path)?;
    std::fs::remove_file(path)?;
    std::fs::copy(&target, path)?;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(perm.mode() | 0o200);
    std::fs::set_permissions(path, perm)?;
    Ok(())
}

/// Unlinks every symbolic link under a folder, returning how many.
fn unlink_tree(dir: &Path) -> usize {
    let mut n = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            n += unlink_tree(&entry.path());
        } else if kind.is_symlink()
            && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file())
            && unlink(&entry.path()).is_ok()
        {
            n += 1;
        }
    }
    n
}

/// Whether the session has a display for the installers' windows: a Wayland
/// socket in the runtime folder, or an X11 display. `var` reads the environment.
fn display_available(var: &dyn Fn(&str) -> Option<String>) -> bool {
    let wayland = var("XDG_RUNTIME_DIR")
        .map(|dir| Path::new(&dir).join(var("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into())))
        .is_some_and(|socket| socket.exists());
    wayland || var("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// The tool that runs a command on the host, which is where Wine runs when the
/// program is in a Flatpak: its sandbox forbids the 32-bit processes Proton's
/// Wine starts (the system call filter kills their loader, with a crash
/// report) and has no 32-bit libraries for them.
const SPAWN: &str = "flatpak-spawn";

fn host_spawn() -> Option<PathBuf> {
    Path::new("/.flatpak-info")
        .exists()
        .then(|| PathBuf::from(SPAWN))
}

/// Wine runs on the host, because this is a Flatpak.
pub fn on_host() -> bool {
    host_spawn().is_some()
}

const NO_HOST: &str = "This Flatpak is not allowed to run Wine on the host. Allow it with: flatpak override --user --talk-name=org.freedesktop.Flatpak io.github.the_beper.PaletoModkit (or use the AppImage or the tarball)";

/// Why Wine cannot be run, if it cannot: in a Flatpak, running commands on
/// the host needs the permission to talk to `org.freedesktop.Flatpak`.
pub fn sandbox_problem() -> Option<&'static str> {
    static PROBLEM: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PROBLEM.get_or_init(current_sandbox_problem)
}

/// [`sandbox_problem`] checked now, for when the user has just granted the permission.
pub fn current_sandbox_problem() -> Option<&'static str> {
    host_problem(on_host(), || {
        Command::new(SPAWN)
            .args(["--host", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

fn host_problem(sandboxed: bool, allowed: impl FnOnce() -> bool) -> Option<&'static str> {
    (sandboxed && !allowed()).then_some(NO_HOST)
}

/// `cmd` as a command that runs it on the host: `flatpak-spawn --host`, its
/// directory, then `env` to unset and set its variables (the host gives its own
/// otherwise). The user's session variables are passed on unless `cmd` sets or
/// removes them, so what needs a window can open one.
fn host_command(cmd: &Command, spawn: &Path, var: &dyn Fn(&str) -> Option<String>) -> Command {
    let mut out = Command::new(spawn);
    out.arg("--host");
    if let Some(dir) = cmd.get_current_dir() {
        out.arg(format!("--directory={}", dir.display()));
    }
    out.arg("env");
    let envs: Vec<_> = cmd.get_envs().collect();
    for (name, value) in &envs {
        if value.is_none() {
            out.arg("-u").arg(name);
        }
    }
    for name in ["WAYLAND_DISPLAY", "DISPLAY", "XDG_RUNTIME_DIR"] {
        if !envs.iter().any(|(k, _)| *k == name)
            && let Some(value) = var(name)
        {
            out.arg(format!("{name}={value}"));
        }
    }
    for (name, value) in &envs {
        if let Some(value) = value {
            let mut pair = name.to_os_string();
            pair.push("=");
            pair.push(value);
            out.arg(pair);
        }
    }
    out.arg(cmd.get_program()).args(cmd.get_args());
    out.stdin(Stdio::null());
    out
}

/// What to spawn for `cmd`: itself, or, in a Flatpak, the same on the host
/// (with the private runtime folder of the isolated commands made there too).
fn finish(cmd: Command) -> Command {
    let Some(spawn) = host_spawn() else {
        return cmd;
    };
    let runtime = runtime_dir();
    let isolated = cmd
        .get_envs()
        .any(|(k, v)| k == "XDG_RUNTIME_DIR" && v.is_some_and(|v| v == runtime.as_os_str()));
    if isolated {
        let _ = Command::new(&spawn)
            .args(["--host", "mkdir", "-p", "-m", "700"])
            .arg(&runtime)
            .status();
    }
    host_command(&cmd, &spawn, &|name| std::env::var(name).ok())
}

/// Removes the private runtime folder, here and on the host.
fn remove_runtime() {
    let _ = std::fs::remove_dir_all(runtime_dir());
    if let Some(spawn) = host_spawn() {
        let _ = Command::new(spawn)
            .args(["--host", "rm", "-rf"])
            .arg(runtime_dir())
            .status();
    }
}

/// Stops a command started by [`finish`]: from a Flatpak that is the
/// `flatpak-spawn` client, which hands SIGTERM on to the command on the host
/// (killing the client would leave the command running).
fn stop(child: &mut std::process::Child) {
    if on_host() {
        // SAFETY: kill(2) with the pid of our own child.
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    } else {
        let _ = child.kill();
    }
}

/// Installs .NET Framework 4.8 in the prefix.
pub fn install_dotnet(prefix: &Prefix, options: &Options, run: &mut Run) -> Result<(), Error> {
    if let Some(why) = sandbox_problem() {
        return Err(Error::Prefix(why.into()));
    }
    if is_running(prefix) {
        return Err(Error::Prefix(
            "the game (or Wine) is using the prefix: close it first".into(),
        ));
    }
    if !display_available(&|name| std::env::var(name).ok()) {
        return Err(Error::Prefix(
            "there is no graphical session: Microsoft's installer needs one for its windows (it keeps them hidden)".into(),
        ));
    }
    let before = dotnet_state(prefix);
    let log_path = data_dir().join("prefix-tweaks.log");
    std::fs::create_dir_all(data_dir())?;
    let mut file = std::fs::File::create(&log_path)?;
    run.say(&format!("Prefix: {}", prefix.path.display()));
    run.say(&format!("Proton: {}", prefix.proton.name));
    let result = run.check().and_then(|()| {
        for d in &DOWNLOADS {
            fetch(d, run)?;
        }
        install_steps(
            prefix,
            options,
            run,
            &mut file,
            &before,
            &download_path(&DOWNLOADS[0]),
            &backups_dir(),
        )
    });
    remove_runtime();
    if let Err(e) = &result {
        let _ = writeln!(file, "stopped: {e}");
        if !matches!(e, Error::Prefix(m) if m == "cancelled") {
            run.say(&format!("The full output is in {}", log_path.display()));
        }
    }
    result
}

/// Everything after the downloads, with winetricks at `script` and the backups in `backups`.
fn install_steps(
    prefix: &Prefix,
    options: &Options,
    run: &mut Run,
    file: &mut std::fs::File,
    before: &DotnetState,
    script: &Path,
    backups: &Path,
) -> Result<(), Error> {
    run.check()?;
    if options.backup {
        run.say("Backing the prefix up");
        let b = backup_in(backups, prefix, "before installing .NET")?;
        run.say(&format!("  saved as {}", b.dir.display()));
    }
    run.check()?;
    let net = resolve_ci(&prefix.path.join("drive_c"), "windows/Microsoft.NET");
    run.say(&format!(
        "Unlinking {} Proton links in the .NET folders",
        unlink_tree(&net)
    ));

    let mut attempt = 0;
    loop {
        attempt += 1;
        run.check()?;
        run.say(if attempt == 1 {
            "Installing .NET Framework 4.8 (a few minutes)"
        } else {
            "Trying again"
        });
        // The installer opens windows (hidden: it runs quiet) and waits for them for ever when there
        // is no display to open them on: it gets the user's session.
        let mut cmd = wine_command(prefix, Path::new("sh"), false);
        cmd.arg(script)
            .args(["--force", "-q", "dotnet48"])
            .env("WINEDEBUG", "err+all,fixme-all,warn+msi");
        let (ok, lines) = run_command(prefix, cmd, run, file)?;
        if ok {
            break;
        }
        let blocked = blocked_files(&lines);
        let mut fixed = 0;
        for path in &blocked {
            // "C:\windows\system32\dxva2.dll" -> the file in the prefix, whatever its case.
            let rel = path
                .trim_start_matches("C:")
                .trim_start_matches('\\')
                .replace('\\', "/");
            let file_path = resolve_ci(&prefix.path.join("drive_c"), &rel);
            if std::fs::symlink_metadata(&file_path).is_ok_and(|m| m.file_type().is_symlink())
                && unlink(&file_path).is_ok()
            {
                run.say(&format!("  unlinked {path}"));
                fixed += 1;
            }
        }
        if fixed == 0 || attempt >= MAX_ATTEMPTS {
            return Err(Error::Prefix(if blocked.is_empty() {
                "winetricks failed".into()
            } else {
                format!("the installer cannot write {}", blocked.join(", "))
            }));
        }
    }

    // winetricks leaves the prefix on Windows XP or 7: put back what it was.
    let windows = before.winecfg_name();
    run.say(&format!("Setting the Windows version back to {windows}"));
    let quiet = |args: &[&str], run: &mut Run, file: &mut std::fs::File| -> Result<bool, Error> {
        let mut cmd = wine_command(prefix, &prefix.proton.wine, true);
        cmd.args(args);
        Ok(run_command(prefix, cmd, run, file)?.0)
    };
    quiet(&["winecfg", "-v", windows], run, file)?;
    run.say("Updating the prefix");
    quiet(&["wineboot", "-u"], run, file)?;
    wineserver(prefix, "-w");

    let after = dotnet_state(prefix);
    if !after.installed() {
        return Err(Error::Prefix(
            ".NET Framework 4.8 is still not installed after the installer ran".into(),
        ));
    }
    if !after.windows_ok() {
        return Err(Error::Prefix(format!(
            "the prefix is still set to {}",
            after.windows_name()
        )));
    }
    run.say(&after.describe());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG: &str = "WINE REGISTRY Version 2\n\n[Software\\\\Microsoft\\\\NET Framework Setup\\\\NDP\\\\v4\\\\Full] 1791542463\n#time=1dd\n\"Install\"=dword:00000001\n\"Release\"=dword:00080eb1\n\"Version\"=\"4.8.04084\"\n\n[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion] 1791\n\"CurrentBuild\"=\"22000\"\n\n";

    #[test]
    fn reads_the_registry() {
        let key = "Software\\\\Microsoft\\\\NET Framework Setup\\\\NDP\\\\v4\\\\Full";
        assert_eq!(
            reg_value(REG, key, "Release").as_deref(),
            Some("dword:00080eb1")
        );
        assert_eq!(reg_dword("dword:00080eb1"), Some(528_049));
        assert_eq!(reg_value(REG, key, "CurrentBuild"), None);
        assert_eq!(
            reg_value(
                REG,
                "Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion",
                "CurrentBuild"
            )
            .as_deref(),
            Some("\"22000\"")
        );
    }

    #[test]
    fn the_host_needs_a_permission_in_a_flatpak() {
        assert_eq!(host_problem(false, || false), None, "not in a Flatpak: nothing to allow");
        assert_eq!(host_problem(true, || true), None);
        let why = host_problem(true, || false).unwrap();
        assert!(why.contains("--talk-name=org.freedesktop.Flatpak") && why.contains("AppImage"), "{why}");
    }

    /// A stand-in for flatpak-spawn: runs what follows the host options, from the
    /// directory asked for, in the "host" environment it makes up (B is set there).
    fn fake_spawn(dir: &Path) -> PathBuf {
        let path = dir.join("flatpak-spawn");
        std::fs::write(
            &path,
            "#!/bin/sh\n[ \"$1\" = --host ] && shift\nwhile case \"$1\" in --directory=*) cd \"${1#--directory=}\"; true;; --*) true;; *) false;; esac; do shift; done\nB=fromhost; export B\nexec \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_command_runs_on_the_host_as_it_would_here() {
        let dir = tempfile::tempdir().unwrap();
        let spawn = fake_spawn(dir.path());
        let work = dir.path().join("a folder");
        std::fs::create_dir(&work).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo \"$A|${B-unset}|$WAYLAND_DISPLAY|$PWD|$1\"", "sh", "two words"])
            .env("A", "x=y")
            .env_remove("B")
            .current_dir(&work);
        let session = |name: &str| (name == "WAYLAND_DISPLAY").then(|| "wayland-7".to_string());
        let out = host_command(&cmd, &spawn, &session).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        // A set (its value kept whole), B unset although the host has it, the
        // session passed on, the folder and the arguments as given.
        assert_eq!(text.trim(), format!("x=y|unset|wayland-7|{}|two words", work.display()), "{text}");
        // What the command sets itself is not replaced by the session's.
        let mut own = Command::new("sh");
        own.args(["-c", "echo $WAYLAND_DISPLAY"]).env("WAYLAND_DISPLAY", "mine");
        let out = host_command(&own, &spawn, &session).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "mine");
    }

    #[test]
    fn the_running_check_of_the_host_finds_the_prefix() {
        // The shell script, on a process that has the prefix in its environment.
        let run = |prefix: &str| {
            Command::new("sh")
                .args(["-c", RUNNING_ON_HOST, "sh", prefix, "/compat"])
                .status()
                .unwrap()
                .success()
        };
        let mut child = Command::new("sleep").arg("5").env("WINEPREFIX", "/steam/pfx/").spawn().unwrap();
        let found = run("/steam/pfx");
        let other = run("/steam/other");
        let _ = child.kill();
        let _ = child.wait();
        assert!(found, "a process with WINEPREFIX=/steam/pfx/ is using /steam/pfx");
        assert!(!other);
    }

    #[test]
    fn stub_is_not_net() {
        // Wine's stub claims a Release too, but its mscorlib is small.
        let stub = DotnetState {
            release: Some(533_320),
            mscorlib: 752_128,
            windows_build: Some(22_000),
        };
        assert!(!stub.installed());
        let real = DotnetState {
            release: Some(528_049),
            mscorlib: 5_436_976,
            windows_build: Some(22_000),
        };
        assert!(real.installed() && real.windows_ok());
        let xp = DotnetState {
            windows_build: Some(3_790),
            ..real.clone()
        };
        assert!(xp.installed() && !xp.windows_ok());
        assert_eq!(xp.winecfg_name(), "win11");
        assert_eq!(
            DotnetState {
                windows_build: Some(19_045),
                ..real.clone()
            }
            .winecfg_name(),
            "win10"
        );
        assert_eq!(
            DotnetState {
                windows_build: Some(7_601),
                ..real
            }
            .winecfg_name(),
            "win7"
        );
    }

    #[test]
    fn finds_blocked_files() {
        let lines = vec![
            "013c:warn:msi:cabinet_copy_file failed to create L\"C:\\\\windows\\\\system32\\\\dxva2.dll\" (error 5)".to_string(),
            "013c:warn:msi:cabinet_copy_file failed to create L\"C:\\\\windows\\\\system32\\\\dxva2.dll\" (error 5)".to_string(),
            "013c:warn:msi:cabinet_copy_file failed to create L\"C:\\\\x\\\\y.dll\" (error 32)".to_string(),
            "something else".to_string(),
        ];
        assert_eq!(
            blocked_files(&lines),
            vec!["C:\\windows\\system32\\dxva2.dll".to_string()]
        );
    }

    #[test]
    fn proton_from_config_info() {
        let dir = tempfile::tempdir().unwrap();
        let proton = dir.path().join("Proton X");
        std::fs::create_dir_all(proton.join("files/bin")).unwrap();
        std::fs::write(proton.join("files/bin/wine"), b"").unwrap();
        let compat = dir.path().join("compat");
        std::fs::create_dir_all(&compat).unwrap();
        std::fs::write(
            compat.join("config_info"),
            format!(
                "X-1\n{}/files/share/fonts/\n{}/files/lib/\n",
                proton.display(),
                proton.display()
            ),
        )
        .unwrap();
        let found = proton_of(&compat).unwrap();
        assert_eq!(found.name, "Proton X");
        assert_eq!(found.wine, proton.join("files/bin/wine"));
        std::fs::remove_file(compat.join("config_info")).unwrap();
        assert!(proton_of(&compat).is_err());
    }

    /// A fake Proton (scripts for wine and wineserver) and a fake prefix whose
    /// system files are links to the Proton, like Proton makes them.
    fn fake(dir: &Path) -> Prefix {
        use std::os::unix::fs::symlink;
        let write = |path: &Path, text: &str, mode: u32| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let proton = dir.join("Proton X");
        // winecfg -v VERSION sets the build in the registry, like the real one.
        write(
            &proton.join("files/bin/wine"),
            "#!/bin/sh\nif [ \"$1\" = winecfg ]; then\n case \"$3\" in win11) b=22000;; win10) b=19045;; win7) b=7601;; *) b=3790;; esac\n sed -i \"s/\\\"CurrentBuild\\\"=\\\"[0-9]*\\\"/\\\"CurrentBuild\\\"=\\\"$b\\\"/\" \"$WINEPREFIX/system.reg\"\nfi\nexit 0\n",
            0o755,
        );
        write(
            &proton.join("files/bin/wineserver"),
            "#!/bin/sh\nexit 0\n",
            0o755,
        );
        write(&proton.join("files/share/dxva2.dll"), "builtin", 0o555);
        write(&proton.join("files/share/ngen.exe"), "builtin", 0o555);
        let compat = dir.join("steamapps/compatdata/3240220");
        let pfx = compat.join("pfx");
        std::fs::create_dir_all(pfx.join("drive_c/windows/system32")).unwrap();
        std::fs::create_dir_all(pfx.join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319"))
            .unwrap();
        symlink(
            proton.join("files/share/dxva2.dll"),
            pfx.join("drive_c/windows/system32/dxva2.dll"),
        )
        .unwrap();
        symlink(
            proton.join("files/share/ngen.exe"),
            pfx.join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319/ngen.exe"),
        )
        .unwrap();
        // Wine's stub claims a Release, and the prefix is set to Windows 11.
        std::fs::write(
            pfx.join("system.reg"),
            "[Software\\\\Microsoft\\\\NET Framework Setup\\\\NDP\\\\v4\\\\Full] 1\n\"Release\"=dword:00082348\n\n[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion] 1\n\"CurrentBuild\"=\"22000\"\n\n",
        )
        .unwrap();
        std::fs::write(
            compat.join("config_info"),
            format!("X-1\n{}/files/share/\n", proton.display()),
        )
        .unwrap();
        proton_of(&compat)
            .map(|proton| Prefix {
                app_id: "3240220",
                compat,
                path: pfx,
                proton,
            })
            .unwrap()
    }

    /// A winetricks that fails like the real one while a Proton link stands in the
    /// way, then installs: a real mscorlib, and Windows left on XP.
    const WINETRICKS: &str = "#!/bin/sh\nP=\"$WINEPREFIX/drive_c/windows\"\necho run >> \"$WINEPREFIX/runs\"\nif [ -L \"$P/system32/dxva2.dll\" ]; then\n echo '0001:warn:msi:cabinet_copy_file failed to create L\"C:\\\\windows\\\\SYSTEM32\\\\Dxva2.dll\" (error 5)' >&2\n echo 'Executing winetricks dotnet48'\n exit 1\nfi\nhead -c 3000000 /dev/zero > \"$P/Microsoft.NET/Framework64/v4.0.30319/mscorlib.dll\"\nsed -i 's/dword:00082348/dword:00080eb1/; s/\"CurrentBuild\"=\"[0-9]*\"/\"CurrentBuild\"=\"3790\"/' \"$WINEPREFIX/system.reg\"\nexit 0\n";

    fn run_install(
        prefix: &Prefix,
        script: &Path,
        options: &Options,
        cancel: &AtomicBool,
    ) -> (Result<(), Error>, Vec<String>, tempfile::TempDir) {
        let scratch = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(scratch.path().join("log")).unwrap();
        let mut lines = Vec::new();
        let mut log = |l: &str| lines.push(l.to_string());
        let mut run = Run {
            log: &mut log,
            cancel,
        };
        let before = dotnet_state(prefix);
        let result = install_steps(
            prefix,
            options,
            &mut run,
            &mut file,
            &before,
            script,
            &scratch.path().join("backups"),
        );
        let _ = std::fs::remove_dir_all(runtime_dir());
        (result, lines, scratch)
    }

    #[test]
    fn installs_after_unlinking_what_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let script = dir.path().join("winetricks");
        std::fs::write(&script, WINETRICKS).unwrap();
        assert!(!dotnet_state(&prefix).installed());

        let (result, lines, scratch) = run_install(
            &prefix,
            &script,
            &Options { backup: true },
            &AtomicBool::new(false),
        );
        result.unwrap_or_else(|e| panic!("{e}; log: {lines:?}"));
        // The link that blocked the installer (named in another case) became a real file; so did the .NET one.
        let dxva2 = prefix.path.join("drive_c/windows/system32/dxva2.dll");
        assert!(
            !std::fs::symlink_metadata(&dxva2)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&dxva2).unwrap(), b"builtin");
        assert!(
            !std::fs::symlink_metadata(
                prefix
                    .path
                    .join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319/ngen.exe")
            )
            .unwrap()
            .file_type()
            .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(prefix.path.join("runs"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("unlinked C:\\windows\\SYSTEM32\\Dxva2.dll")),
            "{lines:?}"
        );
        // The prefix is back on Windows 11 and has .NET.
        let state = dotnet_state(&prefix);
        assert!(state.installed() && state.windows_ok(), "{state:?}");
        assert_eq!(state.windows_build, Some(22_000));
        // The backup, made before anything changed, still has the links.
        let backup = backups_in(&scratch.path().join("backups"), &prefix);
        assert_eq!(backup.len(), 1);
        assert!(
            std::fs::symlink_metadata(backup[0].dir.join("pfx/drive_c/windows/system32/dxva2.dll"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn keeps_a_windows_10_prefix_on_windows_10() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let reg = std::fs::read_to_string(prefix.path.join("system.reg"))
            .unwrap()
            .replace("22000", "19045");
        std::fs::write(prefix.path.join("system.reg"), reg).unwrap();
        let script = dir.path().join("winetricks");
        std::fs::write(&script, WINETRICKS).unwrap();
        let (result, lines, _scratch) = run_install(
            &prefix,
            &script,
            &Options { backup: false },
            &AtomicBool::new(false),
        );
        result.unwrap_or_else(|e| panic!("{e}; {lines:?}"));
        assert_eq!(dotnet_state(&prefix).windows_build, Some(19_045));
    }

    #[test]
    fn reports_a_failure_it_cannot_fix_and_stops_when_told() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let script = dir.path().join("winetricks");
        std::fs::write(&script, "#!/bin/sh\necho broken >&2\nexit 1\n").unwrap();
        let (result, _, _s) = run_install(
            &prefix,
            &script,
            &Options { backup: false },
            &AtomicBool::new(false),
        );
        assert!(
            matches!(&result, Err(Error::Prefix(m)) if m == "winetricks failed"),
            "{result:?}"
        );

        let (result, _, _s) = run_install(
            &prefix,
            &script,
            &Options { backup: false },
            &AtomicBool::new(true),
        );
        assert!(
            matches!(&result, Err(Error::Prefix(m)) if m == "cancelled"),
            "{result:?}"
        );
    }

    #[test]
    fn backup_and_restore_keep_links() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let base = dir.path().join("backups");
        let first = backup_in(&base, &prefix, " before ").unwrap();
        assert_eq!(first.label, "before");
        // The prefix changes: a link becomes a file.
        let dxva2 = prefix.path.join("drive_c/windows/system32/dxva2.dll");
        std::fs::remove_file(&dxva2).unwrap();
        std::fs::write(&dxva2, "changed").unwrap();

        restore_in(&base, &prefix, &first.id).unwrap();
        assert!(
            std::fs::symlink_metadata(&dxva2)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&dxva2).unwrap(), b"builtin");
        // The state before restoring was kept as a backup too.
        let all = backups_in(&base, &prefix);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "before restoring a backup");
        assert!(
            !prefix.compat.join(".pfx.replaced").exists()
                && !prefix.compat.join(".pfx.restoring").exists()
        );
        assert!(restore_in(&base, &prefix, "3240220-1").is_err());
    }

    #[test]
    fn wine_cannot_reach_the_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let mut cmd = wine_command(&prefix, Path::new("sh"), true);
        cmd.args(["-c", "echo \"$XDG_RUNTIME_DIR|$WAYLAND_DISPLAY|${DISPLAY-unset}|${DBUS_SESSION_BUS_ADDRESS-unset}\""]);
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let parts: Vec<&str> = text.trim().split('|').collect();
        assert!(parts[0].contains("paleto-wine-"), "{text}");
        assert_eq!(&parts[1..], ["paleto-no-display", "unset", "unset"], "{text}");
        let _ = std::fs::remove_dir_all(runtime_dir());
    }

    #[test]
    fn needs_a_display_for_the_installers() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().to_string_lossy().into_owned();
        let env = |pairs: &'static [(&'static str, &'static str)], runtime: String| {
            move |name: &str| -> Option<String> {
                if name == "XDG_RUNTIME_DIR" {
                    return Some(runtime.clone());
                }
                pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
            }
        };
        assert!(!display_available(&env(&[], runtime.clone())));
        std::fs::write(dir.path().join("wayland-0"), b"").unwrap();
        assert!(display_available(&env(&[], runtime.clone())));
        // A named socket, and X11.
        assert!(!display_available(&env(&[("WAYLAND_DISPLAY", "wayland-9")], runtime.clone())));
        assert!(display_available(&env(&[("DISPLAY", ":0")], "/nonexistent".into())));
        assert!(!display_available(&env(&[("DISPLAY", "")], "/nonexistent".into())));
    }

    #[test]
    fn finds_the_prefix_of_a_steam_game() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake(dir.path());
        let root = dir
            .path()
            .join("steamapps/common/Grand Theft Auto V Enhanced");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        // Reached through a link, as a game folder often is.
        let link = dir.path().join("Games");
        std::os::unix::fs::symlink(dir.path().join("steamapps/common"), &link).unwrap();
        let game = Game::open(&link.join("Grand Theft Auto V Enhanced")).unwrap();
        let found = find(&game).unwrap();
        assert_eq!(found.path, std::fs::canonicalize(&prefix.path).unwrap());
        assert_eq!(found.proton.name, "Proton X");
        // Not in a Steam library: no prefix.
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("GTA5_Enhanced.exe"), b"").unwrap();
        assert!(find(&Game::open(elsewhere.path()).unwrap()).is_err());
        // The report names a missing .NET only when ScriptHookVDotNet is there.
        assert!(report(&game).unwrap().problem().is_none());
        std::fs::write(root.join("ScriptHookVDotNet.asi"), b"").unwrap();
        assert!(
            report(&game)
                .unwrap()
                .problem()
                .unwrap()
                .contains("no .NET Framework 4.8")
        );
    }
}
