//! `paleto new` and the editing commands in a game folder: confined, never over the originals.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Output};

fn paleto(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or settings of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").current_dir(dir).args(args);
    cmd.output().unwrap()
}

/// A game folder with an original archive `x64/a.rpf`, a copy of an empty archive made by `paleto new`.
fn game(dir: &Path) -> std::path::PathBuf {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("x64")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let made = paleto(dir, &["new", &root.join("x64/a.rpf").to_string_lossy()]);
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    // The new archive of a game's data went to the mods folder; the original is a copy of it.
    std::fs::copy(root.join("mods/x64/a.rpf"), root.join("x64/a.rpf")).unwrap();
    std::fs::remove_dir_all(root.join("mods")).unwrap();
    root
}

fn original(root: &Path) -> (u64, u64, Vec<u8>) {
    let m = std::fs::metadata(root.join("x64/a.rpf")).unwrap();
    (m.dev(), m.ino(), std::fs::read(root.join("x64/a.rpf")).unwrap())
}

#[test]
fn a_new_archive_in_a_game_folder_is_made_in_the_mods_folder_and_never_over_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    let before = original(&root);
    // Game data goes to the mods folder, through the engine: the originals are not touched.
    let out = paleto(dir.path(), &["new", &root.join("x64/b.rpf").to_string_lossy()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join("mods/x64/b.rpf").is_file() && !root.join("x64/b.rpf").exists());
    assert!(String::from_utf8_lossy(&out.stdout).contains("mods/x64/b.rpf"));
    // An existing file is never replaced, whichever name it is reached by.
    for path in [root.join("x64/a.rpf"), root.join("mods/x64/b.rpf")] {
        let again = paleto(dir.path(), &["new", &path.to_string_lossy()]);
        assert!(!again.status.success(), "{path:?}");
    }
    assert_eq!(original(&root), before);
    // A loose archive outside every game folder is made where it is.
    let loose = paleto(dir.path(), &["new", &dir.path().join("loose.rpf").to_string_lossy()]);
    assert!(loose.status.success() && dir.path().join("loose.rpf").is_file());
}

#[test]
fn nothing_is_made_through_a_link_from_mods_back_to_the_originals() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::os::unix::fs::symlink("../x64", root.join("mods/x64")).unwrap();
    let before = original(&root);
    let made = paleto(dir.path(), &["new", &root.join("mods/x64/c.rpf").to_string_lossy()]);
    assert!(!made.status.success() && !root.join("x64/c.rpf").exists());
    assert!(String::from_utf8_lossy(&made.stderr).contains("leads outside"), "{}", String::from_utf8_lossy(&made.stderr));
    assert_eq!(original(&root), before);
}

#[test]
fn new_follows_the_game_that_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("GTA5.exe"), b"").unwrap();
    // A path of one game while another is named is refused, as for the editing commands.
    let target = root.join("x64/n.rpf").to_string_lossy().into_owned();
    let out = paleto(dir.path(), &["--game", &other.to_string_lossy(), "new", &target]);
    assert!(!out.status.success() && !root.join("mods/x64/n.rpf").exists(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = paleto(dir.path(), &["--game", &root.to_string_lossy(), "new", &target]);
    assert!(out.status.success() && root.join("mods/x64/n.rpf").is_file(), "{}", String::from_utf8_lossy(&out.stderr));
}
