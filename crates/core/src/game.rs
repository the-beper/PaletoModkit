//! The game installation, its mods folder, and the encryption keys.

use std::path::{Path, PathBuf};

use paleto_rpf::Keys;
use sha2::{Digest, Sha256};

use crate::Error;
use crate::fsroot::{Dir, GameRoots};
use crate::relpath::RelPath;

/// CodeWalker's magic.dat, pinned. It has not changed since 2018.
const MAGIC_URL: &str =
    "https://raw.githubusercontent.com/dexyfex/CodeWalker/485d56bec00262ed7fa472261cce7bbc6202b96e/CodeWalker.Core/Resources/magic.dat";
const MAGIC_SHA256: &str = "dc35981f822e892ced3aa81d31e7a96927d573ee28f67417592b5afeaf330832";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Legacy,
    Enhanced,
}

impl Edition {
    pub fn exe_name(self) -> &'static str {
        match self {
            Edition::Legacy => "GTA5.exe",
            Edition::Enhanced => "GTA5_Enhanced.exe",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Game {
    pub root: PathBuf,
    pub edition: Edition,
}

static KNOWN: std::sync::Mutex<Vec<Game>> = std::sync::Mutex::new(Vec::new());

fn remember(game: &Game) {
    let mut known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !known.iter().any(|g| g.root == game.root) {
        known.push(game.clone());
    }
}

impl Game {
    /// Recognises a game folder from its executable.
    pub fn open(root: &Path) -> Result<Self, Error> {
        for edition in [Edition::Enhanced, Edition::Legacy] {
            if root.join(edition.exe_name()).is_file() {
                let game = Game { root: root.to_path_buf(), edition };
                remember(&game);
                return Ok(game);
            }
        }
        Err(Error::NotAGameFolder(root.to_path_buf()))
    }

    /// The game folders this program has opened so far: a path on the drive a link of one
    /// of them (`mods`, say) leads to belongs to that game too, though it is not below it.
    pub fn known() -> Vec<Game> {
        KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.root.join("mods")
    }

    /// The game folder and its mods folder, open for confined writes (see [`crate::fsroot`]).
    pub fn roots(&self) -> Result<GameRoots, Error> {
        GameRoots::open(&self.root)
    }

    /// Whether a game-relative path is data the game streams (archives, `update/`,
    /// `x64/`): such files are changed in the mods folder, everything else
    /// (ASI plugins, scripts, configs) in the game folder.
    pub fn routes_to_mods(&self, rel: &str) -> bool {
        is_game_data(rel)
    }

    /// The original, unmodded file for a game-relative path (read-only lookup).
    pub fn original(&self, rel: &str) -> PathBuf {
        resolve_ci(&self.root, rel)
    }

    /// The copy of a game data file inside the mods folder, created from the
    /// original when missing. Returns `None` if neither exists. Copies are
    /// edited in place, so a copy that is a link (to the original or to
    /// anything else) is first made a file of its own. Refused if the copy
    /// would land anywhere but the mods folder (a link that leads out of it).
    pub fn mods_copy(&self, rel: &str) -> Result<Option<PathBuf>, Error> {
        self.roots()?.mods_copy(&RelPath::parse(rel)?)
    }

    /// Replaces a file of the game folder or its mods folder (a mods copy made by [`Game::mods_copy`], or
    /// a loose file) with `data`: one journaled replacement under the game lock, so the file is the old
    /// one or the new one, and [`Error::Busy`] while another change to this game runs. Programs, the
    /// game's own `.dll` and `.txt` files and its data are refused; a link at the name is replaced, never written through.
    pub fn replace_game_file(&self, rel: &str, data: &[u8]) -> Result<(), Error> {
        use std::io::Write;
        crate::txn::replace_one(&self.roots()?, &RelPath::parse(rel)?, "saving a file in the mods folder", |f| f.write_all(data))
    }

    /// Keys for this game, extracted on first use and then cached.
    pub fn keys(&self) -> Result<Keys, Error> {
        let dir = data_dir();
        let keys_path = dir.join("keys.bin");
        // Keys cached before the AWC key was kept are derived again.
        if let Ok(data) = std::fs::read(&keys_path)
            && let Ok(keys) = Keys::from_bytes(&data)
            && keys.awc.is_some()
        {
            return Ok(keys);
        }
        let magic = magic_dat()?;
        let exe = std::fs::read(self.root.join(self.edition.exe_name()))?;
        let keys = Keys::from_exe_and_magic(&exe, &magic)?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&keys_path, keys.to_bytes())?;
        Ok(keys)
    }
}

/// Writes `dest` through `write` into a new file of an unpredictable name next
/// to it, then renames that over `dest`: a link at `dest` is replaced, never
/// written through. For files outside a game folder; inside one, use
/// [`GameRoots::replace`].
pub fn replace_file(dest: &Path, write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>) -> std::io::Result<()> {
    let name = dest.file_name().and_then(|n| n.to_str()).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let parent = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    crate::fsroot::replace_file(&Dir::open_path(parent)?, name, write)
}

/// GTA V installations in the Steam libraries of this user.
pub fn detect_installs() -> Vec<Game> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut libraries = Vec::new();
    for steam in [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/data/Steam"] {
        let steam = home.join(steam);
        libraries.push(steam.clone());
        // libraryfolders.vdf lists the other libraries as "path" "<dir>".
        if let Ok(vdf) = std::fs::read_to_string(steam.join("steamapps/libraryfolders.vdf")) {
            for line in vdf.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "path" {
                    libraries.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
                }
            }
        }
    }
    let mut found: Vec<Game> = Vec::new();
    for lib in libraries {
        for name in ["Grand Theft Auto V", "Grand Theft Auto V Enhanced"] {
            if let Ok(game) = Game::open(&lib.join("steamapps/common").join(name)) {
                let canon = std::fs::canonicalize(&game.root).ok();
                if !found.iter().any(|g| std::fs::canonicalize(&g.root).ok() == canon) {
                    found.push(game);
                }
            }
        }
    }
    found
}

/// Whether two files hold the same bytes (read whole, unless sizes differ).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::File::open(a), std::fs::File::open(b)) {
        (Ok(a), Ok(b)) => same_content(a, b),
        _ => false,
    }
}

/// [`same_file`] for files already open.
pub fn same_content(mut a: std::fs::File, mut b: std::fs::File) -> bool {
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (a.metadata(), b.metadata()) else { return false };
    if ma.len() != mb.len() {
        return false;
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = match a.read(&mut ba) {
            Ok(0) => return true,
            Ok(n) => n,
            Err(_) => return false,
        };
        if b.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

/// `$XDG_DATA_HOME/paleto`.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("paleto")
}

/// The cached magic.dat, downloaded on first use.
fn magic_dat() -> Result<Vec<u8>, Error> {
    let path = data_dir().join("magic.dat");
    if let Ok(data) = std::fs::read(&path)
        && sha256_hex(&data) == MAGIC_SHA256 {
            return Ok(data);
        }
    let data = ureq::get(MAGIC_URL)
        .call()
        .and_then(|mut r| r.body_mut().read_to_vec())
        .map_err(|e| Error::Download(e.to_string()))?;
    if sha256_hex(&data) != MAGIC_SHA256 {
        return Err(Error::Download("magic.dat checksum mismatch".into()));
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, &data)?;
    Ok(data)
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn is_game_data(rel: &str) -> bool {
    let rel = rel.replace('\\', "/").to_ascii_lowercase();
    let first = rel.trim_start_matches('/').split('/').next().unwrap_or("");
    first == "update" || first == "x64" || rel.ends_with(".rpf")
}

/// Joins a Windows-style relative path onto `base`, matching existing path
/// components case-insensitively (game files are named inconsistently).
pub fn resolve_ci(base: &Path, rel: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".") {
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        let found = std::fs::read_dir(&path).ok().and_then(|entries| {
            entries.flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().eq_ignore_ascii_case(part))
        });
        path.push(found.unwrap_or_else(|| part.into()));
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy in the mods folder that links to another file must not let an
    /// edit of the copy reach that file.
    #[test]
    fn mods_copy_is_never_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/x64")).unwrap();
        std::fs::create_dir_all(root.join("x64")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        for name in ["a.rpf", "b.rpf", "c.rpf"] {
            std::fs::write(root.join("x64").join(name), b"original").unwrap();
        }
        let elsewhere = dir.path().join("modded.rpf");
        std::fs::write(&elsewhere, b"modded").unwrap();
        std::os::unix::fs::symlink(root.join("x64/a.rpf"), root.join("mods/x64/a.rpf")).unwrap();
        std::fs::hard_link(root.join("x64/b.rpf"), root.join("mods/x64/b.rpf")).unwrap();
        std::fs::hard_link(&elsewhere, root.join("mods/x64/c.rpf")).unwrap();
        let dangling = dir.path().join("missing.rpf");
        std::os::unix::fs::symlink(&dangling, root.join("mods/x64/d.rpf")).unwrap();
        std::fs::write(root.join("x64/d.rpf"), b"original").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("mods/x64/.a.rpf.paleto-tmp")).unwrap();
        let game = Game::open(&root).unwrap();

        for (name, content) in [("a.rpf", "original"), ("b.rpf", "original"), ("c.rpf", "modded"), ("d.rpf", "original")] {
            let copy = game.mods_copy(&format!("x64/{name}")).unwrap().unwrap();
            assert!(!copy.is_symlink(), "{name}");
            assert_eq!(std::fs::read(&copy).unwrap(), content.as_bytes(), "{name} keeps its content");
            std::fs::write(&copy, b"edited").unwrap();
        }
        for name in ["a.rpf", "b.rpf", "c.rpf", "d.rpf"] {
            assert_eq!(std::fs::read(root.join("x64").join(name)).unwrap(), b"original", "{name}");
        }
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"modded");
        assert!(!dangling.exists());
        assert_eq!(std::fs::read_dir(root.join("mods/x64")).unwrap().count(), 5, "only the planted link beside the four copies: no temporary file left, none removed");
        assert!(std::fs::symlink_metadata(root.join("mods/x64/.a.rpf.paleto-tmp")).unwrap().is_symlink(), "a link at the old temporary name is not ours to touch");

        // Files written whole (packages, add-on packs) replace a link too.
        std::os::unix::fs::symlink(root.join("x64/a.rpf"), root.join("mods/x64/e.rpf")).unwrap();
        replace_file(&root.join("mods/x64/e.rpf"), |tmp| std::io::Write::write_all(tmp, b"new")).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64/e.rpf")).unwrap(), b"new");
        assert_eq!(std::fs::read(root.join("x64/a.rpf")).unwrap(), b"original");

        // A mods folder that leads back into the game folder is refused.
        std::fs::remove_dir_all(root.join("mods/x64")).unwrap();
        std::os::unix::fs::symlink(root.join("x64"), root.join("mods/x64")).unwrap();
        assert!(game.mods_copy("x64/a.rpf").is_err());
        assert_eq!(std::fs::read(root.join("x64/a.rpf")).unwrap(), b"original");
    }
}
