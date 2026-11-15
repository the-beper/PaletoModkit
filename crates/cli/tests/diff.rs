//! `paleto diff`: a mods copy compared with the game's original, read-only.

use std::path::Path;
use std::process::{Command, Output};

use paleto_rpf::ArchiveEditor;

fn paleto(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or settings of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").env_remove("PALETO_GAME").current_dir(dir).args(args);
    cmd.output().unwrap()
}

#[test]
fn a_mods_copy_against_its_original() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut original = ArchiveEditor::new("update.rpf");
    original.add_file("common/data/a.txt", b"one\ntwo\nthree\n".to_vec()).unwrap();
    original.save(&root.join("update/update.rpf")).unwrap();
    let mut copy = ArchiveEditor::new("update.rpf");
    copy.add_file("common/data/a.txt", b"one\n2\nthree\n".to_vec()).unwrap();
    copy.add_file("common/data/new.txt", b"new".to_vec()).unwrap();
    copy.save(&root.join("mods/update/update.rpf")).unwrap();
    let before = std::fs::read(root.join("update/update.rpf")).unwrap();
    let text = |out: &Output| String::from_utf8_lossy(&out.stdout).into_owned();

    let out = paleto(dir.path(), &["diff", "game/mods/update/update.rpf/common/data/a.txt"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shown = text(&out);
    for line in ["--- original: ", "/game/update/update.rpf/common/data/a.txt\n", "@@ -1,3 +1,3 @@\n", " one\n-two\n+2\n three\n"] {
        assert!(shown.contains(line), "{line:?} in {shown}");
    }
    let added = text(&paleto(dir.path(), &["diff", "game/mods/update/update.rpf/common/data/new.txt"]));
    assert!(added.contains("added: the game has no original"), "{added}");

    // The original itself is not a mods copy; given another file, any two compare.
    let out = paleto(dir.path(), &["diff", "game/update/update.rpf/common/data/a.txt"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("is not in the mods folder"));
    std::fs::write(dir.path().join("x.ini"), b"a=1\r\n").unwrap();
    std::fs::write(dir.path().join("y.ini"), b"a=1\n").unwrap();
    let same = text(&paleto(dir.path(), &["diff", "y.ini", "x.ini"]));
    assert!(same.contains("note: Only the line endings, the final newline or a byte order mark differ") && same.contains("the same text"), "{same}");
    assert_eq!(std::fs::read(root.join("update/update.rpf")).unwrap(), before, "read-only");
}
