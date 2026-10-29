//! Reproductions of write-path findings (OIV-01/02/03/06, FS-04, DLC-05):
//! tests marked `#[ignore]` fail on the current code and say why.

use std::os::unix::fs::symlink;
use std::path::Path;

use paleto_rpf::ArchiveEditor;

use crate::budget::Limits;
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
fn oiv03_huge_assembly_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>{}"#, " ".repeat(BIG));
    let oiv = oiv_raw(dir.path(), "big.oiv", [("assembly.xml".to_string(), assembly.into_bytes())], true);
    let err = Package::open(&oiv).err().expect("a 64 MiB assembly.xml was read whole").to_string();
    assert!(err.contains("assembly.xml is larger than 16 MiB"), "{err}");
}

#[test]
fn oiv03_huge_icon_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let oiv = oiv_raw(dir.path(), "big.oiv", [("assembly.xml".to_string(), assembly.as_bytes().to_vec()), ("icon.png".to_string(), vec![0; BIG])], true);
    assert!(Package::open(&oiv).unwrap().icon().is_none(), "a 64 MiB icon was read whole");
}

#[test]
fn oiv03_package_with_too_many_entries_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let entries = std::iter::once(("assembly.xml".to_string(), assembly.as_bytes().to_vec())).chain((0..200_000).map(|i| (format!("content/f{i}"), Vec::new())));
    let oiv = oiv_raw(dir.path(), "many.oiv", entries, false);
    assert!(Package::open(&oiv).is_err(), "a package of 200000 entries was accepted");
}

/// Pseudo-random bytes (they do not compress).
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len).map(|_| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 32) as u8
    }).collect()
}

/// Where the local and the central header of the entry `name` start in a package.
fn headers(data: &[u8], name: &str) -> (usize, usize) {
    // (signature, where the name's length is, where the name is)
    let find = |sig: &[u8], len_at: usize, name_at: usize| {
        (0..data.len() - 46).find(|&i| data[i..].starts_with(sig) && data[i + name_at..].starts_with(name.as_bytes()) && u16::from_le_bytes([data[i + len_at], data[i + len_at + 1]]) as usize == name.len())
    };
    (find(b"PK\x03\x04", 26, 30).expect("local header"), find(b"PK\x01\x02", 28, 46).expect("central header"))
}

/// Rewrites the uncompressed size of the entry `name` in both its headers of a package.
fn patch_size(oiv: &Path, name: &str, uncompressed: u32) {
    let mut data = std::fs::read(oiv).unwrap();
    let (local, central) = headers(&data, name);
    data[local + 22..local + 26].copy_from_slice(&uncompressed.to_le_bytes());
    data[central + 24..central + 28].copy_from_slice(&uncompressed.to_le_bytes());
    std::fs::write(oiv, data).unwrap();
}

/// Rewrites the compressed size (what the entry says it takes up) of the entry `name` in both its headers of a package.
fn patch_packed(oiv: &Path, name: &str, compressed: u32) {
    let mut data = std::fs::read(oiv).unwrap();
    let (local, central) = headers(&data, name);
    data[local + 18..local + 22].copy_from_slice(&compressed.to_le_bytes());
    data[central + 20..central + 24].copy_from_slice(&compressed.to_le_bytes());
    std::fs::write(oiv, data).unwrap();
}

/// Rewrites the end record of a package.
fn patch_end(oiv: &Path, edit: impl FnOnce(&mut [u8])) {
    let mut data = std::fs::read(oiv).unwrap();
    let at = data.len() - 22;
    assert_eq!(&data[at..at + 4], b"PK\x05\x06");
    edit(&mut data[at..]);
    std::fs::write(oiv, data).unwrap();
}

fn install_limited(game: &Game, oiv: &Path, limits: Limits) -> Result<Vec<String>, Error> {
    Package::open_with(oiv, limits)?.install_with(game, None)
}

/// An install that must fail, and leave the game and mods folders as they were; says why.
fn refused(game: &Game, oiv: &Path, limits: Limits) -> String {
    let before = snapshot(&game.root);
    let err = install_limited(game, oiv, limits).expect_err("the package was installed").to_string();
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
    err
}

#[test]
fn oiv03_a_bomb_with_a_small_header_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let bomb = vec![0; 8 << 20];
    let first = oiv(dir.path(), "bomb.oiv", r#"<add source="x">scripts\new.asi</add>"#, &[("content/x", &bomb)], true);
    // The headers say 1000 bytes; the stream holds 8 MiB.
    patch_size(&first, "content/x", 1000);
    // (The zip reader notices it too; whichever does first, the install stops.)
    let overflow = |err: &str| err.contains("holds more than the 1000 bytes") || err.contains("larger than its declared");
    let err = refused(&game, &first, Limits::PACKAGE);
    assert!(overflow(&err), "{err}");
    // The same inside an archive.
    let second = oiv(dir.path(), "bomb2.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", &bomb)], true);
    patch_size(&second, "content/x", 1000);
    let err = refused(&game, &second, Limits::PACKAGE);
    assert!(overflow(&err), "{err}");
}

#[test]
fn oiv03_a_header_that_declares_more_than_the_file_holds_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let oiv = oiv(dir.path(), "short.oiv", r#"<add source="x">scripts\new.asi</add>"#, &[("content/x", &vec![0; 1 << 20])], true);
    patch_size(&oiv, "content/x", 3 << 20);
    let err = refused(&game, &oiv, Limits::PACKAGE);
    assert!(err.contains("not the 3145728 its header says"), "{err}");
    // A manifest that lies the same way.
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let oiv = oiv_raw(dir.path(), "lie.oiv", [("assembly.xml".to_string(), assembly.as_bytes().to_vec())], true);
    patch_size(&oiv, "assembly.xml", assembly.len() as u32 + 1000);
    assert!(Package::open(&oiv).err().expect("a lying manifest was read").to_string().contains("its header says"));
}

#[test]
fn oiv03_the_end_record_is_judged_before_the_zip_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let entries = || std::iter::once(("assembly.xml".to_string(), assembly.as_bytes().to_vec())).chain((0..5).map(|i| (format!("content/f{i}"), vec![i as u8])));
    let small = Limits { entries: 5, ..Limits::PACKAGE };
    // Six entries against a limit of five: refused by the count in the end record.
    let oiv = oiv_raw(dir.path(), "six.oiv", entries(), false);
    let err = Package::open_with(&oiv, small).err().expect("six entries were accepted").to_string();
    assert!(err.contains("the archive has 6 files; more than 5 are refused"), "{err}");
    // An end record that claims 60000 entries (the file holds 6) is refused by that claim alone.
    patch_end(&oiv, |end| end[8..12].copy_from_slice(&[0x60, 0xEA, 0x60, 0xEA]));
    let err = Package::open_with(&oiv, small).err().unwrap().to_string();
    assert!(err.contains("the archive has 60000 files"), "{err}");
    // So is one that claims a list of 100 MiB.
    let oiv = oiv_raw(dir.path(), "wide.oiv", entries(), false);
    patch_end(&oiv, |end| end[12..16].copy_from_slice(&(100u32 << 20).to_le_bytes()));
    let err = Package::open(&oiv).err().unwrap().to_string();
    assert_eq!(err, "the list of the archive's files is larger than 64 MiB");
    // The same file is fine under limits it fits.
    let ok = oiv_raw(dir.path(), "ok.oiv", entries(), false);
    assert!(Package::open_with(&ok, Limits { entries: 6, ..Limits::PACKAGE }).is_ok());
}

#[test]
fn oiv03_a_package_just_under_each_limit_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content><add source="x">scripts\new.asi</add></content></package>"#;
    let icon = vec![9; 100];
    let entries = [("assembly.xml".to_string(), assembly.as_bytes().to_vec()), ("icon.png".to_string(), icon.clone()), ("content/x".to_string(), vec![1; 300])];
    let oiv = oiv_raw(dir.path(), "edge.oiv", entries, true);
    let exact = Limits { manifest: assembly.len() as u64, icon: 100, entries: 3, in_memory: 300, in_memory_total: 300, streamed: 300, total: 300, ..Limits::PACKAGE };
    let mut package = Package::open_with(&oiv, exact).unwrap();
    assert_eq!(package.icon().unwrap(), icon);
    let game = game_folder(dir.path(), Edition::Enhanced);
    package.install_with(&game, None).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/new.asi")).unwrap(), vec![1; 300]);
    // One under each: refused (or, for the icon, left out).
    assert!(Package::open_with(&oiv, Limits { manifest: exact.manifest - 1, ..exact }).is_err());
    assert!(Package::open_with(&oiv, Limits { entries: 2, ..exact }).is_err());
    assert!(Package::open_with(&oiv, Limits { icon: 99, ..exact }).unwrap().icon().is_none());
    let other = tempfile::tempdir().unwrap();
    let err = refused(&game_folder(other.path(), Edition::Enhanced), &oiv, Limits { streamed: 299, ..exact });
    assert!(err.contains("content/x is larger than 299 bytes"), "{err}");
}

#[test]
fn oiv03_large_payloads_are_streamed_not_held() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // Nothing may be held beyond 1 MiB, and 2 MiB in all.
    let limits = Limits { in_memory: 1 << 20, in_memory_total: 2 << 20, ..Limits::PACKAGE };
    let big = noise(10 << 20);
    let pack = dir.path().join("pack.rpf");
    rpf(&pack, "pack.rpf", &[("big.bin", &big)]);
    let pack = std::fs::read(&pack).unwrap();
    assert!(pack.len() > 10 << 20);
    let oiv = oiv(
        dir.path(),
        "big.oiv",
        r#"<add source="big">scripts\big.asi</add>
        <archive path="x64\a.rpf"><add source="pack">inner\pack.rpf</add></archive>"#,
        &[("content/big", &big), ("content/pack", &pack)],
        true,
    );
    let original = snapshot(&game.root.join("x64"));
    install_limited(&game, &oiv, limits).unwrap();
    assert!(std::fs::read(game.root.join("scripts/big.asi")).unwrap() == big, "the loose file differs");
    let installed = ArchiveEditor::open(&game.root.join("mods/x64/a.rpf"), None).unwrap();
    assert_eq!(installed.read("a.txt", None).unwrap(), b"a");
    assert!(installed.read("inner/pack.rpf", None).unwrap() == pack, "the archive added into the archive differs");
    assert_unchanged(&original, &snapshot(&game.root.join("x64")));
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn oiv03_a_file_for_inside_an_archive_is_held_within_the_caps() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let limits = Limits { in_memory: 1 << 20, in_memory_total: 2 << 20, ..Limits::PACKAGE };
    // Not an archive, sound or video: it has to be held, and it is over the cap.
    let oiv = oiv(dir.path(), "big.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", &noise(2 << 20))], true);
    let err = refused(&game, &oiv, limits);
    assert!(err.contains("content/x is larger than 1 MiB"), "{err}");
}

#[test]
fn oiv03_the_budget_runs_out_across_entries() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // Each file is within every cap; together they are not.
    let files = [noise(1 << 20), noise(1 << 20), noise(1 << 20)];
    let content = [("content/a", files[0].as_slice()), ("content/b", files[1].as_slice()), ("content/c", files[2].as_slice())];
    let loose = oiv(dir.path(), "three.oiv", r#"<add source="a">scripts\a.asi</add><add source="b">scripts\b.asi</add><add source="c">scripts\c.asi</add>"#, &content, false);
    let err = refused(&game, &loose, Limits { total: 2 << 20, ..Limits::PACKAGE });
    assert!(err.contains("everything read from the archive is larger than 2 MiB"), "{err}");
    // The same held in memory, inside an archive.
    let held = oiv(dir.path(), "texts.oiv", r#"<archive path="x64\a.rpf"><add source="a">a.bin</add><add source="b">b.bin</add><add source="c">c.bin</add></archive>"#, &content, false);
    let err = refused(&game, &held, Limits { in_memory: 1 << 20, in_memory_total: 2 << 20, ..Limits::PACKAGE });
    assert!(err.contains("the files held in memory together is larger than 2 MiB"), "{err}");
    // And all three go in when the budget has room for them.
    install_limited(&game, &held, Limits { in_memory: 1 << 20, in_memory_total: 4 << 20, ..Limits::PACKAGE }).unwrap();
}

/// Puts `comment` in the end record of a package (which has none).
fn add_comment(oiv: &Path, comment: &[u8]) {
    let mut data = std::fs::read(oiv).unwrap();
    let at = data.len() - 2;
    assert_eq!(&data[at..], [0, 0]);
    data[at..].copy_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
    data.extend_from_slice(comment);
    std::fs::write(oiv, data).unwrap();
}

#[test]
fn oiv03_an_end_record_hidden_in_the_comment_is_refused_before_the_zip_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>"#;
    let entries = || std::iter::once(("assembly.xml".to_string(), assembly.as_bytes().to_vec())).chain((0..5).map(|i| (format!("content/f{i}"), vec![i as u8])));
    let limits = Limits { entries: 8, ..Limits::PACKAGE };
    // A second end record in the comment of the real one, claiming 60000 files, and a byte after it that its own
    // (empty) comment does not cover: the file does not end with it, but a zip reader that looks back from the end finds it first.
    let fake = [b"PK\x05\x06\0\0\0\0".as_slice(), &60_000u16.to_le_bytes(), &60_000u16.to_le_bytes(), &150u32.to_le_bytes(), &20u32.to_le_bytes(), &[0, 0], &[0]].concat();
    let hidden = oiv_raw(dir.path(), "hidden.oiv", entries(), false);
    add_comment(&hidden, &fake);
    let err = Package::open_with(&hidden, limits).err().expect("a package with a hidden end record was opened").to_string();
    // (The message of the precheck, not the one of the count after the zip is read.)
    assert!(err.contains("the archive has 60000 files; more than 8 are refused"), "{err}");
    // The same package without it opens, and so does one that holds a zip of its own as its last file.
    assert!(Package::open_with(&oiv_raw(dir.path(), "plain.oiv", entries(), false), limits).is_ok());
    let inner = std::fs::read(oiv_raw(dir.path(), "inner.zip", [("a".to_string(), vec![1; 10])], false)).unwrap();
    let nested = oiv_raw(dir.path(), "nested.oiv", entries().chain(std::iter::once(("content/inner.zip".to_string(), inner))), false);
    assert!(Package::open_with(&nested, limits).is_ok());
}

#[test]
fn oiv03_a_file_that_expands_too_much_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // Zeros pack to a few KiB: more than 200 times, and over the 16 MiB below which the ratio is not judged.
    let sparse = oiv(dir.path(), "sparse.oiv", r#"<add source="x">scripts\new.asi</add>"#, &[("content/x", &vec![0; (16 << 20) + 1])], true);
    let err = refused(&game, &sparse, Limits::PACKAGE);
    assert!(err.contains("content/x (packed to") && err.contains("200 times that at most) is larger than"), "{err}");
    // The same inside an archive, where it would be held.
    let held = oiv(dir.path(), "held.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", &vec![0; 2 << 20])], true);
    let err = refused(&game, &held, Limits { ratio_after: 1 << 20, ..Limits::PACKAGE });
    assert!(err.contains("content/x (packed to"), "{err}");
    // A manifest that expands too much is stopped as it is read.
    let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content></content></package>{}"#, " ".repeat(2 << 20));
    let spaces = oiv_raw(dir.path(), "spaces.oiv", [("assembly.xml".to_string(), assembly.into_bytes())], true);
    let err = Package::open_with(&spaces, Limits { ratio_after: 1 << 20, ..Limits::PACKAGE }).err().expect("a manifest that expands too much was read").to_string();
    assert!(err.contains("assembly.xml (packed to"), "{err}");
    // Up to the 16 MiB it is not judged, and a file that does not pack is no matter at any size.
    let fine = oiv(dir.path(), "fine.oiv", r#"<add source="x">scripts\a.asi</add><add source="y">scripts\b.asi</add>"#, &[("content/x", &vec![0; 16 << 20]), ("content/y", &noise(17 << 20))], true);
    install_limited(&game, &fine, Limits::PACKAGE).unwrap();
    assert_eq!(std::fs::metadata(game.root.join("scripts/a.asi")).unwrap().len(), 16 << 20);
    assert_eq!(std::fs::metadata(game.root.join("scripts/b.asi")).unwrap().len(), 17 << 20);
}

#[test]
fn oiv03_spooled_files_count_against_the_total() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // Three 1 MiB videos for inside an archive: spooled to files, never held, but read all the same.
    let files = [noise(1 << 20), noise(1 << 20), noise(1 << 20)];
    let content = [("content/a", files[0].as_slice()), ("content/b", files[1].as_slice()), ("content/c", files[2].as_slice())];
    let videos = oiv(dir.path(), "videos.oiv", r#"<archive path="x64\a.rpf"><add source="a">a.bik</add><add source="b">b.bik</add><add source="c">c.bik</add></archive>"#, &content, false);
    let err = refused(&game, &videos, Limits { total: 2 << 20, in_memory_total: 1 << 20, ..Limits::PACKAGE });
    assert!(err.contains("everything read from the archive is larger than 2 MiB"), "{err}");
    install_limited(&game, &videos, Limits { total: 4 << 20, in_memory_total: 1 << 20, ..Limits::PACKAGE }).unwrap();
    let installed = ArchiveEditor::open(&game.root.join("mods/x64/a.rpf"), None).unwrap();
    assert!(installed.read("c.bik", None).unwrap() == files[2], "the video differs");
}

#[test]
fn oiv03_a_file_may_not_claim_more_room_than_it_has() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let commands = r#"<add source="x">scripts\a.asi</add><add source="y">scripts\b.asi</add>"#;
    // Zeros pack to 20 KiB or so, 1000 times: refused as it is.
    let honest = oiv(dir.path(), "honest.oiv", commands, &[("content/x", &vec![0; 20 << 20]), ("content/y", b"y")], true);
    assert!(refused(&game, &honest, Limits::PACKAGE).contains("content/x (packed to"));
    // The same, saying it takes up 1 MiB, which would make the ratio 20 and pass: but 1 MiB goes past the next file.
    let lying = oiv(dir.path(), "lying.oiv", commands, &[("content/x", &vec![0; 20 << 20]), ("content/y", b"y")], true);
    patch_packed(&lying, "content/x", 1 << 20);
    let err = Package::open(&lying).err().expect("a file that claims room it does not have was accepted").to_string();
    assert!(err.contains("damaged zip file"), "{err}");
    // The last one into the list of files.
    let last = oiv(dir.path(), "last.oiv", commands, &[("content/y", b"y"), ("content/x", &vec![0; 20 << 20])], true);
    patch_packed(&last, "content/x", 1 << 20);
    assert!(Package::open(&last).err().expect("a file that claims room in the list of files was accepted").to_string().contains("damaged zip file"));
    // Two entries that share one header (and its data) are not a package either.
    let shared = oiv(dir.path(), "shared.oiv", commands, &[("content/x", &vec![0; 1 << 20]), ("content/y", b"y")], true);
    let mut data = std::fs::read(&shared).unwrap();
    let (x, y) = (headers(&data, "content/x"), headers(&data, "content/y"));
    let at = u32::try_from(x.0).unwrap().to_le_bytes();
    data[y.1 + 42..y.1 + 46].copy_from_slice(&at);
    std::fs::write(&shared, data).unwrap();
    let err = Package::open(&shared).err().expect("two files with one header were accepted").to_string();
    assert!(err.contains("damaged zip file"), "{err}");
    // A package with stored and packed files, a zip inside it, and a file that does not pack, is fine.
    let inner = std::fs::read(oiv_raw(dir.path(), "inner.zip", [("a".to_string(), vec![1; 10])], false)).unwrap();
    for deflate in [false, true] {
        let fine = oiv(dir.path(), "fine.oiv", commands, &[("content/x", &inner), ("content/y", &noise(1 << 20))], deflate);
        install_limited(&game, &fine, Limits::PACKAGE).unwrap();
        assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), inner);
    }
}

/// The package at `oiv` as a zip64 one: the same files, with the end record in the zip64 form (the markers, and the record and locator before it).
fn make_zip64(oiv: &Path) {
    let mut data = std::fs::read(oiv).unwrap();
    let end = data.len() - 22;
    let (entries, size, offset) = (u16::from_le_bytes([data[end + 10], data[end + 11]]), u32::from_le_bytes(data[end + 12..end + 16].try_into().unwrap()), u32::from_le_bytes(data[end + 16..end + 20].try_into().unwrap()));
    data.truncate(end);
    let record = data.len() as u64;
    data.extend_from_slice(b"PK\x06\x06");
    data.extend_from_slice(&44u64.to_le_bytes());
    data.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    data.extend_from_slice(&u64::from(entries).to_le_bytes());
    data.extend_from_slice(&u64::from(entries).to_le_bytes());
    data.extend_from_slice(&u64::from(size).to_le_bytes());
    data.extend_from_slice(&u64::from(offset).to_le_bytes());
    data.extend_from_slice(b"PK\x06\x07\0\0\0\0");
    data.extend_from_slice(&record.to_le_bytes());
    data.extend_from_slice(&[1, 0, 0, 0]);
    data.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    data.extend_from_slice(&[0xFF; 12]);
    data.extend_from_slice(&[0, 0]);
    std::fs::write(oiv, data).unwrap();
}

#[test]
fn oiv03_a_real_zip64_package_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let package = oiv(dir.path(), "big.oiv", r#"<add source="x">scripts\new.asi</add>"#, &[("content/x", b"zip64")], true);
    make_zip64(&package);
    assert_eq!(Package::open(&package).unwrap().metadata.name, "X");
    Package::open(&package).unwrap().install_with(&game, None).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/new.asi")).unwrap(), b"zip64");
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

// ---- A2b: unique exclusive names, the game lock, interrupted restores, rollback reports ----

fn script_hook_zip() -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("bin/ScriptHookV.dll", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(b"MZ\0\0junkv3889.0/1158.13\0").unwrap();
    zip.finish().unwrap().into_inner()
}

/// Plants a file (or, with `links`, a link to something outside) at every name an older version
/// used for its temporaries, and returns the names (relative to the game folder).
fn plant_old_names(root: &Path, outside: &Path, links: bool) -> Vec<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut names: Vec<String> = ["scripts/.a.asi.paleto-tmp", "scripts/.a.asi.paleto-undo", "scripts/.new.asi.paleto-tmp", ".ScriptHookV.dll.new", ".mods.restoring", ".mods.replaced"].map(String::from).into();
    names.extend((now.saturating_sub(5)..now + 10).map(|s| format!("mods_snapshots/.{s}.partial")));
    std::fs::create_dir_all(root.join("mods_snapshots")).unwrap();
    for name in &names {
        let path = root.join(name);
        let folder = name.starts_with(".mods.") || name.ends_with(".partial");
        if links {
            symlink(if folder { outside.to_path_buf() } else { outside.join("file.txt") }, &path).unwrap();
        } else if folder {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("user.txt"), b"not ours").unwrap();
        } else {
            std::fs::write(&path, b"not ours").unwrap();
        }
    }
    names
}

#[test]
fn old_predictable_names_survive_installs_snapshots_and_script_hook_installs() {
    for links in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let game = game_folder(dir.path(), Edition::Enhanced);
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file.txt"), b"outside").unwrap();
        std::fs::write(outside.join("user.txt"), b"outside").unwrap();
        std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"mod").unwrap();
        std::fs::write(game.root.join("scripts/a.asi"), b"old").unwrap();
        let names = plant_old_names(&game.root, &outside, links);
        let planted = |root: &Path| -> std::collections::BTreeMap<String, Entry> { snapshot(root).into_iter().filter(|(k, _)| names.iter().any(|n| k == n || k.starts_with(&format!("{n}/")))).collect() };
        let (before, before_out) = (planted(&game.root), snapshot(&outside));
        assert!(before.len() >= names.len());

        let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add><add source="x">scripts\new.asi</add><archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#, &[("content/x", b"new")], false);
        install(&game, &package).unwrap();
        let snap = crate::snapshots::create(&game, "s").unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"changed").unwrap();
        crate::snapshots::restore(&game, &snap.id).unwrap();
        assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), b"mod");
        crate::snapshots::delete(&game, &snap.id).unwrap();
        crate::scripthook::install_from_zip(&game, &script_hook_zip()).unwrap();

        assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), b"new", "links: {links}");
        assert_unchanged(&before, &planted(&game.root));
        assert_unchanged(&before_out, &snapshot(&outside));
        let ours: Vec<String> = leftovers(&game.root).into_iter().filter(|l| !names.iter().any(|n| l == n || l.starts_with(&format!("{n}/")))).collect();
        assert!(ours.is_empty(), "{ours:?}");
    }
}

#[test]
fn a_second_writer_gets_busy_while_other_games_and_reads_go_on() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let other_dir = tempfile::tempdir().unwrap();
    let other = game_folder(other_dir.path(), Edition::Enhanced);
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    let other_package = oiv(other_dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    let roots = game.roots().unwrap();
    let held = crate::txn::guard(&roots, "a test holds the lock").unwrap();
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    std::thread::scope(|s| {
        s.spawn(|| {
            let busy = |what: &str, r: Result<(), Error>| assert!(matches!(r, Err(Error::Busy(_))), "{what}: {r:?}");
            busy("install", install(&game, &package).map(drop));
            busy("snapshot", crate::snapshots::create(&game, "x").map(drop));
            busy("script hook", crate::scripthook::install_from_zip(&game, &script_hook_zip()).map(drop));
            busy("mods copy", game.mods_copy("x64/a.rpf").map(drop));
            busy("cleanup", crate::cleanup::delete(&game, &[]).map(drop));
            busy("hunt", crate::crash_hunt::start(&game).map(drop));
            busy("pack", crate::addon::install_pack(&game, None, &dir.path().join("none.rpf"), "piccolo", None).map(drop));
            // Reading, and another game, are not held up.
            assert!(crate::snapshots::list(&game).is_empty());
            install(&other, &other_package).unwrap();
        });
    });
    drop(held);
    install(&game, &package).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/a.asi")).unwrap(), b"new");
}

#[test]
fn a_restore_interrupted_between_its_renames_is_put_right_at_the_next_change() {
    use crate::fsroot::faults::{self, Mode, Point};
    for k in 1..=3 {
        let dir = tempfile::tempdir().unwrap();
        let game = game_folder(dir.path(), Edition::Enhanced);
        std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"snapshotted").unwrap();
        let snap = crate::snapshots::create(&game, "s").unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"current").unwrap();
        faults::arm_at(Point::Rename, k, Mode::Crash);
        let result = crate::snapshots::restore(&game, &snap.id);
        if !faults::disarm() {
            result.unwrap();
            assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), b"snapshotted");
            break;
        }
        assert!(result.is_err(), "k={k}");
        assert_eq!(crate::txn::pending(&game).len(), 1, "k={k}: the interrupted restore is reported");
        // The next change (here a snapshot) undoes it first: the mods folder is whole again.
        let saved = crate::snapshots::create(&game, "after").unwrap();
        assert_eq!(std::fs::read(saved.dir.join("mods/x64/m.rpf")).unwrap(), b"current", "k={k}");
        assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), b"current", "k={k}");
        assert!(crate::txn::pending(&game).is_empty() && crate::txn::leftovers(&game).is_empty(), "k={k}: {:?}", crate::txn::leftovers(&game));
        crate::snapshots::restore(&game, &snap.id).unwrap();
        assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), b"snapshotted");
    }
}

#[test]
fn the_leftovers_scan_lists_temporary_names_and_never_touches_them() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
    std::fs::create_dir_all(game.root.join("mods_snapshots")).unwrap();
    for rel in ["mods/x64/.paleto-0123456789abcdef-0.old", "mods_snapshots/.paleto-fedcba9876543210.partial", ".paleto-aaaaaaaaaaaaaaaa.new"] {
        std::fs::write(game.root.join(rel), b"orphan").unwrap();
    }
    let before = snapshot(&game.root);
    let found: Vec<String> = crate::txn::leftovers(&game).iter().map(|p| p.strip_prefix(&game.root).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, [".paleto-aaaaaaaaaaaaaaaa.new", "mods/x64/.paleto-0123456789abcdef-0.old", "mods_snapshots/.paleto-fedcba9876543210.partial"]);
    assert_unchanged(&before, &snapshot(&game.root));
    // They are not mods: the listings of the mods folder skip them.
    assert!(crate::loader::modded_files(&game, None).is_empty());
    assert!(crate::vfs::local_files(&game.root.join("mods")).unwrap().is_empty());
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    install(&game, &package).unwrap();
    assert_eq!(crate::txn::leftovers(&game).len(), 3);
}

#[test]
fn a_rollback_that_cannot_finish_says_so_and_names_the_backup() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/x"), b"precious").unwrap();
    let package = oiv(dir.path(), "x.oiv", r#"<delete>scripts\x</delete><add source="x">scripts\x\y.asi</add><xml path="update\missing.xml"><remove xpath="/a" /></xml>"#, &[("content/x", b"new")], false);
    // Something else puts a file in the folder the install made, so the folder stays and the original cannot go back.
    let user_file = game.root.join("scripts/x/user.txt");
    let planted = user_file.clone();
    crate::oiv::ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || std::fs::write(&planted, b"user").unwrap())));
    let err = install(&game, &package).unwrap_err();
    let text = err.to_string();
    assert!(!text.contains("nothing was installed") && text.contains(".paleto-") && text.contains("scripts/x"), "{text}");
    let Error::Txn(failure) = &err else { panic!("{err:?}") };
    assert!(!failure.undone && !failure.failures.is_empty());
    assert_eq!(std::fs::read(&user_file).unwrap(), b"user");
    let backups: Vec<_> = std::fs::read_dir(game.root.join("scripts")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".paleto-")).collect();
    assert_eq!(backups.len(), 1, "the backup is kept");
    assert_eq!(std::fs::read(backups[0].path()).unwrap(), b"precious");
}

#[test]
fn a_clean_rollback_puts_everything_back_and_says_nothing_was_installed() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/x"), b"precious").unwrap();
    let package = oiv(dir.path(), "x.oiv", r#"<delete>scripts\x</delete><add source="x">scripts\x\y.asi</add><xml path="update\missing.xml"><remove xpath="/a" /></xml>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root);
    let err = install(&game, &package).unwrap_err();
    assert!(err.to_string().contains("nothing was installed"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
}

// ---- follow-ups of the security review ----

#[test]
fn restoring_with_a_linked_mods_folder_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let real = dir.path().join("other_drive/mods_real");
    std::fs::create_dir_all(real.join("x64")).unwrap();
    std::fs::write(real.join("x64/m.rpf"), b"mod").unwrap();
    symlink(&real, game.root.join("mods")).unwrap();
    let snap = crate::snapshots::create(&game, "s").unwrap();
    std::fs::write(real.join("x64/m.rpf"), b"changed").unwrap();
    let (before, before_real) = (snapshot(&game.root), snapshot(&real));
    let err = crate::snapshots::restore(&game, &snap.id).unwrap_err().to_string();
    assert!(err.contains("link") && err.contains("mods_real"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    assert_unchanged(&before_real, &snapshot(&real));
    assert!(std::fs::symlink_metadata(game.root.join("mods")).unwrap().is_symlink());
    assert!(crate::txn::pending(&game).is_empty() && crate::txn::leftovers(&game).is_empty());
}

#[test]
fn a_flush_that_fails_after_the_swap_keeps_the_old_folder_and_is_not_reported_as_done() {
    use crate::fsroot::faults::{self, Mode, Point};
    let old_folder = |game: &Game| std::fs::read_dir(&game.root).unwrap().flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with("-mods.old"));
    let (mut unconfirmed, mut fired_any) = (false, false);
    for k in 1..=12 {
        let dir = tempfile::tempdir().unwrap();
        let game = game_folder(dir.path(), Edition::Enhanced);
        std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"snapshotted").unwrap();
        let snap = crate::snapshots::create(&game, "s").unwrap();
        std::fs::write(game.root.join("mods/x64/m.rpf"), b"current").unwrap();
        faults::arm_at(Point::Sync, k, Mode::Fail(std::io::ErrorKind::Other));
        let result = crate::snapshots::restore(&game, &snap.id);
        let fired = faults::disarm();
        let now = std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap();
        fired_any |= fired;
        match result {
            Ok(()) => assert_eq!(now, b"snapshotted", "k={k}"),
            Err(e) if e.to_string().contains("not yet confirmed") => {
                // The swap is in place but was not confirmed on disk: the old folder and the journal stay.
                unconfirmed = true;
                assert_eq!(now, b"snapshotted", "k={k}");
                assert!(old_folder(&game).is_some(), "k={k}: the old mods folder must be kept ({e})");
                assert!(crate::txn::pending(&game).iter().any(|p| p.state == crate::txn::PendingState::Committed), "k={k}");
                // A recovery whose own flush fails removes nothing either, and says so.
                faults::arm_at(Point::Sync, 1, Mode::Fail(std::io::ErrorKind::Other));
                let again = crate::txn::recover(&game).unwrap();
                faults::disarm();
                assert!(again.failures.iter().any(|f| f.contains("flushed")) && old_folder(&game).is_some(), "k={k}: {again:?}");
            }
            Err(e) => assert_eq!(now, b"current", "k={k}: an error, so the mods folder is as it was ({e})"),
        }
        crate::txn::recover(&game).unwrap();
        assert!(crate::txn::pending(&game).is_empty() && crate::txn::leftovers(&game).is_empty() && old_folder(&game).is_none(), "k={k}: {:?}", crate::txn::leftovers(&game));
        assert_eq!(std::fs::read(game.root.join("mods/x64/m.rpf")).unwrap(), now, "k={k}: the recovery does not change which folder is in place");
        if !fired {
            break;
        }
    }
    assert!(fired_any && unconfirmed, "a flush that fails after the swap must be reported as not confirmed, not as a finished restore");
}

#[test]
fn an_archive_on_the_drive_a_mods_link_leads_to_is_edited_under_the_game_lock() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let real = dir.path().join("other_drive/mods_real");
    rpf(&real.join("x64/a.rpf"), "a.rpf", &[("a.txt", b"a")]);
    symlink(&real, game.root.join("mods")).unwrap();
    let archive = crate::ArchivePath::new(real.join("x64/a.rpf"));
    let roots = game.roots().unwrap();
    let held = crate::txn::guard(&roots, "a test holds the lock").unwrap();
    let busy = archive.edit(None, |_, _| Ok(()));
    assert!(matches!(busy, Err(Error::Busy(_))), "{busy:?}");
    drop(held);
    archive.edit(None, |_, _| Ok(())).unwrap();
}

#[test]
fn a_path_reached_through_a_link_into_a_game_never_opened_takes_that_games_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("games/gta");
    std::fs::create_dir_all(root.join("scripts/deep")).unwrap();
    std::fs::write(root.join(Edition::Enhanced.exe_name()), b"").unwrap();
    // Built by hand, never passed to `Game::open`, so this program has not remembered the game.
    let game = Game { root: root.clone(), edition: Edition::Enhanced };
    let link = dir.path().join("elsewhere/shortcut");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(root.join("scripts/deep"), &link).unwrap();
    let roots = game.roots().unwrap();
    let held = crate::txn::guard(&roots, "a test holds the lock").unwrap();
    let busy = crate::vfs::locked_in_game(&link, "renaming a file", || Ok(()));
    assert!(matches!(busy, Err(Error::Busy(_))), "{busy:?}");
    drop(held);
    crate::vfs::locked_in_game(&link, "renaming a file", || Ok(())).unwrap();
    // A folder that leads into no game is not held up.
    let free = dir.path().join("elsewhere/free");
    std::fs::create_dir_all(&free).unwrap();
    let held = crate::txn::guard(&roots, "a test holds the lock").unwrap();
    crate::vfs::locked_in_game(&free, "renaming a file", || Ok(())).unwrap();
    drop(held);
}
