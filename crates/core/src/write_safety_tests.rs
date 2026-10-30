//! Reproductions of write-path findings (OIV-01/02/03/06, FS-04, DLC-05):
//! tests marked `#[ignore]` fail on the current code and say why.

use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use paleto_rpf::ArchiveEditor;

use crate::budget::Limits;
use crate::fixtures::*;
use crate::fsroot::faults::{self, Mode as Fault, Point};
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

#[test]
fn oiv03_a_zip_is_never_searched_for_where_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    // 2 MiB took a zip reader that searches seven seconds, and 64 MiB would take hours.
    let path = dir.path().join("bait.oiv");
    std::fs::write(&path, crate::fixtures::search_bait(2 << 20)).unwrap();
    let start = std::time::Instant::now();
    let err = Package::open(&path).err().expect("a package made of bait was opened").to_string();
    assert!(start.elapsed() < std::time::Duration::from_millis(500), "took {:?}: {err}", start.elapsed());
    // A package with something in front of it is not opened (and says so at once); one without opens.
    let package = oiv(dir.path(), "plain.oiv", "", &[], true);
    assert!(Package::open(&package).is_ok());
    let stubbed = dir.path().join("stubbed.oiv");
    std::fs::write(&stubbed, [b"#!/bin/sh\n".as_slice(), &std::fs::read(&package).unwrap()].concat()).unwrap();
    assert!(Package::open(&stubbed).is_err());
}

// ---- OIV-01: archives saved after the loose files are final ----

#[test]
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

/// A file deleted, then its name made a folder by a later `<add>`: the package is refused
/// while it is planned, with the file where it was, and the error says so.
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

// ---- DLC-05: the packs and the list are one transaction ----

/// `dir/src/<name>/dlc.rpf` for each name, to install under that name.
fn dlc_sources(dir: &Path, names: &[&str]) -> Vec<(PathBuf, String)> {
    names
        .iter()
        .map(|name| {
            let path = dir.join(format!("src/{name}/dlc.rpf"));
            dlc_pack(&path, name);
            (path, name.to_string())
        })
        .collect()
}

fn install_packs(game: &Game, packs: &[(PathBuf, String)]) -> Result<Vec<String>, Error> {
    crate::addon::install_packs(game, None, packs, None)
}

/// The identity of a file: a rewrite or a replacement shows in it.
fn identity(path: &Path) -> (u64, u64, i64, i64) {
    let m = std::fs::metadata(path).unwrap();
    (m.dev(), m.ino(), m.mtime(), m.mtime_nsec())
}

#[test]
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
    let err = crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).unwrap_err();
    assert!(honest(&err) && matches!(err, Error::Txn(_)), "{err}");
    assert!(!crate::addon::pack_path(&game, "piccolo").exists(), "dlc.rpf was left installed but not listed");
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
    assert!(journals(&game).is_empty());
}

#[test]
fn dlc05_corrupt_update_rpf_leaves_no_pack() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    std::fs::write(game.root.join("update/update.rpf"), b"garbage that is not an archive").unwrap();
    let before = snapshot(&game.root);
    let err = crate::addon::install_pack(&game, None, &dir.path().join("dlc.rpf"), "piccolo", None).unwrap_err();
    assert!(honest(&err), "{err}");
    assert!(!crate::addon::pack_path(&game, "piccolo").exists(), "dlc.rpf was left installed but not listed");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty(), "{:?}", leftovers(&game.root));
    assert!(journals(&game).is_empty());
}

#[test]
fn dlc05_a_missing_update_rpf_is_refused_before_anything_is_staged() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    std::fs::remove_file(game.root.join("update/update.rpf")).unwrap();
    let before = snapshot(&game.root);
    let err = install_packs(&game, &packs).unwrap_err();
    assert!(honest(&err) && err.to_string().contains("update/update.rpf not found"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(!game.root.join("mods").exists(), "a folder was made for nothing");
    assert!(journals(&game).is_empty());
}

#[test]
fn dlc05_registered_pack_installs_again_without_touching_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // A pack of a mod, listed in the mods copy of the list (by an earlier install, or by hand).
    update_rpf(&game.root.join("mods/update/update.rpf"), &["mpheist", "piccolo"]);
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    let original = snapshot(&game.root.join("update"));
    let list = identity(&game.root.join("mods/update/update.rpf"));
    let log = install_packs(&game, &packs).unwrap();
    assert!(log[0].starts_with("installed "), "{log:?}");
    assert_eq!(log[1], "piccolo is already in the DLC list");
    assert_eq!(identity(&game.root.join("mods/update/update.rpf")), list, "a registered pack rewrote the list");
    // Again: the pack is the same, byte for byte, and nothing is written (not even a replacement by the same bytes).
    let (before, pack) = (snapshot(&game.root), identity(&crate::addon::pack_path(&game, "piccolo")));
    let log = install_packs(&game, &packs).unwrap();
    assert_eq!(log, ["piccolo: already installed (identical)", "piccolo is already in the DLC list"]);
    assert_unchanged(&before, &snapshot(&game.root));
    assert_eq!(identity(&crate::addon::pack_path(&game, "piccolo")), pack, "the same pack was written again");
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(journals(&game).is_empty() && leftovers(&game.root).is_empty());
}

#[test]
fn dlc05_a_reinstall_of_unlisted_identical_pack_stages_only_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    // The pack is in the mods folder already but not listed (what a crash between the two renames used to leave).
    let dest = crate::addon::pack_path(&game, "piccolo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&packs[0].0, &dest).unwrap();
    let pack = identity(&dest);
    let log = install_packs(&game, &packs).unwrap();
    assert_eq!(log[0], "piccolo: already installed (identical)");
    assert!(log[1].starts_with("added piccolo to the DLC list of "), "{log:?}");
    assert_eq!(identity(&dest), pack);
    assert!(list_of(&game.root.join("mods/update/update.rpf")).contains("<Item>dlcpacks:/piccolo/</Item>"));
}

#[test]
fn dlc05_a_pack_of_the_same_size_but_other_bytes_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let (a, b) = (dlc_sources(dir.path(), &["piccolo"]), dlc_sources(dir.path(), &["piccolx"]));
    let (bytes_a, bytes_b) = (std::fs::read(&a[0].0).unwrap(), std::fs::read(&b[0].0).unwrap());
    assert!(bytes_a != bytes_b && bytes_a.len() == bytes_b.len(), "the fixture must differ in content only");
    install_packs(&game, &a).unwrap();
    let log = install_packs(&game, &[(b[0].0.clone(), "piccolo".into())]).unwrap();
    assert!(log[0].starts_with("installed "), "{log:?}");
    assert_eq!(std::fs::read(crate::addon::pack_path(&game, "piccolo")).unwrap(), bytes_b);
}

#[test]
fn dlc05_two_packs_one_of_them_already_listed_are_one_install_and_the_original_list_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // `extra` is listed by the mods copy already (not by the game), `piccolo` is not listed.
    update_rpf(&game.root.join("mods/update/update.rpf"), &["mpheist", "extra"]);
    let packs = dlc_sources(dir.path(), &["extra", "piccolo"]);
    let (original_path, original_bytes) = (game.root.join("update/update.rpf"), std::fs::read(game.root.join("update/update.rpf")).unwrap());
    let original = identity(&original_path);
    let log = install_packs(&game, &packs).unwrap();
    assert!(log[0].starts_with("installed ") && log[1] == "extra is already in the DLC list", "{log:?}");
    assert!(log[2].starts_with("installed ") && log[3].starts_with("added piccolo to the DLC list of "), "{log:?}");
    assert_eq!(log.len(), 4, "{log:?}");
    let list = list_of(&game.root.join("mods/update/update.rpf"));
    assert_eq!((list.matches("extra").count(), list.matches("piccolo").count()), (1, 1), "{list}");
    for name in ["extra", "piccolo"] {
        assert_eq!(std::fs::read(crate::addon::pack_path(&game, name)).unwrap(), std::fs::read(dir.path().join(format!("src/{name}/dlc.rpf"))).unwrap());
    }
    assert_eq!(std::fs::read(&original_path).unwrap(), original_bytes, "the game's update.rpf changed");
    assert_eq!(identity(&original_path), original, "the game's update.rpf was replaced or touched");
    assert!(journals(&game).is_empty() && leftovers(&game.root).is_empty());
}

#[test]
fn dlc05_a_name_in_the_games_own_list_is_refused_with_nothing_changed() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // `mpheist` is in the game's list: a pack of that name in mods/ would take the place of the game's.
    let packs = dlc_sources(dir.path(), &["piccolo", "mpheist"]);
    let before = snapshot(&game.root);
    let err = install_packs(&game, &packs).unwrap_err();
    assert!(honest(&err) && matches!(err, Error::Txn(_)) && err.to_string().contains("mpheist is the name of one of the game's own DLC packs"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    // Also once the mods folder has a list of its own (which names it too, as a copy of the game's does).
    update_rpf(&game.root.join("mods/update/update.rpf"), &["mpheist"]);
    let before = snapshot(&game.root);
    assert!(install_packs(&game, &packs).unwrap_err().to_string().contains("game's own DLC"));
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(journals(&game).is_empty() && leftovers(&game.root).is_empty());
}

/// A file that looks like an encrypted `dlc.rpf` and has `count` entries (16 bytes each, all zero), no names.
fn encrypted_pack(path: &Path, count: u32) {
    let mut bytes = [&0x5250_4637u32.to_le_bytes()[..], &count.to_le_bytes(), &0u32.to_le_bytes(), &0x0FFF_FFF9u32.to_le_bytes()].concat();
    bytes.extend(vec![0u8; count as usize * 16]);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn dlc05_without_keys_an_encrypted_pack_needs_at_least_a_table_of_contents_that_fits() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let (empty, body) = (dir.path().join("src/empty/dlc.rpf"), dir.path().join("src/body/dlc.rpf"));
    encrypted_pack(&empty, 0);
    encrypted_pack(&body, 3);
    let before = snapshot(&game.root);
    let err = install_packs(&game, &[(empty, "empty".into())]).unwrap_err();
    assert!(honest(&err) && err.to_string().contains("empty table of contents"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    // The table of contents past the end of the file is refused by the opening itself.
    let short = dir.path().join("src/short/dlc.rpf");
    encrypted_pack(&short, 3);
    let bytes = std::fs::read(&short).unwrap();
    std::fs::write(&short, &bytes[..bytes.len() - 1]).unwrap();
    let err = install_packs(&game, &[(short, "short".into())]).unwrap_err();
    assert!(honest(&err) && err.to_string().contains("table of contents"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    // What cannot be read is let through when it is plausible: that is all that is known without the keys.
    install_packs(&game, &[(body, "body".into())]).unwrap();
}

#[test]
fn dlc05_a_pack_found_identical_is_looked_at_again_before_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    // Installed but not listed: the install stages the list only, and leaves the pack alone.
    let dest = crate::addon::pack_path(&game, "piccolo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&packs[0].0, &dest).unwrap();
    let before = snapshot(&game.root);
    // Somebody writes the pack, with the same size, after it was compared.
    let target = dest.clone();
    crate::addon::BEFORE_COMMIT.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            let mut bytes = std::fs::read(&target).unwrap();
            bytes[20] ^= 0xFF;
            std::fs::write(&target, bytes).unwrap();
        }))
    });
    let err = install_packs(&game, &packs).unwrap_err();
    let Error::Txn(failure) = &err else { panic!("{err:?}") };
    assert!(matches!(failure.cause, Error::Conflict(_)) && failure.undone, "{err}");
    assert!(!game.root.join("mods/update/update.rpf").exists(), "the list was put in place for a pack that is not there");
    // Nothing but the pack's own change is different.
    let mut after = snapshot(&game.root);
    assert!(after.remove("mods/update/x64/dlcpacks/piccolo/dlc.rpf") != before.get("mods/update/x64/dlcpacks/piccolo/dlc.rpf").cloned());
    let mut before = before;
    before.remove("mods/update/x64/dlcpacks/piccolo/dlc.rpf");
    assert_unchanged(&before, &after);
    assert!(journals(&game).is_empty());
}

#[test]
fn dlc05_a_batch_with_a_bad_or_repeated_name_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let mut packs = dlc_sources(dir.path(), &["piccolo", "extra"]);
    let before = snapshot(&game.root);
    packs[1].1 = "my car".into();
    assert!(matches!(install_packs(&game, &packs), Err(Error::Package(_))));
    packs[1].1 = "PICCOLO".into();
    let err = install_packs(&game, &packs).unwrap_err();
    assert!(matches!(err, Error::Package(_)) && err.to_string().contains("more than one"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(!game.root.join(".paleto").exists(), "a transaction began for a batch that was refused");
}

#[test]
fn dlc05_a_pack_that_is_not_an_archive_is_refused_with_a_clean_undo() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let mut packs = dlc_sources(dir.path(), &["piccolo", "extra"]);
    std::fs::write(&packs[1].0, b"not an archive at all").unwrap();
    packs.reverse();
    let before = snapshot(&game.root);
    let err = install_packs(&game, &packs).unwrap_err();
    assert!(honest(&err) && matches!(err, Error::Txn(_)) && err.to_string().contains("does not open as an archive"), "{err}");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(journals(&game).is_empty() && leftovers(&game.root).is_empty());
}

#[test]
fn dlc05_a_crash_between_the_two_renames_leaves_a_pack_never_a_listed_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    let old = state(&game);
    // The 1st rename puts the pack in place, the 2nd the list.
    faults::arm_at(Point::Rename, 2, Fault::Crash);
    assert!(install_packs(&game, &packs).is_err());
    assert!(faults::disarm());
    assert!(crate::addon::pack_path(&game, "piccolo").is_file(), "the pack is put in place before the list");
    assert!(!game.root.join("mods/update/update.rpf").exists(), "the list was put in place before the pack");
    assert_eq!(journals(&game).len(), 1);
    // The next change undoes it: the folders are as they were.
    let report = crate::txn::recover(&game).unwrap();
    assert!(report.failures.is_empty(), "{report:?}");
    assert_same("after recovery", &old, &without_temps(state(&game)));
    assert_clean(&game, "after recovery");
    // And the install works.
    install_packs(&game, &packs).unwrap();
    assert!(list_of(&game.root.join("mods/update/update.rpf")).contains("piccolo"));
}

/// Two packs, one new in the list, in a fresh game folder (with `old` given, the first already installed from other bytes).
fn pack_setup(dir: &Path, old: bool) -> (Game, Vec<(PathBuf, String)>) {
    let game = game_folder(dir, Edition::Enhanced);
    let packs = dlc_sources(dir, &["piccolo", "extra"]);
    if old {
        let older = dir.join("older/dlc.rpf");
        dlc_pack(&older, "older-pack");
        crate::addon::install_pack(&game, None, &older, "piccolo", None).unwrap();
    }
    (game, packs)
}

/// After any fault the next install works and gives what an unfaulted one gives.
fn packs_check(new: &std::collections::BTreeMap<String, Entry>) -> impl Fn(&Game, &Vec<(PathBuf, String)>, &str, bool) + '_ {
    move |game, packs, what, done| {
        let log = install_packs(game, packs).unwrap_or_else(|e| panic!("{what}: the next install failed: {e}"));
        if done {
            assert!(log[0].contains("already installed (identical)"), "{what}: {log:?}");
        }
        assert_same(&format!("{what}: the next install differs from an unfaulted one"), new, &state(game));
    }
}

fn reference(old: bool) -> std::collections::BTreeMap<String, Entry> {
    let dir = tempfile::tempdir().unwrap();
    let (game, packs) = pack_setup(dir.path(), old);
    install_packs(&game, &packs).unwrap();
    state(&game)
}

#[test]
fn dlc05_every_failure_point_leaves_the_folders_as_they_were() {
    let new = reference(false);
    let n = exhaust(Fault::Fail(std::io::ErrorKind::Other), |dir| pack_setup(dir, false), |game, packs| install_packs(game, packs), packs_check(&new));
    assert!(n > 30, "only {n} fault points");
}

#[test]
fn dlc05_every_crash_point_is_recovered() {
    let new = reference(false);
    exhaust(Fault::Crash, |dir| pack_setup(dir, false), |game, packs| install_packs(game, packs), packs_check(&new));
}

#[test]
fn dlc05_a_replaced_pack_is_restored_whatever_fails_after_it() {
    let new = reference(true);
    let n = exhaust(Fault::Fail(std::io::ErrorKind::Other), |dir| pack_setup(dir, true), |game, packs| install_packs(game, packs), packs_check(&new));
    assert!(n > 30, "only {n} fault points");
    exhaust(Fault::Crash, |dir| pack_setup(dir, true), |game, packs| install_packs(game, packs), packs_check(&new));
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
    // `.paleto-save-<6 random characters>` is what a killed `ArchiveEditor::save` leaves beside the archive.
    for rel in ["mods/x64/.paleto-0123456789abcdef-0.old", "mods/x64/.paleto-save-aB3x9Q", "mods_snapshots/.paleto-fedcba9876543210.partial", ".paleto-aaaaaaaaaaaaaaaa.new"] {
        std::fs::write(game.root.join(rel), b"orphan").unwrap();
    }
    let before = snapshot(&game.root);
    let found: Vec<String> = crate::txn::leftovers(&game).iter().map(|p| p.strip_prefix(&game.root).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, [".paleto-aaaaaaaaaaaaaaaa.new", "mods/x64/.paleto-0123456789abcdef-0.old", "mods/x64/.paleto-save-aB3x9Q", "mods_snapshots/.paleto-fedcba9876543210.partial"]);
    assert_unchanged(&before, &snapshot(&game.root));
    // They are not mods: the listings of the mods folder skip them.
    assert!(crate::loader::modded_files(&game, None).is_empty());
    assert!(crate::vfs::local_files(&game.root.join("mods")).unwrap().is_empty());
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\a.asi</add>"#, &[("content/x", b"new")], false);
    install(&game, &package).unwrap();
    assert_eq!(crate::txn::leftovers(&game).len(), 4);
}

#[test]
fn a_rollback_that_cannot_finish_says_so_and_names_the_backup() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Legacy);
    std::fs::write(game.root.join("scripts/x"), b"precious").unwrap();
    let package = oiv(dir.path(), "x.oiv", r#"<delete>scripts\x</delete><add source="x">scripts\z.asi</add>"#, &[("content/x", b"new")], false);
    // The commit fails at its second file; when it is undone, something else has put a file where the deleted one was, so the original cannot go back.
    let user_file = game.root.join("scripts/x");
    let planted = user_file.clone();
    crate::txn::ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || std::fs::write(&planted, b"user").unwrap())));
    crate::fsroot::faults::arm_at(crate::fsroot::faults::Point::Rename, 2, crate::fsroot::faults::Mode::Fail(std::io::ErrorKind::Other));
    let err = install(&game, &package).unwrap_err();
    assert!(crate::fsroot::faults::disarm());
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

// ---- the snapshot copy goes through open folders ----

/// A game whose mods folder holds `x64/m.rpf`, `sub/deep/a.txt` and a link, and a snapshot of it; then `x64/m.rpf` changes.
fn restore_setup(dir: &Path) -> (Game, crate::snapshots::Snapshot) {
    let game = game_folder(dir, Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
    std::fs::create_dir_all(game.root.join("mods/sub/deep")).unwrap();
    std::fs::write(game.root.join("mods/x64/m.rpf"), b"snapshotted").unwrap();
    std::fs::write(game.root.join("mods/sub/deep/a.txt"), b"deep").unwrap();
    symlink("x64/m.rpf", game.root.join("mods/sub/link")).unwrap();
    let snap = crate::snapshots::create(&game, "s").unwrap();
    std::fs::write(game.root.join("mods/x64/m.rpf"), b"current").unwrap();
    (game, snap)
}

type Tree = std::collections::BTreeMap<String, Entry>;

/// The mods folder and what is in it.
fn mods_tree(game: &Game) -> Tree {
    snapshot(&game.root).into_iter().filter(|(k, _)| k == "mods" || k.starts_with("mods/")).collect()
}

/// A snapshot's copy of the mods folder, named as the mods folder is in `mods_tree`.
fn saved_tree(snap: &crate::snapshots::Snapshot) -> Tree {
    snapshot(&snap.dir.join("mods")).into_iter().map(|(k, v)| (format!("mods/{k}"), v)).chain([("mods".to_string(), Entry::Dir)]).collect()
}

/// The folder a restore is filling, found by its name.
fn incoming(root: &Path) -> PathBuf {
    let found = std::fs::read_dir(root).unwrap().flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with("-mods.new"));
    found.expect("a restore is filling a folder")
}

/// Runs `hook` with the place of the folder `x64` the first time a copy reaches it.
fn when_copy_reaches_x64(hook: impl FnOnce(&Path) + 'static) {
    let mut hook = Some(hook);
    crate::snapshots::DURING_COPY.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |at: &Path| {
            if at.file_name().is_some_and(|n| n == "x64")
                && let Some(hook) = hook.take()
            {
                hook(at)
            }
        }))
    });
}

fn assert_nothing_left(game: &Game) {
    assert!(crate::txn::pending(game).is_empty() && crate::txn::leftovers(game).is_empty(), "{:?} {:?}", crate::txn::pending(game), crate::txn::leftovers(game));
}

#[test]
fn a_restore_never_writes_through_a_folder_swapped_for_a_link_to_the_originals() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let (originals, mods) = (snapshot(&game.root.join("x64")), mods_tree(&game));
    let root = game.root.clone();
    // While `x64` of the new folder is being filled, it is put aside and a link to the game's own `x64` takes its name.
    when_copy_reaches_x64(move |_| {
        let at = incoming(&root).join("x64");
        let _ = std::fs::rename(&at, at.with_extension("aside"));
        symlink(root.join("x64"), &at).unwrap();
    });
    let result = crate::snapshots::restore(&game, &snap.id);
    crate::snapshots::DURING_COPY.with(|h| h.take());
    assert_unchanged(&originals, &snapshot(&game.root.join("x64")));
    assert!(result.is_err(), "the restore went on over a folder that was swapped");
    assert_unchanged(&mods, &mods_tree(&game));
    // What was found moved into the new folder is kept, with the folder, for the user to look at.
    assert!(crate::txn::pending(&game).is_empty());
}

#[test]
fn a_restore_refuses_a_snapshot_folder_swapped_for_a_link_outside() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let (before_outside, mods) = (snapshot(&outside), mods_tree(&game));
    // After the copy has seen `x64` as a folder, it is replaced by a link to a folder outside the game.
    let target = outside.clone();
    when_copy_reaches_x64(move |at| {
        std::fs::remove_dir_all(at).unwrap();
        symlink(target, at).unwrap();
    });
    let result = crate::snapshots::restore(&game, &snap.id);
    crate::snapshots::DURING_COPY.with(|h| h.take());
    assert!(result.is_err(), "the copy followed a link to a folder outside the game");
    assert_unchanged(&before_outside, &snapshot(&outside));
    assert_unchanged(&mods, &mods_tree(&game));
    assert_nothing_left(&game);
}

#[test]
fn a_folder_put_in_place_of_the_incoming_one_never_becomes_mods() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let mods = mods_tree(&game);
    let root = game.root.clone();
    // Another folder takes the name of the one being filled, with a file of its own.
    when_copy_reaches_x64(move |_| {
        let at = incoming(&root);
        std::fs::rename(&at, at.with_extension("aside")).unwrap();
        std::fs::create_dir(&at).unwrap();
        std::fs::write(at.join("evil.txt"), b"evil").unwrap();
    });
    let result = crate::snapshots::restore(&game, &snap.id);
    crate::snapshots::DURING_COPY.with(|h| h.take());
    assert!(result.is_err() && !game.root.join("mods/evil.txt").exists(), "a folder that is not the one the restore made became the mods folder");
    assert_unchanged(&mods, &mods_tree(&game));
    assert!(crate::txn::pending(&game).is_empty());
}

#[test]
fn links_in_the_mods_folder_come_back_as_links() {
    let dir = tempfile::tempdir().unwrap();
    let (game, _) = restore_setup(dir.path());
    symlink("/somewhere/else/entirely", game.root.join("mods/sub/absolute")).unwrap();
    symlink("nowhere", game.root.join("mods/dangling")).unwrap();
    symlink("../x64", game.root.join("mods/sub/up")).unwrap();
    let want = mods_tree(&game);
    let snap = crate::snapshots::create(&game, "links").unwrap();
    assert_unchanged(&want, &saved_tree(&snap));
    std::fs::remove_dir_all(game.root.join("mods/sub")).unwrap();
    std::fs::remove_file(game.root.join("mods/dangling")).unwrap();
    crate::snapshots::restore(&game, &snap.id).unwrap();
    assert_unchanged(&want, &mods_tree(&game));
    assert_nothing_left(&game);
}

/// Runs `f` on another thread and fails if it has not finished in time; `fifo` is opened to let a copy stuck on it go.
fn finishes<T: Send + 'static>(fifo: &Path, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || tx.send(f()));
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(value) => {
            handle.join().unwrap().unwrap();
            value
        }
        Err(_) => {
            let _release = std::fs::OpenOptions::new().read(true).write(true).open(fifo);
            panic!("the copy is stuck on a pipe");
        }
    }
}

fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
}

#[test]
fn a_pipe_a_linked_snapshot_folder_and_a_name_that_is_not_utf8_are_refused_at_once() {
    use std::os::unix::ffi::OsStrExt;
    let odd = || std::ffi::OsStr::from_bytes(b"bad\xff.rpf").to_owned();
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let mods = mods_tree(&game);
    // A pipe in the mods folder: a snapshot cannot be taken, and does not wait for a writer.
    let fifo = game.root.join("mods/sub/pipe");
    make_fifo(&fifo);
    let g = game.clone();
    let err = finishes(&fifo, move || crate::snapshots::create(&g, "pipe").unwrap_err().to_string());
    assert!(err.contains("not a file, folder or link"), "{err}");
    // (The folder scan of the helpers would read the pipe too.)
    std::fs::remove_file(&fifo).unwrap();
    assert_eq!(crate::snapshots::list(&game).len(), 1);
    assert_nothing_left(&game);

    // A pipe inside a snapshot: the restore stops and the mods folder stays.
    let fifo = snap.dir.join("mods/sub/pipe");
    make_fifo(&fifo);
    let (g, id) = (game.clone(), snap.id.clone());
    let err = finishes(&fifo, move || crate::snapshots::restore(&g, &id).unwrap_err().to_string());
    assert!(err.contains("not a file, folder or link"), "{err}");
    std::fs::remove_file(&fifo).unwrap();
    assert_unchanged(&mods, &mods_tree(&game));
    assert_nothing_left(&game);

    // A name that is not UTF-8, in the snapshot and in the mods folder.
    std::fs::write(snap.dir.join("mods").join(odd()), b"odd").unwrap();
    let err = crate::snapshots::restore(&game, &snap.id).unwrap_err().to_string();
    assert!(err.contains("UTF-8"), "{err}");
    assert_unchanged(&mods, &mods_tree(&game));
    assert_nothing_left(&game);
    std::fs::remove_file(snap.dir.join("mods").join(odd())).unwrap();
    std::fs::write(game.root.join("mods").join(odd()), b"odd").unwrap();
    let err = crate::snapshots::create(&game, "odd").unwrap_err().to_string();
    assert!(err.contains("UTF-8"), "{err}");
    assert_eq!(crate::snapshots::list(&game).len(), 1);
    assert_nothing_left(&game);
    std::fs::remove_file(game.root.join("mods").join(odd())).unwrap();

    // The snapshot's `mods` is a link: it is not followed.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("x64")).unwrap();
    std::fs::write(elsewhere.join("x64/m.rpf"), b"elsewhere").unwrap();
    std::fs::remove_dir_all(snap.dir.join("mods")).unwrap();
    symlink(&elsewhere, snap.dir.join("mods")).unwrap();
    assert!(crate::snapshots::restore(&game, &snap.id).is_err(), "a link in place of the snapshot's mods folder was followed");
    assert_unchanged(&mods, &mods_tree(&game));
    assert_nothing_left(&game);
}

#[test]
fn a_restore_is_whole_or_not_at_all_at_every_fault_point() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        for point in [Point::Create, Point::Mkdir, Point::SyncFile, Point::Rename, Point::Sync] {
            let mut reached = 0;
            for k in 1..300 {
                let dir = tempfile::tempdir().unwrap();
                let (game, snap) = restore_setup(dir.path());
                let (old, new) = (mods_tree(&game), saved_tree(&snap));
                faults::arm_at(point, k, mode);
                let result = crate::snapshots::restore(&game, &snap.id);
                if !faults::disarm() {
                    result.unwrap();
                    assert_same(&format!("{point:?} {mode:?} {k}: no fault"), &new, &mods_tree(&game));
                    break;
                }
                reached = k;
                let what = format!("{point:?} {mode:?} at {k}");
                if matches!(mode, Fault::Crash) {
                    let report = crate::txn::recover(&game).unwrap();
                    assert!(report.failures.is_empty(), "{what}: {report:?}");
                    let now = mods_tree(&game);
                    assert!(now == old || now == new, "{what}: the mods folder is neither as it was nor the snapshot");
                } else {
                    match result {
                        Ok(()) => assert_same(&what, &new, &mods_tree(&game)),
                        // The swap is in place but not confirmed on disk: the next change finishes it.
                        Err(e) if e.to_string().contains("not yet confirmed") => assert_same(&what, &new, &mods_tree(&game)),
                        Err(_) => assert_same(&what, &old, &mods_tree(&game)),
                    }
                    crate::txn::recover(&game).unwrap();
                }
                assert_nothing_left(&game);
            }
            assert!(reached > 0, "{point:?} {mode:?}: no call of this kind was reached");
        }
    }
}

#[test]
fn a_snapshot_is_whole_or_not_taken_at_every_fault_point() {
    for point in [Point::Create, Point::Mkdir, Point::SyncFile, Point::Rename, Point::Sync] {
        for k in 1..300 {
            let dir = tempfile::tempdir().unwrap();
            let (game, _) = restore_setup(dir.path());
            let want = mods_tree(&game);
            faults::arm_at(point, k, Fault::Fail(std::io::ErrorKind::Other));
            let result = crate::snapshots::create(&game, "second");
            if !faults::disarm() {
                result.unwrap();
                break;
            }
            let what = format!("{point:?} at {k}");
            assert_unchanged(&want, &mods_tree(&game));
            let listed = crate::snapshots::list(&game);
            match result {
                Ok(_) => assert_eq!(listed.len(), 2, "{what}"),
                Err(_) => assert_eq!(listed.len(), 1, "{what}"),
            }
            assert!(listed.iter().all(|s| s.dir.join("mods/sub/deep/a.txt").is_file()), "{what}: a snapshot that lists is incomplete");
            assert_nothing_left(&game);
        }
    }
}

// ---- names that differ only in case ----

/// A game with `scripts/Foo` and `scripts/foo`, which the game's own (case-insensitive) filesystem would not have.
fn case_game(dir: &Path) -> Game {
    let game = game_folder(dir, Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("scripts/Foo")).unwrap();
    std::fs::create_dir_all(game.root.join("scripts/foo")).unwrap();
    game
}

fn warnings(log: &[String]) -> Vec<&String> {
    log.iter().filter(|l| l.starts_with("warning:")).collect()
}

#[test]
fn an_install_into_a_name_with_two_spellings_warns_once_and_uses_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let game = case_game(dir.path());
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\FOO\a.asi</add><add source="x">scripts\FOO\b.asi</add>"#, &[("content/x", b"new")], false);
    let log = install(&game, &package).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/Foo/a.asi")).unwrap(), b"new");
    assert!(!game.root.join("scripts/foo/a.asi").exists());
    let found = warnings(&log);
    assert_eq!(found.len(), 1, "{log:?}");
    assert!(found[0].contains("scripts") && found[0].contains("\"Foo\" and \"foo\"") && found[0].contains("differ only in case") && found[0].contains("\"Foo\" was used"), "{}", found[0]);
}

#[test]
fn an_exact_spelling_is_no_guess_and_warns_of_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = case_game(dir.path());
    for spelling in ["Foo", "foo"] {
        let package = oiv(dir.path(), "x.oiv", &format!(r#"<add source="x">scripts\{spelling}\a.asi</add>"#), &[("content/x", b"new")], false);
        let log = install(&game, &package).unwrap();
        assert!(warnings(&log).is_empty(), "{spelling}: {log:?}");
        assert!(game.root.join("scripts").join(spelling).join("a.asi").is_file());
    }
}

#[test]
fn a_failed_install_reports_the_names_it_guessed_too() {
    let dir = tempfile::tempdir().unwrap();
    let game = case_game(dir.path());
    // The second command cannot be applied, after the first resolved a name that has two spellings.
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\FOO\a.asi</add><xml path="update\missing.xml"><remove xpath="/a" /></xml>"#, &[("content/x", b"new")], false);
    let before = snapshot(&game.root);
    let Error::Txn(failure) = install(&game, &package).unwrap_err() else { panic!("not a transaction failure") };
    assert!(failure.log.iter().any(|l| l.starts_with("warning:") && l.contains("differ only in case")), "{:?}", failure.log);
    assert_unchanged(&before, &snapshot(&game.root));
}

#[test]
fn a_pack_into_a_name_with_two_spellings_warns() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    for name in ["Piccolo", "PICCOLO"] {
        std::fs::create_dir_all(game.root.join("mods/update/x64/dlcpacks").join(name)).unwrap();
    }
    let packs = dlc_sources(dir.path(), &["piccolo"]);
    let log = install_packs(&game, &packs).unwrap();
    let found = warnings(&log);
    assert_eq!(found.len(), 1, "{log:?}");
    assert!(found[0].contains("dlcpacks") && found[0].contains("\"PICCOLO\" and \"Piccolo\""), "{}", found[0]);
    assert!(game.root.join("mods/update/x64/dlcpacks/PICCOLO/dlc.rpf").is_file());
}

#[test]
fn an_extraction_into_a_name_with_two_spellings_warns() {
    let dir = tempfile::tempdir().unwrap();
    let game = case_game(dir.path());
    let (_, log) = crate::vfs::write_into(&game.root.join("scripts/FOO"), "extracting", |o| o.write("a.txt", b"x").map(drop)).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/Foo/a.txt")).unwrap(), b"x");
    assert_eq!(warnings(&log).len(), 1, "{log:?}");
}

// ---- follow-ups of the review of the snapshot copy ----

#[test]
fn a_foreign_folder_at_the_incoming_name_is_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let (originals, mods) = (snapshot(&game.root.join("x64")), mods_tree(&game));
    let root = game.root.clone();
    // The game's own `x64` is moved onto the name of the folder being filled.
    when_copy_reaches_x64(move |_| {
        let at = incoming(&root);
        std::fs::rename(&at, at.with_extension("aside")).unwrap();
        std::fs::rename(root.join("x64"), &at).unwrap();
    });
    let result = crate::snapshots::restore(&game, &snap.id);
    crate::snapshots::DURING_COPY.with(|h| h.take());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("left in place"), "{err}");
    assert_unchanged(&originals, &snapshot(&incoming(&game.root)));
    assert_unchanged(&mods, &mods_tree(&game));
    assert!(crate::txn::pending(&game).is_empty());
}

#[test]
fn a_foreign_folder_moved_into_the_incoming_one_is_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let (originals, mods) = (snapshot(&game.root.join("x64")), mods_tree(&game));
    let root = game.root.clone();
    // The game's own `x64` takes the place of the subfolder being filled.
    when_copy_reaches_x64(move |_| {
        let at = incoming(&root).join("x64");
        std::fs::rename(&at, at.with_extension("aside")).unwrap();
        std::fs::rename(root.join("x64"), &at).unwrap();
    });
    let result = crate::snapshots::restore(&game, &snap.id);
    crate::snapshots::DURING_COPY.with(|h| h.take());
    assert!(result.is_err());
    assert_unchanged(&originals, &snapshot(&incoming(&game.root).join("x64")));
    assert_unchanged(&mods, &mods_tree(&game));
    assert!(crate::txn::pending(&game).is_empty());
}

#[test]
fn a_snapshot_of_a_mods_link_into_the_games_data_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    symlink("x64", game.root.join("mods")).unwrap();
    assert!(crate::snapshots::create(&game, "x").is_err(), "the game's own x64 was copied through a mods link");
    assert!(crate::snapshots::list(&game).is_empty());
    assert_nothing_left(&game);
}

#[test]
fn copied_folders_keep_the_permissions_of_their_source() {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let dir = tempfile::tempdir().unwrap();
    let (game, _) = restore_setup(dir.path());
    std::fs::set_permissions(game.root.join("mods/sub"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(game.root.join("mods/x64"), std::fs::Permissions::from_mode(0o750)).unwrap();
    let snap = crate::snapshots::create(&game, "modes").unwrap();
    assert_eq!((mode_of(snap.dir.join("mods/sub")), mode_of(snap.dir.join("mods/x64"))), (0o700, 0o750));
    std::fs::set_permissions(game.root.join("mods/sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::snapshots::restore(&game, &snap.id).unwrap();
    assert_eq!((mode_of(game.root.join("mods/sub")), mode_of(game.root.join("mods/x64"))), (0o700, 0o750));
}

#[test]
fn a_mods_link_planted_to_the_incoming_folder_does_not_make_recovery_remove_the_old_mods() {
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    // The restore stops between its two renames: `mods` is only under its aside name.
    faults::arm_at(Point::Rename, 2, Fault::Crash);
    assert!(crate::snapshots::restore(&game, &snap.id).is_err());
    assert!(faults::disarm());
    let named = |end: &str| std::fs::read_dir(&game.root).unwrap().flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().ends_with(end)).unwrap();
    let (new, old) = (named("-mods.new"), named("-mods.old"));
    assert!(!game.root.join("mods").exists());
    symlink(&new, game.root.join("mods")).unwrap();
    let _ = crate::txn::recover(&game).unwrap();
    assert_eq!(std::fs::read(game.root.join(&old).join("x64/m.rpf")).unwrap(), b"current", "the old mods folder was removed");
}

// ---- round 2 of the review of the snapshot copy ----

#[test]
fn a_kept_folder_is_never_removed_by_a_later_recovery_and_the_failures_are_told() {
    let (mut told, mut stuck) = (0, 0);
    for k in 1..400 {
        let dir = tempfile::tempdir().unwrap();
        let (game, snap) = restore_setup(dir.path());
        let originals = snapshot(&game.root.join("x64"));
        let root = game.root.clone();
        // The game's own `x64` is moved onto the name of the folder being filled; then a look at a name fails.
        when_copy_reaches_x64(move |_| {
            let at = incoming(&root);
            std::fs::rename(&at, at.with_extension("aside")).unwrap();
            std::fs::rename(root.join("x64"), &at).unwrap();
        });
        faults::arm_at(Point::Stat, k, Fault::Fail(std::io::ErrorKind::Other));
        let result = crate::snapshots::restore(&game, &snap.id);
        let fired = faults::disarm();
        crate::snapshots::DURING_COPY.with(|h| h.take());
        let left = !crate::txn::pending(&game).is_empty();
        if left {
            stuck += 1;
            let err = result.as_ref().unwrap_err().to_string();
            assert!(err.contains("journal") && err.contains("left in place"), "k={k}: the failures of the recovery are not told: {err}");
            told += 1;
        }
        // Whatever happened, the next change recovers, and the game's `x64` is still somewhere.
        let _ = crate::txn::recover(&game);
        let safe = snapshot(&game.root.join("x64")) == originals || std::fs::read_dir(&game.root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with("-mods.new") && snapshot(&e.path()) == originals);
        assert!(safe, "k={k}: the game's x64 was deleted by a recovery");
        if !fired {
            break;
        }
    }
    assert!(stuck > 0 && told == stuck, "no look failed during the recovery of a kept folder");
}

#[test]
fn the_folders_of_a_copy_are_private_while_it_is_filled() {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let dir = tempfile::tempdir().unwrap();
    let (game, snap) = restore_setup(dir.path());
    let seen = std::rc::Rc::new(std::cell::Cell::new(None));
    let (root, slot) = (game.root.clone(), seen.clone());
    when_copy_reaches_x64(move |_| {
        let at = incoming(&root);
        slot.set(Some((mode_of(at.clone()), mode_of(at.join("x64")))));
    });
    crate::snapshots::restore(&game, &snap.id).unwrap();
    crate::snapshots::DURING_COPY.with(|h| h.take());
    assert_eq!(seen.get(), Some((0o700, 0o700)), "a folder was open to the group and others while it was filled");
}
