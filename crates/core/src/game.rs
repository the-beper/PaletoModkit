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
    /// original when missing. Returns `None` if neither exists.
    pub fn mods_copy(&self, rel: &str) -> Result<Option<PathBuf>, Error> {
        let dest = resolve_ci(&self.mods_dir(), rel);
        if dest.is_file() {
            return Ok(Some(dest));
        }
        let src = self.original(rel);
        if !src.is_file() {
            return Ok(None);
        }
        std::fs::create_dir_all(dest.parent().unwrap())?;
        // On btrfs/XFS this is a reflink: instant and takes no extra space.
        std::fs::copy(&src, &dest)?;
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
