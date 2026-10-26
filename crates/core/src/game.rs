//! The game installation, its mods folder, and the encryption keys.

use std::path::{Path, PathBuf};

use paleto_rpf::Keys;
use sha2::{Digest, Sha256};

use crate::Error;

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

impl Game {
    /// Recognises a game folder from its executable.
    pub fn open(root: &Path) -> Result<Self, Error> {
        for edition in [Edition::Enhanced, Edition::Legacy] {
            if root.join(edition.exe_name()).is_file() {
                return Ok(Game { root: root.to_path_buf(), edition });
            }
        }
        Err(Error::NotAGameFolder(root.to_path_buf()))
    }

    pub fn mods_dir(&self) -> PathBuf {
        self.root.join("mods")
    }

    /// Where a game-relative path lives: data files the game streams (archives,
    /// `update/`, `x64/`) go to the mods folder, everything else (ASI plugins,
    /// scripts, configs) to the game folder.
    pub fn target(&self, rel: &str) -> PathBuf {
        let base = if is_game_data(rel) { self.mods_dir() } else { self.root.clone() };
        resolve_ci(&base, rel)
    }

    /// The original, unmodded file for a game-relative path.
    pub fn original(&self, rel: &str) -> PathBuf {
        resolve_ci(&self.root, rel)
    }

    /// The copy of a game data file inside the mods folder, created from the
    /// original when missing. Returns `None` if neither exists. Copies are
    /// edited in place, so a copy that is a link (to the original or to
    /// anything else) is first made a file of its own.
    pub fn mods_copy(&self, rel: &str) -> Result<Option<PathBuf>, Error> {
        let dest = resolve_ci(&self.mods_dir(), rel);
        let src = self.original(rel);
        if dest.is_file() {
            unlink_copy(&dest, &src)?;
            return Ok(Some(dest));
        }
        if !src.is_file() {
            return Ok(None);
        }
        std::fs::create_dir_all(dest.parent().unwrap())?;
        // On btrfs/XFS this is a reflink: instant and takes no extra space.
        replace_file(&dest, |tmp| std::fs::copy(&src, tmp).map(drop))?;
        Ok(Some(dest))
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

/// Replaces `dest` with a copy of its content if it is a symbolic link or
/// shares its data with another name (a hard link, or the original `src`).
/// Refuses when `dest` is the original itself, reached through a linked folder.
fn unlink_copy(dest: &Path, src: &Path) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    let link = std::fs::symlink_metadata(dest)?;
    let target = std::fs::metadata(dest)?;
    let same_as_src = std::fs::metadata(src).is_ok_and(|m| (m.dev(), m.ino()) == (target.dev(), target.ino()));
    if !link.file_type().is_symlink() && link.nlink() == 1 && !same_as_src {
        return Ok(());
    }
    let parent = |p: &Path| p.parent().and_then(|p| std::fs::canonicalize(p).ok());
    if parent(dest).is_some() && parent(dest) == parent(src) {
        return Err(Error::Io(std::io::Error::other(format!("{} is the original game file (the mods folder links to the game folder)", dest.display()))));
    }
    // The copy follows the link; the rename replaces the link, not what it points to.
    Ok(replace_file(dest, |tmp| std::fs::copy(dest, tmp).map(drop))?)
}

/// Writes `dest` through `write` into a temporary name next to it, then
/// renames that over `dest`: a link at `dest` is replaced, never written through.
pub fn replace_file(dest: &Path, write: impl FnOnce(&Path) -> std::io::Result<()>) -> std::io::Result<()> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dest.with_file_name(format!(".{name}.paleto-tmp"));
    let _ = std::fs::remove_file(&tmp);
    let result = write(&tmp).and_then(|()| std::fs::rename(&tmp, dest));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = match fa.read(&mut ba) {
            Ok(0) => return true,
            Ok(n) => n,
            Err(_) => return false,
        };
        if fb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
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

fn is_game_data(rel: &str) -> bool {
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
        assert_eq!(std::fs::read_dir(root.join("mods/x64")).unwrap().count(), 4, "no temporary file left");

        // Files written whole (packages, add-on packs) replace a link too.
        std::os::unix::fs::symlink(root.join("x64/a.rpf"), root.join("mods/x64/e.rpf")).unwrap();
        replace_file(&root.join("mods/x64/e.rpf"), |tmp| std::fs::write(tmp, b"new")).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64/e.rpf")).unwrap(), b"new");
        assert_eq!(std::fs::read(root.join("x64/a.rpf")).unwrap(), b"original");

        // A mods folder that leads back into the game folder is refused.
        std::fs::remove_dir_all(root.join("mods/x64")).unwrap();
        std::os::unix::fs::symlink(root.join("x64"), root.join("mods/x64")).unwrap();
        assert!(game.mods_copy("x64/a.rpf").is_err());
        assert_eq!(std::fs::read(root.join("x64/a.rpf")).unwrap(), b"original");
    }
}
