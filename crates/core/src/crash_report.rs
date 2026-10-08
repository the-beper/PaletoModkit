//! What the last game crash says: the minidump the game writes
//! (`CrashLogs/*.dmp` in its user folder) read for the exception, the module
//! it happened in and the modules on the crashing thread's stack, with the
//! game's own `crashcontext.log` for the situation.
//!
//! The stack is not unwound (that needs the modules' unwind data): values on
//! it that point into a module's code are listed, nearest first, which is
//! enough to see an `.asi` plugin taking part.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use paleto_rpf::{Archive, Keys};

use crate::game::{Edition, Game};
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// File name ("GTA5_Enhanced.exe", "Trainer.asi").
    pub name: String,
    pub base: u64,
    pub size: u64,
}

/// One of the game's asset stores (textures, models, metadata…) as the crash
/// found it: the game allows `size` files of this kind and `used` were in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub name: String,
    pub size: u32,
    pub used: u32,
}

impl Store {
    /// No slot left: the next file of this kind cannot be registered, and the game stops.
    /// (A store of one slot, the text store, is always full.)
    pub fn full(&self) -> bool {
        self.size > 1 && self.used >= self.size
    }

    /// Full, or within 2% of it.
    pub fn tight(&self) -> bool {
        self.size > 1 && u64::from(self.used) * 100 >= u64::from(self.size) * 98
    }

    /// The pool of `gameconfig.xml` that sets its size, when it is known.
    pub fn pool(&self) -> Option<&'static str> {
        Some(match self.name.as_str() {
            "MetaDataStore" => "MetaDataStore",
            "DrawableStore" => "DrawableStore",
            "DwdStore" => "DwdStore",
            "FragmentStore" => "FragmentStore",
            "MapDataStore" => "MapDataStore",
            "TxdStore" => "TxdStore",
            "ScaleformStore" => "ScaleformStore",
            "fwStaticBoundsStore" => "StaticBounds",
            "ptfxAssetStore" => "PtFxAssetStore",
            _ => return None,
        })
    }
}

/// The last archive the game mounted before it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastArchive {
    /// The hash the log names it by.
    pub hash: u32,
    /// Its file name, as the log gives it ("occlusion.rpf").
    pub name: String,
    /// The add-on pack holding it (its folder in `dlcpacks`), when found.
    pub pack: Option<String>,
}

impl LastArchive {
    pub fn describe(&self) -> String {
        match &self.pack {
            Some(pack) => format!("{} of the pack {pack}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    pub dump: PathBuf,
    pub time: Option<SystemTime>,
    pub code: u32,
    /// Where the exception happened, and in which module (with the offset).
    pub address: u64,
    pub module: Option<(String, u64)>,
    /// For an access violation: "read", "write" or "execute", and the address.
    pub access: Option<(&'static str, u64)>,
    /// Modules whose code the crashing thread's stack points into, nearest
    /// first, each once.
    pub stack: Vec<String>,
    pub modules: Vec<Module>,
    /// Lines of the game's crashcontext.log worth showing.
    pub context: Vec<String>,
    /// How many of the other dumps crashed at the same place.
    pub same_place: usize,
    /// The asset stores at the crash (crashcontext.log).
    pub stores: Vec<Store>,
    /// The last archive mounted, and how many were of how many the game allows.
    pub last_archive: Option<LastArchive>,
    pub archives: Option<(u32, u32)>,
}

impl CrashReport {
    pub fn code_name(&self) -> &'static str {
        match self.code {
            0xC000_0005 => "access violation",
            0x8000_0003 => "breakpoint (the game stopped itself on an error)",
            0xC000_001D => "illegal instruction",
            0xC000_0094 => "integer division by zero",
            0xC000_00FD => "stack overflow",
            0xC000_0409 => "stack buffer overrun (fail fast)",
            0xC000_0374 => "heap corruption",
            0xE06D_7363 => "C++ exception",
            _ => "exception",
        }
    }

    /// The plugins (.asi, ScriptHookV) on the crashing thread's stack, or the
    /// faulting module if it is one.
    pub fn plugins_involved(&self) -> Vec<String> {
        let plugin = |n: &str| {
            let n = n.to_ascii_lowercase();
            n.ends_with(".asi") || n == "scripthookv.dll" || n == "scripthookvdotnet.dll"
        };
        let mut out: Vec<String> = Vec::new();
        for n in self.module.iter().map(|(n, _)| n).chain(&self.stack) {
            if plugin(n) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        out
    }

    /// The stores with no slot left, or nearly none.
    pub fn tight_stores(&self) -> Vec<&Store> {
        self.stores.iter().filter(|s| s.tight()).collect()
    }

    /// Reads what the game's crashcontext.log says: its situation, its asset
    /// stores and the archives it had mounted.
    pub fn read_log(&mut self, log: &str) {
        self.context = context_lines(log);
        self.stores = parse_stores(log);
        let (last, total) = parse_archives(log);
        self.last_archive = last.map(|(hash, name)| LastArchive { hash, name, pack: None });
        self.archives = total;
    }

    /// A plain reading of the report, in a sentence or two.
    pub fn verdict(&self, game_exe: &str) -> String {
        if let Some(store) = self.stores.iter().find(|s| s.full()) {
            let mut text = format!(
                "The game ran out of slots in its {} ({} of {}): the mods add more files of this kind than it allows.",
                store.name, store.used, store.size
            );
            match store.pool() {
                Some(pool) => text += &format!(" Raise {pool} in gameconfig.xml (common/data in update.rpf, the copy in the mods folder), or use a gameconfig made for mods."),
                None => text += " Use a gameconfig with larger pools, or remove mods that add such files.",
            }
            if let Some(archive) = &self.last_archive {
                text += &format!(" It was loading {} when it stopped.", archive.describe());
            }
            return text;
        }
        if let Some((mounted, limit)) = self.archives.filter(|(m, l)| m >= l) {
            return format!("The game reached its limit of {limit} archives ({mounted} mounted): a Packfile Limit Adjuster raises it, or remove mods that add archives.");
        }
        let plugins = self.plugins_involved();
        let faulting = self.module.as_ref().map(|(n, _)| n.to_ascii_lowercase()).unwrap_or_default();
        if faulting.ends_with(".asi") || faulting == "scripthookv.dll" {
            return format!(
                "The crash happened inside {}: update or remove that plugin (plugins made for another game version often crash after a game update).",
                self.module.as_ref().unwrap().0
            );
        }
        if !plugins.is_empty() {
            return format!(
                "{} {} on the crashing thread's stack: try the game without it (Find the crashing mod can confirm).",
                plugins.join(", "),
                if plugins.len() == 1 { "is" } else { "are" }
            );
        }
        if faulting == game_exe.to_ascii_lowercase() {
            return "The crash happened in the game's own code, with no plugin on the stack: most often a mod's file the game \
                    cannot read (a model or texture made for another edition, a broken .meta). Check Mods in game, or find \
                    the mod by halves."
                .into();
        }
        const GRAPHICS: &[&str] = &["d3d12", "dxgi", "vkd3d", "dxvk", "amdxc", "nvwgf", "nvapi", "winevulkan", "vulkan", "amd_fidelityfx", "sl.", "nvngx"];
        if GRAPHICS.iter().any(|g| faulting.contains(g)) {
            return format!(
                "The crash happened in {faulting}, part of the graphics stack (driver, Proton's Direct3D translation, upscaler): \
                 try other graphics settings (upscaler, frame generation) or another Proton version."
            );
        }
        if faulting.is_empty() {
            return "The crash address is outside every module: often code a plugin patched at runtime.".into();
        }
        format!("The crash happened in {faulting}.")
    }
}

/// Folders where the game writes its crash dumps (under Proton, in the
/// game's prefix), existing ones only.
pub fn crash_dirs(game: &Game) -> Vec<PathBuf> {
    let (app, folders): (&str, &[&str]) = match game.edition {
        Edition::Enhanced => ("3240220", &["GTAV Enhanced"]),
        Edition::Legacy => ("271590", &["GTA V", "GTAV"]),
    };
    let mut out = Vec::new();
    let mut users = vec![game.root.clone()];
    if let Some(steamapps) = std::fs::canonicalize(&game.root).ok().and_then(|r| r.parent()?.parent().map(Path::to_path_buf)) {
        users.push(steamapps.join(format!("compatdata/{app}/pfx/drive_c/users/steamuser/AppData/Local/Rockstar Games")));
    }
    for base in users {
        for f in folders {
            let dir = base.join(f).join("CrashLogs");
            if dir.is_dir() {
                out.push(dir);
            }
        }
    }
    out
}

/// The newest crash dump of the game, if any.
pub fn latest_dump(game: &Game) -> Option<PathBuf> {
    crash_dirs(game)
        .into_iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dmp")))
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// The report of the newest crash dump, `None` if the game never crashed.
pub fn latest(game: &Game) -> Result<Option<CrashReport>, Error> {
    latest_with_keys(game, None)
}

/// [`latest`], with the game's keys for looking in its encrypted add-on packs.
pub fn latest_with_keys(game: &Game, keys: Option<&Keys>) -> Result<Option<CrashReport>, Error> {
    let Some(dump) = latest_dump(game) else { return Ok(None) };
    let mut report = read_dump(&dump)?;
    // The game writes crashcontext.log with the dump (the newest crash only).
    let log = dump.with_file_name("crashcontext.log");
    let same_time = |a: &Path, b: &Path| {
        let t = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        t(a).zip(t(b)).is_some_and(|(x, y)| x.duration_since(y).or_else(|_| y.duration_since(x)).is_ok_and(|d| d.as_secs() < 60))
    };
    if same_time(&log, &dump) {
        report.read_log(&String::from_utf8_lossy(&std::fs::read(&log).unwrap_or_default()));
        if let Some(last) = &mut report.last_archive {
            last.pack = pack_of_archive(game, keys, last.hash);
        }
    }
    // The same crash again and again points to one cause.
    if let Some(place) = &report.module {
        report.same_place = crash_dirs(game)
            .into_iter()
            .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| *p != dump && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dmp")))
            .filter(|p| read_dump(p).is_ok_and(|r| r.module.as_ref() == Some(place)))
            .count();
    }
    Ok(Some(report))
}

/// The asset stores of crashcontext.log. Its rows come run together on one
/// line ("MetaDataStore, 3200, 3200DrawableStore, 81000, 65571…").
fn parse_stores(log: &str) -> Vec<Store> {
    let mut lines = log.lines().skip_while(|l| !l.contains("ASSET STORES INFO")).skip(1);
    let text: String = lines.by_ref().take_while(|l| !l.starts_with("====")).filter(|l| !l.starts_with("Store,")).collect();
    let number = |at: usize| -> Option<(u32, usize)> {
        let digits = text[at..].bytes().take_while(u8::is_ascii_digit).count();
        Some((text[at..at + digits].parse().ok()?, at + digits))
    };
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let name = text[at..].bytes().take_while(|b| b.is_ascii_alphabetic() || *b == b'_').count();
        let Some(rest) = text[at + name..].strip_prefix(", ").filter(|_| name > 0) else { break };
        let start = text.len() - rest.len();
        let Some((size, next)) = number(start) else { break };
        let Some(next) = text[next..].strip_prefix(", ").map(|r| text.len() - r.len()) else { break };
        let Some((used, end)) = number(next) else { break };
        out.push(Store { name: text[at..at + name].to_string(), size, used });
        at = end;
    }
    out
}

/// The last archive of the log's mounted list (hash and name), and its
/// "Total : 4030 / 4265" line.
type Mounted = (u32, String);

fn parse_archives(log: &str) -> (Option<Mounted>, Option<(u32, u32)>) {
    let mut last = None;
    let mut total = None;
    for line in log.lines().skip_while(|l| !l.contains("PACK INFO")).skip(1).take_while(|l| !l.starts_with("====")) {
        if let Some(rest) = line.strip_prefix("Total :") {
            total = rest.split_once('/').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
        } else if let Some((hash, name)) = line.split_once(" : ")
            && let Some(hex) = hash.trim().strip_prefix("0x")
            && let Ok(hash) = u32::from_str_radix(hex, 16)
        {
            last = Some((hash, name.trim().to_string()));
        }
    }
    (last, total)
}

/// The add-on pack whose archive the game names by `hash`: the game hashes
/// `<device>:/<path inside the pack>` in lower case, the device being the
/// `deviceName` of the pack's setup2.xml.
fn pack_of_archive(game: &Game, keys: Option<&Keys>, hash: u32) -> Option<String> {
    for dir in [game.mods_dir().join("update/x64/dlcpacks"), game.root.join("update/x64/dlcpacks")] {
        let mut packs: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().collect();
        packs.sort_by_key(|e| e.file_name());
        for pack in packs {
            let files = std::fs::read_dir(pack.path()).into_iter().flatten().flatten();
            for file in files.map(|f| f.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpf"))) {
                let Ok(archive) = Archive::open(&file, keys) else { continue };
                let entries = archive.walk(0);
                let device = entries
                    .iter()
                    .find(|(p, _)| p.eq_ignore_ascii_case("setup2.xml"))
                    .and_then(|(_, i)| archive.read(*i, keys).ok())
                    .and_then(|data| {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        let from = text.find("<deviceName>")? + "<deviceName>".len();
                        Some(text[from..from + text[from..].find("</deviceName>")?].trim().to_string())
                    });
                let Some(device) = device else { continue };
                let found = entries
                    .iter()
                    .filter(|(_, i)| archive.entries[*i].is_archive())
                    .any(|(path, _)| paleto_formats::hash::joaat_lower(&format!("{device}:/{path}")) == hash);
                if found {
                    return Some(pack.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

fn context_lines(log: &str) -> Vec<String> {
    const KEYS: &[&str] = &["Game State", "System Time", "Version", "Current Mission", "Current Vehicle", "Player Position", "Loading"];
    log.lines()
        .map(str::trim)
        .filter(|l| KEYS.iter().any(|k| l.starts_with(k)))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Reads a minidump (x64).
pub fn read_dump(path: &Path) -> Result<CrashReport, Error> {
    let d = std::fs::read(path)?;
    let bad = || Error::Package(format!("{}: not a readable minidump", path.display()));
    if d.get(..4) != Some(b"MDMP") {
        return Err(bad());
    }
    let (streams, dir) = (u32_at(&d, 8).ok_or_else(bad)? as usize, u32_at(&d, 12).ok_or_else(bad)? as usize);
    let stream = |kind: u32| -> Option<(usize, usize)> {
        (0..streams).find_map(|i| {
            let at = dir + i * 12;
            (u32_at(&d, at)? == kind).then(|| Some((u32_at(&d, at + 8)? as usize, u32_at(&d, at + 4)? as usize))).flatten()
        })
    };
    // The dump's own time, or the file's when it has none.
    let time = u32_at(&d, 20)
        .filter(|&t| t != 0)
        .map(|t| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t.into()))
        .or_else(|| std::fs::metadata(path).and_then(|m| m.modified()).ok());

    // Modules (stream 4).
    let mut modules = Vec::new();
    if let Some((at, _)) = stream(4) {
        let n = u32_at(&d, at).unwrap_or(0) as usize;
        for i in 0..n {
            let m = at + 4 + i * 108;
            let (Some(base), Some(size), Some(name_rva)) = (u64_at(&d, m), u32_at(&d, m + 8), u32_at(&d, m + 20)) else { break };
            let name = minidump_string(&d, name_rva as usize).unwrap_or_default();
            let name = name.rsplit(['\\', '/']).next().unwrap_or(&name).to_string();
            modules.push(Module { name, base, size: size.into() });
        }
    }
    let module_of = |addr: u64| modules.iter().find(|m| addr >= m.base && addr < m.base + m.size).map(|m| (m.name.clone(), addr - m.base));

    // Exception (stream 6): thread id, record, then the thread's context.
    let (at, _) = stream(6).ok_or_else(bad)?;
    let thread = u32_at(&d, at).ok_or_else(bad)?;
    let rec = at + 8;
    let code = u32_at(&d, rec).ok_or_else(bad)?;
    let address = u64_at(&d, rec + 16).ok_or_else(bad)?;
    let params = u32_at(&d, rec + 24).unwrap_or(0);
    let access = (code == 0xC000_0005 && params >= 2).then(|| {
        let kind = match u64_at(&d, rec + 32).unwrap_or(0) {
            0 => "read",
            1 => "write",
            _ => "execute",
        };
        (kind, u64_at(&d, rec + 40).unwrap_or(0))
    });
    let context_rva = u32_at(&d, rec + 152 + 4).map(|v| v as usize);
    let rsp = context_rva.and_then(|c| u64_at(&d, c + 0x98));

    // The crashing thread's stack (stream 3), from its stack pointer up.
    let mut stack = Vec::new();
    if let (Some((at, _)), Some(rsp)) = (stream(3), rsp) {
        let n = u32_at(&d, at).unwrap_or(0) as usize;
        for i in 0..n {
            let t = at + 4 + i * 48;
            if u32_at(&d, t) != Some(thread) {
                continue;
            }
            let (Some(start), Some(size), Some(rva)) = (u64_at(&d, t + 24), u32_at(&d, t + 32), u32_at(&d, t + 36)) else { break };
            let from = rsp.saturating_sub(start) as usize;
            let mem = d.get(rva as usize..rva as usize + size as usize).unwrap_or(&[]);
            for word in mem.get(from.min(mem.len())..).unwrap_or(&[]).as_chunks::<8>().0.iter().take(4096) {
                let value = u64::from_le_bytes(*word);
                if let Some((name, _)) = module_of(value)
                    && !stack.contains(&name)
                {
                    stack.push(name);
                }
            }
        }
    }
    Ok(CrashReport { dump: path.to_path_buf(), time, code, address, module: module_of(address), access, stack, modules, context: Vec::new(), same_place: 0, stores: Vec::new(), last_archive: None, archives: None })
}

/// A MINIDUMP_STRING: length in bytes, then UTF-16.
fn minidump_string(d: &[u8], at: usize) -> Option<String> {
    let len = u32_at(d, at)? as usize;
    let units: Vec<u16> = d.get(at + 4..at + 4 + len)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    Some(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minidump with two modules, a crash at `address` and a stack holding `stack`.
    fn dump(address: u64, stack: &[u64]) -> Vec<u8> {
        let mut d = vec![0u8; 0x400];
        d[..4].copy_from_slice(b"MDMP");
        let put32 = |d: &mut Vec<u8>, at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let put64 = |d: &mut Vec<u8>, at: usize, v: u64| d[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put32(&mut d, 8, 3);
        put32(&mut d, 12, 0x20);
        // Directory: modules at 0x100, exception at 0x200, threads at 0x300.
        for (i, (kind, rva)) in [(4u32, 0x100u32), (6, 0x200), (3, 0x300)].into_iter().enumerate() {
            put32(&mut d, 0x20 + i * 12, kind);
            put32(&mut d, 0x20 + i * 12 + 8, rva);
        }
        d.resize(0x1000, 0);
        put32(&mut d, 0x100, 2);
        for (i, (base, name_at)) in [(0x1_4000_0000u64, 0x800usize), (0x7000_0000, 0x880)].into_iter().enumerate() {
            let m = 0x104 + i * 108;
            put64(&mut d, m, base);
            put32(&mut d, m + 8, 0x10_0000);
            put32(&mut d, m + 20, name_at as u32);
        }
        for (at, name) in [(0x800usize, "C:\\Game\\GTA5_Enhanced.exe"), (0x880, "C:\\Game\\Trainer.asi")] {
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            put32(&mut d, at, units.len() as u32);
            d[at + 4..at + 4 + units.len()].copy_from_slice(&units);
        }
        // Exception: thread 7, access violation reading 0x10 at `address`; context at 0x900.
        put32(&mut d, 0x200, 7);
        put32(&mut d, 0x208, 0xC000_0005);
        put64(&mut d, 0x208 + 16, address);
        put32(&mut d, 0x208 + 24, 2);
        put64(&mut d, 0x208 + 32, 0);
        put64(&mut d, 0x208 + 40, 0x10);
        put32(&mut d, 0x208 + 156, 0x900);
        put64(&mut d, 0x900 + 0x98, 0x5000);
        // Thread 7's stack: 0x5000.., stored at 0xA00.
        put32(&mut d, 0x300, 1);
        put32(&mut d, 0x304, 7);
        put64(&mut d, 0x304 + 24, 0x5000);
        put32(&mut d, 0x304 + 32, (stack.len() * 8) as u32);
        put32(&mut d, 0x304 + 36, 0xA00);
        for (i, v) in stack.iter().enumerate() {
            put64(&mut d, 0xA00 + i * 8, *v);
        }
        d
    }

    #[test]
    fn reads_a_dump() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.dmp");
        std::fs::write(&path, dump(0x1_4000_1234, &[0x42, 0x7000_0500, 0x1_4000_9000, 0x7000_0600])).unwrap();
        let r = read_dump(&path).unwrap();
        assert_eq!(r.code_name(), "access violation");
        assert_eq!(r.module, Some(("GTA5_Enhanced.exe".into(), 0x1234)));
        assert_eq!(r.access, Some(("read", 0x10)));
        assert_eq!(r.stack, ["Trainer.asi", "GTA5_Enhanced.exe"]);
        assert_eq!(r.plugins_involved(), ["Trainer.asi"]);
        assert!(r.verdict("GTA5_Enhanced.exe").starts_with("Trainer.asi is on the crashing thread's stack"), "{}", r.verdict("GTA5_Enhanced.exe"));

        std::fs::write(&path, dump(0x7000_0010, &[])).unwrap();
        let r = read_dump(&path).unwrap();
        assert!(r.verdict("GTA5_Enhanced.exe").starts_with("The crash happened inside Trainer.asi"));
        std::fs::write(&path, dump(0x1_4000_0010, &[0x1_4000_0020])).unwrap();
        assert!(read_dump(&path).unwrap().verdict("GTA5_Enhanced.exe").contains("game's own code"));
        std::fs::write(&path, b"not a dump").unwrap();
        assert!(read_dump(&path).is_err());
    }

    const LOG: &str = "###\nGame State : Game Running\n============ ASSET STORES INFO ============\nStore, Size, Used\n\
        CutSceneStore, 1400, 784ScaleformStore, 1000, 798MetaDataStore, 3200, 3200DrawableStore, 81000, 65571fwStaticBoundsStore, 16000, 9527fwClipDictionaryStore, 21200, 20721FragmentStore, 42000, 41634fwTextStore, 1, 1\n\
        ============ PACK INFO ============\n0xA35819AB : scaleform_frontend.rpf\n0x7341E085 : occlusion.rpf\nTotal : 4030 / 4265\n\
        ============ STREAMING INFO ============\n";

    #[test]
    fn stores_and_archives_of_the_log() {
        let mut r = read_dump_for_tests();
        r.read_log(LOG);
        let names: Vec<_> = r.stores.iter().map(|s| (s.name.as_str(), s.size, s.used)).collect();
        assert_eq!(
            names,
            [
                ("CutSceneStore", 1400, 784),
                ("ScaleformStore", 1000, 798),
                ("MetaDataStore", 3200, 3200),
                ("DrawableStore", 81000, 65571),
                ("fwStaticBoundsStore", 16000, 9527),
                ("fwClipDictionaryStore", 21200, 20721),
                ("FragmentStore", 42000, 41634),
                ("fwTextStore", 1, 1)
            ]
        );
        // The full one, and the one within 2% of it (not the clip dictionaries, at 97.7%, nor the one-slot text store).
        assert_eq!(r.tight_stores().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["MetaDataStore", "FragmentStore"]);
        assert_eq!(r.last_archive, Some(LastArchive { hash: 0x7341E085, name: "occlusion.rpf".into(), pack: None }));
        assert_eq!(r.archives, Some((4030, 4265)));
        let verdict = r.verdict("GTA5_Enhanced.exe");
        assert!(verdict.contains("MetaDataStore (3200 of 3200)") && verdict.contains("Raise MetaDataStore in gameconfig.xml"), "{verdict}");
        assert!(verdict.contains("loading occlusion.rpf when"), "{verdict}");

        // The archive limit, when no store is full.
        r.stores.clear();
        r.archives = Some((4265, 4265));
        assert!(r.verdict("GTA5_Enhanced.exe").contains("limit of 4265 archives"));
        assert_eq!(parse_stores("no stores here"), []);
    }

    fn read_dump_for_tests() -> CrashReport {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.dmp");
        std::fs::write(&path, dump(0x1_4000_1234, &[])).unwrap();
        read_dump(&path).unwrap()
    }

    #[test]
    fn names_the_pack_of_the_last_archive() {
        use paleto_rpf::ArchiveEditor;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        let pack = root.join("mods/update/x64/dlcpacks/MyMap");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let mut ed = ArchiveEditor::new("dlc.rpf");
        ed.add_file("setup2.xml", b"<SSetupData><deviceName>dlc_MyMap_Gen9</deviceName></SSetupData>".to_vec()).unwrap();
        ed.archive("x64/audio/occlusion.rpf", true, None).unwrap().add_file("1.ymt", vec![1]).unwrap();
        ed.save(&pack.join("dlc.rpf")).unwrap();
        let game = Game::open(&root).unwrap();

        let hash = paleto_formats::hash::joaat_lower("dlc_mymap_gen9:/x64/audio/occlusion.rpf");
        assert_eq!(pack_of_archive(&game, None, hash), Some("MyMap".to_string()));
        assert_eq!(pack_of_archive(&game, None, hash ^ 1), None);
        let last = LastArchive { hash, name: "occlusion.rpf".into(), pack: Some("MyMap".into()) };
        assert_eq!(last.describe(), "occlusion.rpf of the pack MyMap");
    }

    #[test]
    fn context_of_the_crash() {
        let lines = context_lines("###\nGame State : Game Running\nVersion : 1158.16\nPlayer Position (1;2;3)\nPeds: 4\n");
        assert_eq!(lines, ["Game State : Game Running", "Version : 1158.16", "Player Position (1;2;3)"]);
    }
}
