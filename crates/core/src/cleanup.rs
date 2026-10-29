//! Copies in the mods folder identical to the game's own files: left by
//! tools that copy a whole archive to change nothing, or a change undone.
//! They do nothing but slow the game's loading (OpenRPF redirects them) and
//! hide the real changes, so they can be deleted: the game then reads its own.

use crate::fsroot::Existing;
use crate::game::{same_content, same_file};
use crate::relpath::RelPath;
use crate::{Error, Game};

/// A file of the mods folder identical to the game's: its path relative to
/// the mods folder ('/' separators) and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub path: String,
    pub size: u64,
}

/// The mods folder's files identical to the game's. Reads the copies whole
/// (tens of GB take tens of seconds; sizes are compared first).
pub fn identical_copies(game: &Game) -> Vec<Copy> {
    let mods = game.mods_dir();
    if !mods.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (file, _) in crate::vfs::local_files(&mods).unwrap_or_default() {
        let rel = file.strip_prefix(&mods).unwrap().to_string_lossy().replace('\\', "/");
        let original = game.original(&rel);
        if same_file(&file, &original) {
            out.push(Copy { size: std::fs::metadata(&file).map_or(0, |m| m.len()), path: rel });
        }
    }
    out
}

/// Deletes these files of the mods folder (after checking each is still
/// identical to the game's, and that the name still holds the file that was
/// compared), then the folders left empty.
pub fn delete(game: &Game, copies: &[Copy]) -> Result<usize, Error> {
    let roots = game.roots()?;
    let _lock = crate::txn::guard(&roots, "deleting identical copies from the mods folder")?;
    let mut deleted = 0;
    for c in copies {
        let dest = roots.mods_dest(&RelPath::parse(&c.path)?)?;
        let (Some(Existing::File(_)), Some(original)) = (&dest.existing, roots.original(&RelPath::parse(&c.path)?)?) else { continue };
        if same_content(dest.parent.open_read(&dest.name)?, original) {
            roots.remove(&dest)?;
            deleted += 1;
            let _ = roots.prune_empty(&dest);
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_only_exact_copies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        for (rel, original, copy) in [
            ("update/x64/dlcpacks/mpbeach/dlc.rpf", "beach", Some("beach")),
            ("update/x64/dlcpacks/mpheist/dlc.rpf", "heist", Some("HEIST")),
            ("x64e.rpf", "x64e", Some("x64e")),
            ("update/update.rpf", "update", Some("update, changed")),
        ] {
            std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            std::fs::write(root.join(rel), original).unwrap();
            if let Some(copy) = copy {
                std::fs::create_dir_all(root.join("mods").join(rel).parent().unwrap()).unwrap();
                std::fs::write(root.join("mods").join(rel), copy).unwrap();
            }
        }
        // An add-on pack has no original.
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::write(root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), b"car").unwrap();
        let game = Game::open(root).unwrap();

        let copies = identical_copies(&game);
        let paths: Vec<&str> = copies.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["update/x64/dlcpacks/mpbeach/dlc.rpf", "x64e.rpf"]);
        assert_eq!(copies[0].size, 5);
        assert_eq!(delete(&game, &copies).unwrap(), 2);
        assert!(!root.join("mods/update/x64/dlcpacks/mpbeach").exists(), "empty folder left");
        assert!(root.join("mods/update/x64/dlcpacks/mpheist/dlc.rpf").exists());
        assert!(root.join("mods/update/x64/dlcpacks/car/dlc.rpf").exists());
        assert!(root.join("mods/update/update.rpf").exists());
        assert!(identical_copies(&game).is_empty());
    }
}
