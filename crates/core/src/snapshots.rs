//! Snapshots of the mods folder: whole copies kept in the game folder's
//! `mods_snapshots/<time>/`, to go back to a state that worked.
//!
//! Copies use `std::fs::copy`, which on btrfs, XFS and bcachefs shares the
//! data with the original (a reflink): instant and taking no space until a
//! file changes. On other filesystems they are full copies.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, Game};

/// The file inside a snapshot that holds its label.
const LABEL: &str = "snapshot.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Its folder name: the time it was taken, in seconds since 1970.
    pub id: String,
    pub label: String,
    pub dir: PathBuf,
}

impl Snapshot {
    pub fn time(&self) -> Option<SystemTime> {
        self.id.parse().ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
    }

    /// Its copy of the mods folder.
    fn mods(&self) -> PathBuf {
        self.dir.join("mods")
    }
}

pub fn snapshots_dir(game: &Game) -> PathBuf {
    game.root.join("mods_snapshots")
}

/// The snapshots of this game, newest first.
pub fn list(game: &Game) -> Vec<Snapshot> {
    let mut out: Vec<(u64, Snapshot)> = std::fs::read_dir(snapshots_dir(game))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let secs = id.parse::<u64>().ok()?;
            let dir = e.path();
            dir.join("mods").is_dir().then_some(())?;
            let label = std::fs::read_to_string(dir.join(LABEL)).unwrap_or_default().trim().to_string();
            Some((secs, Snapshot { id, label, dir }))
        })
        .collect();
    out.sort_by_key(|(secs, _)| std::cmp::Reverse(*secs));
    out.into_iter().map(|(_, s)| s).collect()
}

/// Takes a snapshot of the mods folder.
pub fn create(game: &Game, label: &str) -> Result<Snapshot, Error> {
    let mods = game.mods_dir();
    put_back(game)?;
    if !mods.is_dir() {
        return Err(Error::Package("there is no mods folder to save".into()));
    }
    let mut secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // Two snapshots in the same second get the next free second.
    while snapshots_dir(game).join(secs.to_string()).exists() {
        secs += 1;
    }
    let dir = snapshots_dir(game).join(secs.to_string());
    let partial = snapshots_dir(game).join(format!(".{secs}.partial"));
    let _ = std::fs::remove_dir_all(&partial);
    let result = copy_tree(&mods, &partial.join("mods")).and_then(|_| {
        std::fs::write(partial.join(LABEL), label.trim())?;
        std::fs::rename(&partial, &dir)?;
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    result?;
    Ok(Snapshot { id: secs.to_string(), label: label.trim().to_string(), dir })
}

/// Puts a snapshot's copy in place of the mods folder. The current mods
/// folder is replaced only once the copy is complete.
pub fn restore(game: &Game, id: &str) -> Result<(), Error> {
    let snapshot = list(game).into_iter().find(|s| s.id == id).ok_or_else(|| Error::Package(format!("no snapshot {id}")))?;
    let mods = game.mods_dir();
    put_back(game)?;
    let incoming = game.root.join(".mods.restoring");
    let outgoing = game.root.join(".mods.replaced");
    for dir in [&incoming, &outgoing] {
        let _ = std::fs::remove_dir_all(dir);
    }
    if let Err(e) = copy_tree(&snapshot.mods(), &incoming) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(e);
    }
    if mods.exists() {
        std::fs::rename(&mods, &outgoing)?;
    }
    if let Err(e) = std::fs::rename(&incoming, &mods) {
        // Put the old folder back rather than leave none.
        let _ = std::fs::rename(&outgoing, &mods);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&outgoing);
    Ok(())
}

/// A restore that failed between its two renames left the mods folder only
/// in `.mods.replaced`: puts it back rather than delete it with the leftovers.
fn put_back(game: &Game) -> Result<(), Error> {
    let outgoing = game.root.join(".mods.replaced");
    if !game.mods_dir().exists() && outgoing.is_dir() {
        std::fs::rename(&outgoing, game.mods_dir())?;
    }
    Ok(())
}

pub fn delete(game: &Game, id: &str) -> Result<(), Error> {
    let snapshot = list(game).into_iter().find(|s| s.id == id).ok_or_else(|| Error::Package(format!("no snapshot {id}")))?;
    std::fs::remove_dir_all(snapshot.dir)?;
    Ok(())
}

/// Bytes in a folder (what a full copy would take).
pub fn size(dir: &Path) -> u64 {
    crate::vfs::local_files(dir).unwrap_or_default().iter().filter_map(|(p, _)| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// Whether copies in this folder share their data (btrfs, XFS, bcachefs),
/// from the mount table: the filesystem of the longest mount point holding it.
pub fn shares_copies(dir: &Path) -> bool {
    let Ok(dir) = std::fs::canonicalize(dir) else { return false };
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, point, fs) = (f.next()?, f.next()?, f.next()?);
            // Spaces in mount points are written as \040.
            let point = point.replace("\\040", " ");
            dir.starts_with(&point).then(|| (point.len(), fs.to_string()))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, fs)| matches!(fs.as_str(), "btrfs" | "xfs" | "bcachefs"))
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"one").unwrap();
        std::fs::write(root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), b"car").unwrap();
        let game = Game::open(root).unwrap();

        let first = create(&game, " works ").unwrap();
        assert_eq!(first.label, "works");
        // A broken state: a file changed, one added, one removed.
        std::fs::write(root.join("mods/x64e.rpf"), b"two").unwrap();
        std::fs::write(root.join("mods/new.rpf"), b"new").unwrap();
        std::fs::remove_file(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap();
        let second = create(&game, "broken").unwrap();
        let ids: Vec<String> = list(&game).into_iter().map(|s| s.label).collect();
        assert_eq!(ids, ["broken", "works"]);
        assert!(size(&second.mods()) > 0);

        restore(&game, &first.id).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64e.rpf")).unwrap(), b"one");
        assert_eq!(std::fs::read(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap(), b"car");
        assert!(!root.join("mods/new.rpf").exists());
        // The snapshot stays as it was, and can be restored again.
        std::fs::write(root.join("mods/x64e.rpf"), b"three").unwrap();
        assert_eq!(std::fs::read(first.mods().join("x64e.rpf")).unwrap(), b"one");
        assert!(!root.join(".mods.restoring").exists() && !root.join(".mods.replaced").exists());

        delete(&game, &second.id).unwrap();
        assert_eq!(list(&game).len(), 1);
        assert!(restore(&game, &second.id).is_err());
    }

    #[test]
    fn a_failed_restore_loses_no_mods() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"old").unwrap();
        let game = Game::open(root).unwrap();
        let first = create(&game, "old").unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"current").unwrap();
        // A restore stopped between its two renames: the mods folder is only in `.mods.replaced`.
        std::fs::rename(root.join("mods"), root.join(".mods.replaced")).unwrap();

        // Saving first keeps it, and the restore does not delete it with the leftovers.
        let saved = create(&game, "before").unwrap();
        assert_eq!(std::fs::read(saved.mods().join("x64e.rpf")).unwrap(), b"current");
        restore(&game, &first.id).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64e.rpf")).unwrap(), b"old");
        assert!(!root.join(".mods.replaced").exists());
    }
}
