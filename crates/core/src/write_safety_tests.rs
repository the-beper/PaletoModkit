//! Reproductions of write-path findings (OIV-01/02/03/06, FS-04, DLC-05):
//! tests marked `#[ignore]` fail on the current code and say why.

use std::os::unix::fs::symlink;
use std::path::Path;

use paleto_rpf::ArchiveEditor;

use crate::fixtures::*;
use crate::game::{Edition, Game};
use crate::{Error, Package};

fn install(game: &Game, oiv: &Path) -> Result<Vec<String>, Error> {
    Package::open(oiv)?.install_with(game, None)
}

fn list_of(path: &Path) -> String {
    String::from_utf8(ArchiveEditor::open(path, None).unwrap().read("common/data/dlclist.xml", None).unwrap()).unwrap()
}

// ---- OIV-02: path confinement ----

#[test]
fn oiv02_add_through_linked_mods_x64_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../x64", game.root.join("mods/x64")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">x64\new.rpf</add>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root);
    assert!(install(&game, &oiv).is_err(), "a file was added to the original x64 folder");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv02_archive_edit_through_linked_mods_x64_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../x64", game.root.join("mods/x64")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root);
    assert!(install(&game, &oiv).is_err());
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv02_new_archive_through_linked_mods_x64_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../x64", game.root.join("mods/x64")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<archive path="x64\b.rpf" createIfNotExist="True"><add source="x">b.txt</add></archive>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root);
    assert!(install(&game, &oiv).is_err(), "an archive was created in the original x64 folder");
    assert_unchanged(&before, &snapshot(&game.root));
}

#[test]
fn oiv02_add_through_linked_scripts_outside_the_game_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    std::fs::remove_dir_all(game.root.join("scripts")).unwrap();
    symlink(&outside, game.root.join("scripts")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\evil.asi</add>"#, &[("content/x", b"evil")], false);
    let (before, before_out) = (snapshot(&game.root), snapshot(&outside));
    assert!(install(&game, &oiv).is_err(), "a file was written outside the game folder");
    assert_unchanged(&before_out, &snapshot(&outside));
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&outside).is_empty());
}

#[test]
fn oiv02_delete_through_linked_mods_update_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../update", game.root.join("mods/update")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<delete>update\update.rpf</delete>"#, &[], false);
    let before = snapshot(&game.root);
    assert!(install(&game, &oiv).is_err(), "the original update.rpf was deleted");
    assert_unchanged(&before, &snapshot(&game.root));
}

#[test]
fn oiv02_text_edit_through_linked_mods_update_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::write(game.root.join("update/list.txt"), b"a\n").unwrap();
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../update", game.root.join("mods/update")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<text path="update\list.txt"><add>b</add></text>"#, &[], false);
    let before = snapshot(&game.root);
    assert!(install(&game, &oiv).is_err());
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv02_mods_linked_to_the_game_root_refuses_game_data() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    symlink(".", game.root.join("mods")).unwrap();
    for commands in [r#"<add source="x">x64\new.rpf</add>"#, r#"<add source="x">update\new.txt</add>"#] {
        let oiv = oiv(dir.path(), "x.oiv", commands, &[("content/x", b"new")], false);
        let before = snapshot(&game.root);
        assert!(install(&game, &oiv).is_err(), "{commands}: wrote game data over the originals");
        assert_unchanged(&before, &snapshot(&game.root));
    }
}

#[test]
fn oiv02_install_pack_through_linked_mods_update_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    symlink("../update", game.root.join("mods/update")).unwrap();
    let before = snapshot(&game.root);
    assert!(crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).is_err());
    assert_unchanged(&before, &snapshot(&game.root));
}

#[test]
fn oiv02_valid_internal_link_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::rename(game.root.join("scripts"), game.root.join("scripts_real")).unwrap();
    symlink("scripts_real", game.root.join("scripts")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"asi")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts_real/a.asi")).unwrap(), b"asi");
    assert!(std::fs::symlink_metadata(game.root.join("scripts")).unwrap().is_symlink());
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv02_game_reached_through_a_linked_library_installs() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real_lib");
    std::fs::create_dir_all(&real).unwrap();
    game_folder(&real, Edition::Enhanced);
    symlink("real_lib", dir.path().join("lib")).unwrap();
    let game = Game::open(&dir.path().join("lib/game")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add><add source="x">x64\new.rpf</add><archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", b"new")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(real.join("game/scripts/a.asi")).unwrap(), b"new");
    assert_eq!(std::fs::read(real.join("game/mods/x64/new.rpf")).unwrap(), b"new");
    assert_eq!(ArchiveEditor::open(&real.join("game/mods/x64/a.rpf"), None).unwrap().read("b.txt", None).unwrap(), b"new");
    assert!(leftovers(&real).is_empty());
}

#[test]
fn oiv02_existing_folder_case_and_separators_and_deep_folders() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::rename(game.root.join("scripts"), game.root.join("Scripts")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts/a.asi</add><add source="x">plugins\deep/er\b.cfg</add><add source="x">x64\deep\er\f.bin</add>"#, &[("content/x", b"new")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(game.root.join("Scripts/a.asi")).unwrap(), b"new");
    assert!(!game.root.join("scripts").exists(), "a second folder was made next to Scripts");
    assert_eq!(std::fs::read(game.root.join("plugins/deep/er/b.cfg")).unwrap(), b"new");
    assert_eq!(std::fs::read(game.root.join("mods/x64/deep/er/f.bin")).unwrap(), b"new");
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv02_hard_linked_mods_copy_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
    std::fs::hard_link(game.root.join("x64/a.rpf"), game.root.join("mods/x64/a.rpf")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root.join("x64"));
    install(&game, &oiv).unwrap();
    assert_unchanged(&before, &snapshot(&game.root.join("x64")));
    assert_eq!(ArchiveEditor::open(&game.root.join("mods/x64/a.rpf"), None).unwrap().read("b.txt", None).unwrap(), b"new");
    assert!(leftovers(&game.root).is_empty());
}

// ---- FS-04: predictable temporary names ----

#[test]
#[ignore = "repro FS-04: an unrelated .a.asi.paleto-tmp is deleted"]
fn fs04_unrelated_paleto_tmp_survives_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/.a.asi.paleto-tmp"), b"not ours").unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), b"new");
    assert_eq!(std::fs::read(game.root.join("scripts/.a.asi.paleto-tmp")).ok().as_deref(), Some(b"not ours".as_slice()), "an unrelated file was deleted");
}

#[test]
#[ignore = "repro FS-04: an unrelated .a.asi.paleto-undo is deleted"]
fn fs04_unrelated_paleto_undo_survives_a_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/a.asi"), b"old").unwrap();
    std::fs::write(game.root.join("scripts/.a.asi.paleto-undo"), b"not ours").unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), b"new");
    assert_eq!(std::fs::read(game.root.join("scripts/.a.asi.paleto-undo")).ok().as_deref(), Some(b"not ours".as_slice()), "an unrelated file was deleted");
}

#[test]
fn fs04_planted_links_at_temporary_names_do_not_reach_outside() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::write(game.root.join("scripts/a.asi"), b"old").unwrap();
    symlink(&outside, game.root.join("scripts/.a.asi.paleto-tmp")).unwrap();
    symlink(&outside, game.root.join("scripts/.a.asi.paleto-undo")).unwrap();
    let oiv = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    install(&game, &oiv).unwrap();
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), b"new");
}

#[test]
#[ignore = "repro FS-04: an unrelated .mods.restoring folder is deleted"]
fn fs04_unrelated_restoring_folder_survives_a_snapshot_restore() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
    std::fs::write(game.root.join("mods/x64/m.rpf"), b"mod").unwrap();
    let snap = crate::snapshots::create(&game, "s").unwrap();
    std::fs::create_dir_all(game.root.join(".mods.restoring")).unwrap();
    std::fs::write(game.root.join(".mods.restoring/user.txt"), b"not ours").unwrap();
    crate::snapshots::restore(&game, &snap.id).unwrap();
    assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), b"mod");
    assert_eq!(std::fs::read(game.root.join(".mods.restoring/user.txt")).ok().as_deref(), Some(b"not ours".as_slice()), "an unrelated folder was deleted");
}

#[test]
#[ignore = "repro FS-04: an unrelated .ScriptHookV.dll.new is deleted"]
fn fs04_unrelated_scripthook_new_file_survives_an_install() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::write(game.root.join(".ScriptHookV.dll.new"), b"not ours").unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("bin/ScriptHookV.dll", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(b"MZ\0\0junkv3889.0/1158.13\0").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    assert_eq!(crate::scripthook::install_from_zip(&game, &bytes).unwrap(), "3889.0/1158.13");
    assert_eq!(std::fs::read(game.root.join(".ScriptHookV.dll.new")).ok().as_deref(), Some(b"not ours".as_slice()), "an unrelated file was deleted");
}

#[test]
#[ignore = "repro FS-04: two installs at once interleave (shared.asi from one package, the archive edit from the other); intermittent, seen within 20 rounds"]
fn fs04_two_installs_at_once_leave_exactly_one_result() {
    for round in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let game = game_folder(dir.path(), Edition::Enhanced);
        let commands = |who: &str| format!(r#"<add source="x">scripts\shared.asi</add><add source="x">scripts\only_{who}.asi</add><archive path="x64\a.rpf"><add source="x">who.txt</add></archive>"#);
        let a = oiv(dir.path(), "a.oiv", &commands("a"), &[("content/x", b"a")], false);
        let b = oiv(dir.path(), "b.oiv", &commands("b"), &[("content/x", b"b")], false);
        let barrier = std::sync::Barrier::new(2);
        let results: Vec<_> = std::thread::scope(|s| {
            let jobs: Vec<_> = [&a, &b].into_iter().map(|oiv| s.spawn(|| {
                let mut pkg = Package::open(oiv).unwrap();
                barrier.wait();
                pkg.install_with(&game, None).map_err(|e| e.to_string())
            })).collect();
            jobs.into_iter().map(|j| j.join().unwrap()).collect()
        });
        let shared = std::fs::read(game.root.join("scripts/shared.asi")).unwrap();
        let who = ArchiveEditor::open(&game.root.join("mods/x64/a.rpf"), None).unwrap().read("who.txt", None).unwrap();
        assert_eq!(who, shared, "round {round}: mixed state ({results:?})");
        for (name, result) in ["a", "b"].into_iter().zip(&results) {
            assert!(result.is_err() || game.root.join(format!("scripts/only_{name}.asi")).exists(), "round {round}: {name} installed but is gone ({results:?})");
        }
        assert!(leftovers(&game.root).is_empty(), "round {round}: {:?}", leftovers(&game.root));
    }
}

// ---- OIV-03: unbounded reads from the package ----

const BIG: usize = 64 << 20;

#[test]
#[ignore = "repro OIV-03: a 64 MiB assembly.xml is read whole"]
fn oiv03_huge_assembly_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>{}"#, " ".repeat(BIG));
    let oiv = oiv_raw(dir.path(), "big.oiv", [("assembly.xml".to_string(), assembly.into_bytes())], true);
    let err = Package::open(&oiv).err().expect("a 64 MiB assembly.xml was read whole").to_string();
    assert!(err.contains("too large") || err.contains("too big") || err.contains("size"), "{err}");
}

#[test]
#[ignore = "repro OIV-03: a 64 MiB icon.png is read whole"]
fn oiv03_huge_icon_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let oiv = oiv_raw(dir.path(), "big.oiv", [("assembly.xml".to_string(), assembly.as_bytes().to_vec()), ("icon.png".to_string(), vec![0; BIG])], true);
    assert!(Package::open(&oiv).unwrap().icon().is_none(), "a 64 MiB icon was read whole");
}

#[test]
#[ignore = "repro OIV-03: a package of 200000 entries is accepted"]
fn oiv03_package_with_too_many_entries_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let entries = std::iter::once(("assembly.xml".to_string(), assembly.as_bytes().to_vec())).chain((0..200_000).map(|i| (format!("content/f{i}"), Vec::new())));
    let oiv = oiv_raw(dir.path(), "many.oiv", entries, false);
    assert!(Package::open(&oiv).is_err(), "a package of 200000 entries was accepted");
}

// ---- OIV-01: archives saved after the loose files are final ----

#[test]
#[ignore = "repro OIV-01: archives are saved after the undo is finished, loose files stay installed"]
fn oiv01_failing_archive_save_leaves_nothing_installed() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    game.mods_copy("x64/a.rpf").unwrap().unwrap();
    let oiv = oiv(
        dir.path(),
        "x.oiv",
        r#"<add source="x">scripts\new.asi</add>
        <archive path="x64\a.rpf"><add source="x">b.txt</add></archive>
        <archive path="x64\b.rpf" createIfNotExist="True"><add source="x">b.txt</add></archive>"#,
        &[("content/x", b"new")],
        false,
    );
    let copy_before = std::fs::read(game.root.join("mods/x64/a.rpf")).unwrap();
    let original = snapshot(&game.root.join("x64"));
    let _guard = ReadOnly::new(&game.root.join("mods/x64"));
    assert!(install(&game, &oiv).is_err());
    assert!(!game.root.join("scripts/new.asi").exists(), "a loose file stays installed after the failure");
    assert_eq!(std::fs::read(game.root.join("mods/x64/a.rpf")).unwrap(), copy_before, "the archive was edited though the install failed");
    assert_unchanged(&original, &snapshot(&game.root.join("x64")));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
}

// ---- OIV-06: rollback errors are ignored ----

/// A file deleted, then its name made a folder by a later `<add>`: the roll-back
/// cannot put the file back, and the error says nothing was installed.
#[test]
#[ignore = "repro OIV-06: a failed roll-back still reports \"nothing was installed\""]
fn oiv06_failed_rollback_is_not_reported_as_clean() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/x"), b"precious").unwrap();
    let oiv = oiv(
        dir.path(),
        "x.oiv",
        r#"<delete>scripts\x</delete>
        <add source="x">scripts\x\y.asi</add>
        <xml path="update\missing.xml"><remove xpath="/a" /></xml>"#,
        &[("content/x", b"new")],
        false,
    );
    let err = install(&game, &oiv).unwrap_err().to_string();
    let restored = std::fs::read(game.root.join("scripts/x")).is_ok_and(|b| b == b"precious");
    assert!(restored || !err.contains("nothing was installed"), "scripts/x is lost but the error says: {err}");
}

// ---- DLC-05: the pack is written before the list is known to work ----

#[test]
#[ignore = "repro DLC-05: dlc.rpf stays installed when the list update fails"]
fn dlc05_list_update_failure_leaves_no_pack() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    std::fs::create_dir_all(game.root.join("mods/update/x64/dlcpacks")).unwrap();
    let original = snapshot(&game.root.join("update"));
    let _guard = ReadOnly::new(&game.root.join("mods/update"));
    assert!(crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).is_err());
    assert!(!crate::addon::pack_path(&game, "piccolo").exists(), "dlc.rpf was left installed but not listed");
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
}

#[test]
#[ignore = "repro DLC-05: dlc.rpf stays installed when update.rpf is corrupt"]
fn dlc05_corrupt_update_rpf_leaves_no_pack() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    std::fs::write(game.root.join("update/update.rpf"), b"garbage that is not an archive").unwrap();
    let original = snapshot(&game.root.join("update"));
    assert!(crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).is_err());
    assert!(!crate::addon::pack_path(&game, "piccolo").exists(), "dlc.rpf was left installed but not listed");
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
}

#[test]
fn dlc05_registered_pack_installs_again_without_touching_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    update_rpf(&game.root.join("update/update.rpf"), &["mpheist", "piccolo"]);
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    let original_bytes = std::fs::read(game.root.join("update/update.rpf")).unwrap();
    let original = snapshot(&game.root.join("update"));
    let log = crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).unwrap();
    assert_eq!(log[1], "piccolo is already in the DLC list");
    let copy = game.root.join("mods/update/update.rpf");
    let copy_bytes = std::fs::read(&copy).unwrap();
    assert_eq!(copy_bytes, original_bytes, "the list was rewritten");
    let log = crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).unwrap();
    assert_eq!(log[1], "piccolo is already in the DLC list");
    assert_eq!(std::fs::read(&copy).unwrap(), copy_bytes);
    assert_eq!(list_of(&copy).matches("piccolo").count(), 1);
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(leftovers(&game.root).is_empty());
}
