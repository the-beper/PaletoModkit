//! Work package A4c-2: archive edits, conversions and their undo, the crash hunt and the file browser's
//! disk actions run as transactions of the engine. Every failure leaves the folders as they were, every crash is
//! recovered, originals are never written, and a link from `mods` back to the originals (or any link that leaves
//! the mods folder) is refused instead of followed.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use paleto_rpf::ArchiveEditor;

use crate::convert::{self, tests::fake_tool};
use crate::crash_hunt;
use crate::fixtures::*;
use crate::fsroot::faults::Mode as Fault;
use crate::game::{Edition, Game};
use crate::relpath::RelPath;
use crate::{ArchivePath, Error, vfs};

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).unwrap()
}

/// Device, inode and bytes of a file: what "never written" means.
fn identity(path: &Path) -> (u64, u64, Vec<u8>) {
    let m = std::fs::metadata(path).unwrap();
    (m.dev(), m.ino(), std::fs::read(path).unwrap())
}

fn add_new(path: &ArchivePath) -> Result<(), Error> {
    path.edit(None, |e, folder| Ok(e.add_file(&vfs::join_inner(folder, "new.txt"), b"new".to_vec())?))
}

fn has_new(archive: &Path) -> bool {
    ArchivePath::new(archive.to_path_buf()).join("new.txt").read(None).is_ok()
}

// ---- archive edits ----

#[test]
fn an_edit_of_an_original_makes_the_mods_copy_in_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let original = identity(&game.root.join("x64/a.rpf"));
    let path = ArchivePath::new(game.root.join("x64/a.rpf"));
    let target = path.editable(Some(&game)).unwrap();
    assert_eq!(target.disk, game.root.join("mods/x64/a.rpf"));
    assert!(!game.root.join("mods").exists(), "editable made a copy");
    add_new(&target).unwrap();
    assert!(has_new(&game.root.join("mods/x64/a.rpf")));
    assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
    // The next edit starts from the mods copy.
    target.edit(None, |e, folder| Ok(e.add_file(&vfs::join_inner(folder, "more.txt"), b"more".to_vec())?)).unwrap();
    let copy = ArchivePath::new(game.root.join("mods/x64/a.rpf"));
    assert!(copy.join("new.txt").read(None).is_ok() && copy.join("more.txt").read(None).is_ok() && copy.join("a.txt").read(None).is_ok());
    assert_clean(&game, "after the edits");
}

#[test]
fn a_failed_edit_makes_no_mods_copy() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let before = state(&game);
    let target = ArchivePath::new(game.root.join("x64/a.rpf")).editable(Some(&game)).unwrap();
    let e = target
        .edit(None, |e, _| {
            e.add_file("half.txt", b"half".to_vec())?;
            Err::<(), _>(Error::Package("the edit cannot be applied".into()))
        })
        .unwrap_err();
    assert!(e.to_string().contains("the edit cannot be applied") && e.to_string().ends_with("(nothing was changed)"), "{e}");
    assert_unchanged(&before, &state(&game));
    assert_clean(&game, "after the failed edit");
    // An archive that is nowhere is an error at once, not a copy of nothing.
    let missing = ArchivePath::new(game.root.join("x64/missing.rpf")).editable(Some(&game)).unwrap_err().to_string();
    assert!(missing.contains("is not in the game folder"), "{missing}");
}

#[test]
fn every_failure_and_crash_point_of_an_archive_edit() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust(
            mode,
            |dir| {
                let game = game_folder(dir, Edition::Enhanced);
                let original = identity(&game.root.join("x64/a.rpf"));
                (game, original)
            },
            |game, _| add_new(&ArchivePath::new(game.root.join("x64/a.rpf"))).map(|()| Vec::new()),
            |game, original, what, done| {
                assert_eq!(&identity(&game.root.join("x64/a.rpf")), original, "{what}: the original changed");
                let copy = game.root.join("mods/x64/a.rpf");
                // Each target is wholly old (no copy) or wholly new (a copy that opens, with the change).
                assert_eq!(copy.exists(), done, "{what}");
                if done {
                    assert!(has_new(&copy), "{what}");
                }
            },
        );
        assert!(points > 5, "{mode:?}: only {points} fault points");
    }
}

/// A game whose `mods/x64` is a link back to the originals, the way an earlier layout or a mistake can make one.
fn game_with_a_link_to_the_originals(dir: &Path, absolute: bool) -> Game {
    let game = game_folder(dir, Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    let target = if absolute { game.root.join("x64") } else { PathBuf::from("../x64") };
    std::os::unix::fs::symlink(target, game.root.join("mods/x64")).unwrap();
    game
}

#[test]
fn an_edit_through_a_link_back_to_the_originals_never_writes_them() {
    for absolute in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let game = game_with_a_link_to_the_originals(dir.path(), absolute);
        let original = identity(&game.root.join("x64/a.rpf"));
        let before = state(&game);
        for disk in [game.root.join("mods/x64/a.rpf"), game.root.join("x64/a.rpf")] {
            let result = ArchivePath::new(disk.clone()).editable(Some(&game)).and_then(|t| add_new(&t));
            assert!(result.is_err(), "an edit through the link was done ({absolute}, {disk:?})");
            assert_eq!(identity(&game.root.join("x64/a.rpf")), original, "the original was written ({absolute}, {disk:?})");
        }
        // The edit of the original path itself would make its copy through the same link: refused too.
        assert!(!has_new(&game.root.join("x64/a.rpf")));
        assert_unchanged(&before, &state(&game));
        assert_clean(&game, "after the refused edits");
    }
}

#[test]
fn an_edit_of_a_hard_linked_mods_copy_leaves_the_original_inode_alone() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/x64")).unwrap();
    std::fs::hard_link(game.root.join("x64/a.rpf"), game.root.join("mods/x64/a.rpf")).unwrap();
    let original = identity(&game.root.join("x64/a.rpf"));
    add_new(&ArchivePath::new(game.root.join("mods/x64/a.rpf"))).unwrap();
    assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
    assert!(has_new(&game.root.join("mods/x64/a.rpf")));
    assert_ne!(std::fs::metadata(game.root.join("mods/x64/a.rpf")).unwrap().ino(), original.1);
}

#[test]
fn an_archive_outside_every_game_is_saved_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("loose.rpf");
    rpf(&archive, "loose.rpf", &[("a.txt", b"a")]);
    add_new(&ArchivePath::new(archive.clone())).unwrap();
    assert!(has_new(&archive));
}

#[test]
fn new_archives_are_made_through_the_engine_and_never_over_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let original = identity(&game.root.join("x64/a.rpf"));
    // A path the game streams is made in the mods folder; the original is not touched.
    assert_eq!(vfs::new_archive(&game, &rel("x64/new.rpf")).unwrap(), game.root.join("mods/x64/new.rpf"));
    assert!(ArchiveEditor::open(&game.root.join("mods/x64/new.rpf"), None).is_ok());
    assert_eq!(vfs::new_archive(&game, &rel("scripts/pack.rpf")).unwrap(), game.root.join("mods/scripts/pack.rpf"));
    let again = vfs::new_archive(&game, &rel("x64/new.rpf")).unwrap_err().to_string();
    assert!(again.contains("already exists"), "{again}");
    assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
    assert_clean(&game, "after new archives");
    // Through a link from mods back to the originals, nothing is made.
    let linked = tempfile::tempdir().unwrap();
    let game = game_with_a_link_to_the_originals(linked.path(), false);
    let before = state(&game);
    assert!(vfs::new_archive(&game, &rel("x64/new.rpf")).is_err());
    assert_unchanged(&before, &state(&game));
}

// ---- conversions ----

fn res(v: u32) -> Vec<u8> {
    [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat()
}

const PACK: &str = "update/x64/dlcpacks/car/dlc.rpf";
const STAMP: &str = "1700000000";

/// An Enhanced game with a mods copy of a pack holding a Legacy and an Enhanced vehicle; the converter is beside it.
fn game_with_a_pack(dir: &Path) -> (Game, PathBuf) {
    let game = game_folder(dir, Edition::Enhanced);
    let mut pack = ArchiveEditor::new("dlc.rpf");
    let vehicles = pack.archive("x64/vehicles.rpf", true, None).unwrap();
    vehicles.add_file("car.yft", res(162)).unwrap();
    vehicles.add_file("car_hi.yft", res(171)).unwrap();
    pack.add_file("x64/odd.ydr", res(7)).unwrap();
    let disk = game.root.join("mods").join(PACK);
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    pack.save(&disk).unwrap();
    (game, fake_tool(dir))
}

fn pack_paths() -> Vec<String> {
    ["x64/vehicles.rpf/car.yft", "x64/vehicles.rpf/car_hi.yft", "x64/odd.ydr"].iter().map(|p| format!("{PACK}/{p}")).collect()
}

fn car(game: &Game) -> Vec<u8> {
    ArchivePath::new(game.root.join("mods").join(PACK)).join("x64").join("vehicles.rpf").join("car.yft").read(None).unwrap()
}

#[test]
fn a_conversion_is_one_transaction_with_its_backups() {
    let dir = tempfile::tempdir().unwrap();
    let (game, tool) = game_with_a_pack(dir.path());
    let backup = game.root.join("mods_legacy").join(STAMP);
    let report = convert::convert_in_mods(&game, None, &tool, &pack_paths(), Some(&backup)).unwrap();
    assert_eq!(report.iter().filter(|r| r.error.is_none()).count(), 1);
    assert_eq!(car(&game), res(171));
    assert_eq!(std::fs::read(backup.join(&pack_paths()[0])).unwrap(), res(162));
    assert_clean(&game, "after the conversion");
    // The Legacy original that is kept is never overwritten by a later run.
    std::fs::write(backup.join(&pack_paths()[0]), b"kept").unwrap();
    let mut editor = ArchiveEditor::open(&game.root.join("mods").join(PACK), None).unwrap();
    editor.archive("x64/vehicles.rpf", false, None).unwrap().add_file("car.yft", res(162)).unwrap();
    editor.save(&game.root.join("mods").join(PACK)).unwrap();
    convert::convert_in_mods(&game, None, &tool, &pack_paths(), Some(&backup)).unwrap();
    assert_eq!(std::fs::read(backup.join(&pack_paths()[0])).unwrap(), b"kept");
}

#[test]
fn the_legacy_backups_go_only_to_mods_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let (game, tool) = game_with_a_pack(dir.path());
    let before = state(&game);
    for outside in [dir.path().join("backup"), game.root.join("scripts/backup"), game.root.join("mods/backup"), game.root.join("x64")] {
        let e = convert::convert_in_mods(&game, None, &tool, &pack_paths(), Some(&outside)).unwrap_err().to_string();
        assert!(e.contains("mods_legacy"), "{e}");
    }
    assert_eq!(car(&game), res(162));
    assert_unchanged(&before, &state(&game));
}

#[test]
fn a_failed_conversion_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (game, _) = game_with_a_pack(dir.path());
    let before = state(&game);
    // A converter that fails: nothing is staged, nothing is left.
    let failing = dir.path().join("failing");
    crate::write_script(&failing, "#!/bin/sh\necho oops >&2\nexit 1\n");
    let e = convert::convert_in_mods(&game, None, &failing, &pack_paths(), Some(&game.root.join("mods_legacy").join(STAMP))).unwrap_err().to_string();
    assert!(e.contains("oops") && e.ends_with("(nothing was changed)"), "{e}");
    assert_unchanged(&before, &state(&game));
    assert_clean(&game, "after the failed conversion");
}

#[test]
fn every_failure_and_crash_point_of_a_conversion() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust_pruning(
            mode,
            // A crash may leave the folders made for the backups empty (recovery cannot prove they are its own).
            &["mods_legacy"],
            |dir| {
                let (game, tool) = game_with_a_pack(dir);
                let original = identity(&game.root.join("x64/a.rpf"));
                (game, (tool, original))
            },
            |game, (tool, _)| convert::convert_in_mods(game, None, tool, &pack_paths(), Some(&game.root.join("mods_legacy").join(STAMP))).map(|_| Vec::new()),
            |game, (_, original), what, done| {
                assert_eq!(&identity(&game.root.join("x64/a.rpf")), original, "{what}: an original changed");
                // The archive and its backup are both new or both old.
                assert_eq!(car(game), if done { res(171) } else { res(162) }, "{what}");
                assert_eq!(game.root.join("mods_legacy").join(STAMP).join(&pack_paths()[0]).exists(), done, "{what}");
            },
        );
        assert!(points > 10, "{mode:?}: only {points} fault points");
    }
}

#[test]
fn undoing_a_conversion_is_all_or_nothing_and_leaves_no_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (game, tool) = game_with_a_pack(dir.path());
    convert::convert_in_mods(&game, None, &tool, &pack_paths(), Some(&game.root.join("mods_legacy").join(STAMP))).unwrap();
    // The mods copy is gone: nothing can be put back, and the backups stay.
    let archive = game.root.join("mods").join(PACK);
    let saved = std::fs::read(&archive).unwrap();
    std::fs::remove_file(&archive).unwrap();
    let before = state(&game);
    assert!(convert::undo(&game, None, STAMP).is_err());
    assert_unchanged(&before, &state(&game));
    std::fs::write(&archive, saved).unwrap();
    let report = convert::undo(&game, None, STAMP).unwrap();
    assert_eq!(report.len(), 1);
    assert!(report[0].error.is_none());
    assert_eq!(car(&game), res(162));
    assert!(!game.root.join("mods_legacy").join(STAMP).exists());
    assert_clean(&game, "after the undo");
    assert!(convert::undo(&game, None, STAMP).is_err());
    assert!(convert::undo(&game, None, "../x64").is_err());
}

#[test]
fn every_failure_and_crash_point_of_undoing_a_conversion() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust_pruning(
            mode,
            // The folders the backups were in are removed after the commit.
            &["mods_legacy"],
            |dir| {
                let (game, tool) = game_with_a_pack(dir);
                convert::convert_in_mods(&game, None, &tool, &pack_paths(), Some(&game.root.join("mods_legacy").join(STAMP))).unwrap();
                let original = identity(&game.root.join("x64/a.rpf"));
                (game, original)
            },
            |game, _| convert::undo(game, None, STAMP).map(|_| Vec::new()),
            |game, original, what, done| {
                assert_eq!(&identity(&game.root.join("x64/a.rpf")), original, "{what}: an original changed");
                // The Legacy file is back exactly when its backup is gone.
                assert_eq!(car(game), if done { res(162) } else { res(171) }, "{what}");
                assert_eq!(game.root.join("mods_legacy").join(STAMP).join(&pack_paths()[0]).exists(), !done, "{what}");
            },
        );
        assert!(points > 10, "{mode:?}: only {points} fault points");
    }
}

// ---- the crash hunt ----

/// A game whose mods copy of `update.rpf` holds five changes, with a hunt inside it under way.
fn game_in_a_hunt(dir: &Path) -> (Game, crash_hunt::Hunt) {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut update = ArchiveEditor::new("update.rpf");
    for f in ["a.meta", "b.meta", "c.meta", "gone.meta"] {
        update.add_file(&format!("common/data/{f}"), b"stock".to_vec()).unwrap();
    }
    update.archive("x64/levels.rpf", true, None).unwrap().add_file("map.ymap", b"stock".to_vec()).unwrap();
    update.save(&root.join("update/update.rpf")).unwrap();
    let mut update = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
    update.add_file("common/data/a.meta", b"mod a".to_vec()).unwrap();
    update.add_file("common/data/b.meta", b"mod b".to_vec()).unwrap();
    update.add_file("common/data/new.meta", b"new".to_vec()).unwrap();
    update.delete("common/data/gone.meta").unwrap();
    update.archive("x64/levels.rpf", false, None).unwrap().add_file("map.ymap", b"mod map".to_vec()).unwrap();
    update.save(&root.join("mods/update/update.rpf")).unwrap();
    let game = Game::open(&root).unwrap();
    let crash_hunt::Outcome::Test(hunt) = crash_hunt::start_inside(&game, None, "mods/update/update.rpf", &dir.join("changes.tsv")).unwrap() else { panic!("no test to run") };
    assert!(!hunt.disabled.is_empty());
    (game, hunt)
}

#[test]
fn every_failure_and_crash_point_of_a_hunt_step() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust_pruning(
            mode,
            &["mods_disabled"],
            |dir| {
                let (game, hunt) = game_in_a_hunt(dir);
                let original = identity(&game.root.join("update/update.rpf"));
                (game, (hunt, original))
            },
            // The game still crashed: the culprit is among the files that were on.
            |game, _| crash_hunt::answer(game, None, true).map(|_| Vec::new()),
            |game, (hunt, original), what, done| {
                assert_eq!(&identity(&game.root.join("update/update.rpf")), original, "{what}: the original changed");
                // The state of the hunt and the archive it describes change together.
                let now = crash_hunt::load(game).unwrap();
                assert_eq!(now == *hunt, !done, "{what}");
                for p in &now.disabled {
                    assert!(crash_hunt::turned_off(game).contains(p), "{what}: {p} is said off but is not");
                }
            },
        );
        assert!(points > 10, "{mode:?}: only {points} fault points");
    }
}

#[test]
fn every_failure_and_crash_point_of_stopping_a_hunt() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust_pruning(
            mode,
            // The folders the files were kept in are removed after the commit.
            &["mods_disabled"],
            |dir| {
                let (game, hunt) = game_in_a_hunt(dir);
                let original = identity(&game.root.join("update/update.rpf"));
                (game, (hunt, original))
            },
            |game, _| crash_hunt::stop(game, None).map(|()| Vec::new()),
            |game, (hunt, original), what, done| {
                assert_eq!(&identity(&game.root.join("update/update.rpf")), original, "{what}: the original changed");
                if done {
                    assert!(crash_hunt::load(game).is_none() && crash_hunt::turned_off(game).is_empty(), "{what}");
                } else {
                    assert_eq!(crash_hunt::load(game).as_ref(), Some(hunt), "{what}");
                }
            },
        );
        assert!(points > 10, "{mode:?}: only {points} fault points");
    }
}

// ---- the file browser's disk actions ----

fn game_with_a_mods_folder(dir: &Path) -> Game {
    let game = game_folder(dir, Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods/folder/sub")).unwrap();
    std::fs::write(game.root.join("mods/folder/a.txt"), b"a").unwrap();
    std::fs::write(game.root.join("mods/folder/sub/b.txt"), b"b").unwrap();
    std::fs::write(game.root.join("mods/c.txt"), b"c").unwrap();
    game
}

#[test]
fn deleting_files_and_folders_in_a_game_folder() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    // A link inside a folder is removed itself, never followed.
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, game.root.join("mods/folder/link")).unwrap();
    vfs::delete_local(&game, &[rel("mods/folder"), rel("mods/c.txt"), rel("scripts/orig.asi")]).unwrap();
    assert!(!game.root.join("mods/folder").exists() && !game.root.join("mods/c.txt").exists() && !game.root.join("scripts/orig.asi").exists());
    assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    assert_clean(&game, "after the deletion");
    let e = vfs::delete_local(&game, &[rel("mods/folder")]).unwrap_err().to_string();
    assert!(e.contains("does not exist"), "{e}");
}

#[test]
fn a_deletion_that_is_refused_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    let before = state(&game);
    for refused in ["x64/a.rpf", "x64", "update/update.rpf", "GTA5_Enhanced.exe", "mods", ".paleto", "mods_legacy"] {
        let result = vfs::delete_local(&game, &[rel("mods/c.txt"), rel(refused)]);
        assert!(result.is_err(), "{refused} was deleted");
        assert_unchanged(&before, &state(&game));
    }
    // The game's own libraries and notes at the top of the game folder, as for packages.
    for name in ["steam_api64.dll", "commandline.txt"] {
        std::fs::write(game.root.join(name), b"x").unwrap();
        assert!(vfs::delete_local(&game, &[rel(name)]).is_err(), "{name}");
        assert!(game.root.join(name).exists());
    }
    std::fs::write(game.root.join("dinput8.dll"), b"loader").unwrap();
    vfs::delete_local(&game, &[rel("dinput8.dll")]).unwrap();
}

#[test]
fn every_failure_and_crash_point_of_deleting_files() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust(
            mode,
            |dir| (game_with_a_mods_folder(dir), ()),
            |game, ()| vfs::delete_local(game, &[rel("mods/c.txt"), rel("mods/folder/a.txt"), rel("mods/folder/sub/b.txt")]).map(|()| Vec::new()),
            |game, (), what, done| {
                // The files go together or stay together, and the folders stay.
                for name in ["mods/c.txt", "mods/folder/a.txt", "mods/folder/sub/b.txt"] {
                    assert_eq!(game.root.join(name).exists(), !done, "{what}: {name}");
                }
                assert!(game.root.join("mods/folder/sub").is_dir(), "{what}");
            },
        );
        assert!(points > 5, "{mode:?}: only {points} fault points");
    }
}

#[test]
fn a_path_through_a_link_back_to_the_originals_is_not_deleted_or_renamed() {
    for absolute in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let game = game_with_a_link_to_the_originals(dir.path(), absolute);
        let original = identity(&game.root.join("x64/a.rpf"));
        let before = state(&game);
        assert!(vfs::delete_local(&game, &[rel("mods/x64/a.rpf")]).is_err(), "deleted through the link ({absolute})");
        assert!(vfs::rename_local(&game, &rel("mods/x64/a.rpf"), "b.rpf").is_err(), "renamed through the link ({absolute})");
        assert!(vfs::mkdir_local(&game, &rel("mods/x64/new")).is_err(), "made through the link ({absolute})");
        assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
        assert_unchanged(&before, &state(&game));
        // The link is refused with a reason before the user is asked.
        let (name, why) = vfs::local_refusal(&game.root.join("mods/x64"), &["a.rpf".to_string()]).expect("a refusal");
        assert_eq!(name, "a.rpf");
        assert!(why.contains("leads outside") || why.contains("link"), "{why}");
        // The link itself is only a name in the mods folder: it can be deleted, and the originals stay.
        vfs::delete_local(&game, &[rel("mods/x64")]).unwrap();
        assert!(!game.root.join("mods/x64").exists());
        assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
    }
}

#[test]
fn the_originals_of_any_game_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    // Two games: the file browser may show either, whichever one is open.
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let _first = game_folder(&a, Edition::Enhanced);
    let other = game_folder(&b, Edition::Legacy);
    std::fs::write(other.root.join("x64a.rpf"), b"rpf").unwrap();
    for name in ["x64", "update", "GTA5.exe", "x64a.rpf"] {
        let (refused, why) = vfs::local_refusal(&other.root, &[name.to_string()]).unwrap_or_else(|| panic!("{name} is not refused"));
        assert_eq!(refused, name);
        assert!(!why.is_empty());
    }
    for name in ["steam_api64.dll", "commandline.txt"] {
        std::fs::write(other.root.join(name), b"x").unwrap();
        assert!(vfs::local_refusal(&other.root, &[name.to_string()]).is_some(), "{name}");
    }
    let before = state(&other);
    for name in ["x64", "update", "GTA5.exe", "steam_api64.dll"] {
        assert!(vfs::delete_local(&other, &[rel(name)]).is_err(), "{name}");
        assert!(vfs::rename_local(&other, &rel(name), "renamed").is_err(), "{name}");
    }
    assert_unchanged(&before, &state(&other));
    // Plugins and the mods folder are not refused.
    assert!(vfs::local_refusal(&other.root.join("scripts"), &["orig.asi".to_string()]).is_none());
    assert!(vfs::local_refusal(&other.root, &["scripts".to_string()]).is_none());
    // A folder that is no game folder is nobody's business.
    assert!(vfs::local_refusal(dir.path(), &["a".to_string()]).is_none());
}

#[test]
fn renaming_never_replaces_what_is_at_the_new_name() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    std::fs::write(game.root.join("mods/d.txt"), b"d").unwrap();
    let before = state(&game);
    let e = vfs::rename_local(&game, &rel("mods/c.txt"), "d.txt").unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert_unchanged(&before, &state(&game));
    // A name that differs in case from another file is another file's, not this one's.
    let e = vfs::rename_local(&game, &rel("mods/c.txt"), "D.TXT").unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert_unchanged(&before, &state(&game));
    // A change of case alone is a rename.
    vfs::rename_local(&game, &rel("mods/c.txt"), "C.txt").unwrap();
    assert_eq!(std::fs::read(game.root.join("mods/C.txt")).unwrap(), b"c");
    // Folders, plain names.
    vfs::rename_local(&game, &rel("mods/folder"), "renamed").unwrap();
    assert!(game.root.join("mods/renamed/sub/b.txt").exists() && !game.root.join("mods/folder").exists());
    vfs::rename_local(&game, &rel("scripts/orig.asi"), "other.asi").unwrap();
    // Not a name, not in the game, not a data file.
    for bad in ["a/b", "..", "new.rpf", ".paleto-x"] {
        assert!(vfs::rename_local(&game, &rel("scripts/other.asi"), bad).is_err(), "{bad}");
    }
    assert!(game.root.join("scripts/other.asi").exists());
    assert_clean(&game, "after the renames");
}

#[test]
fn a_new_folder_is_made_once_through_the_confined_folders() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    vfs::mkdir_local(&game, &rel("mods/folder/new")).unwrap();
    assert!(game.root.join("mods/folder/new").is_dir());
    assert!(vfs::mkdir_local(&game, &rel("mods/folder/new")).is_err());
    assert!(vfs::mkdir_local(&game, &rel("mods/FOLDER/NEW")).is_err(), "the same name in another case");
    vfs::mkdir_local(&game, &rel("scripts/more")).unwrap();
    // Not in the originals.
    assert!(vfs::mkdir_local(&game, &rel("x64/more")).is_err());
    assert!(!game.root.join("x64/more").exists());
}

// ---- review fixes ----

#[test]
fn a_link_chosen_in_a_folder_reached_by_its_real_place_is_the_item_not_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    // `mods` is on another drive: the file browser shows it by its real place, which is not under the game folder.
    let elsewhere = dir.path().join("drive/mods");
    std::fs::create_dir_all(elsewhere.join("bar")).unwrap();
    std::fs::write(elsewhere.join("bar/f.txt"), b"f").unwrap();
    std::os::unix::fs::symlink(&elsewhere, game.root.join("mods")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("bar"), elsewhere.join("foo")).unwrap();
    std::os::unix::fs::symlink(game.root.join("scripts"), elsewhere.join("sl")).unwrap();
    // The folder is found through the link; the last name is not followed.
    assert_eq!(vfs::local_rel(&elsewhere.join("foo"), &game).unwrap(), rel("mods/foo"));
    assert_eq!(vfs::local_rel(&elsewhere.join("sl"), &game).unwrap(), rel("mods/sl"));
    assert!(vfs::local_refusal(&elsewhere, &["foo".to_string(), "sl".to_string()]).is_none());
    vfs::rename_local(&game, &vfs::local_rel(&elsewhere.join("foo"), &game).unwrap(), "moved").unwrap();
    assert!(elsewhere.join("moved").is_symlink() && !elsewhere.join("foo").exists());
    vfs::delete_local(&game, &[vfs::local_rel(&elsewhere.join("moved"), &game).unwrap(), vfs::local_rel(&elsewhere.join("sl"), &game).unwrap()]).unwrap();
    assert!(!elsewhere.join("moved").is_symlink() && !elsewhere.join("sl").is_symlink());
    assert_eq!(std::fs::read(elsewhere.join("bar/f.txt")).unwrap(), b"f");
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"orig");
    assert_clean(&game, "after the link deletions");
}

#[test]
fn every_failure_and_crash_point_of_deleting_a_folder() {
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        let points = exhaust_pruning(
            mode,
            // The folders are removed after the commit: there empty, or gone; the files are all there or all gone.
            &["mods/folder"],
            |dir| (game_with_a_mods_folder(dir), ()),
            |game, ()| match vfs::delete_local(game, &[rel("mods/folder")]) {
                // The files went and a folder was left: said, and the same end as the folders gone.
                Err(Error::Io(e)) if e.to_string().starts_with("the files were deleted, but") => Ok(Vec::new()),
                other => other.map(|()| Vec::new()),
            },
            |game, (), what, done| {
                for name in ["mods/folder/a.txt", "mods/folder/sub/b.txt"] {
                    assert_eq!(game.root.join(name).exists(), !done, "{what}: {name}");
                }
            },
        );
        assert!(points > 5, "{mode:?}: only {points} fault points");
    }
}

#[test]
fn a_rename_never_replaces_what_appeared_at_the_new_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), b"a").unwrap();
    std::fs::write(dir.path().join("B"), b"B").unwrap();
    let held = crate::fsroot::Dir::open_path(dir.path()).unwrap();
    // `B` is another file than `a`: even for a change of case it is not replaced.
    assert!(vfs::rename_noreplace(&held, "a", "B", true).is_err());
    assert_eq!(std::fs::read(dir.path().join("B")).unwrap(), b"B");
    assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), b"a");
    assert!(vfs::rename_noreplace(&held, "a", "B", false).is_err());
    vfs::rename_noreplace(&held, "a", "c", false).unwrap();
    assert_eq!(std::fs::read(dir.path().join("c")).unwrap(), b"a");
}

#[test]
fn the_mods_folder_itself_is_deleted_and_renamed_as_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    let original = identity(&game.root.join("x64/a.rpf"));
    vfs::rename_local(&game, &rel("mods"), "mods_old").unwrap();
    assert!(game.root.join("mods_old/folder/sub/b.txt").is_file() && !game.root.join("mods").exists());
    assert!(vfs::rename_local(&game, &rel("mods"), "x64").is_err(), "no mods folder any more");
    assert!(vfs::rename_local(&game, &rel("mods_old"), "x64").is_err());
    vfs::rename_local(&game, &rel("mods_old"), "mods").unwrap();
    assert!(vfs::rename_local(&game, &rel("mods"), "x64").is_err(), "a name of the game's data");
    assert!(vfs::rename_local(&game, &rel("mods"), "scripts").is_err(), "a name that is taken");
    assert!(vfs::local_refusal(&game.root, &["mods".to_string()]).is_none());
    vfs::delete_local(&game, &[rel("mods")]).unwrap();
    assert!(!game.root.join("mods").exists());
    assert_eq!(identity(&game.root.join("x64/a.rpf")), original);
    assert_clean(&game, "after deleting the mods folder");
    assert!(vfs::delete_local(&game, &[rel("mods")]).is_err());
}

#[test]
fn a_mods_link_is_deleted_and_renamed_itself() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(profile.join("sub")).unwrap();
    std::fs::write(profile.join("sub/k.txt"), b"keep").unwrap();
    std::fs::write(profile.join("p.txt"), b"p").unwrap();
    std::os::unix::fs::symlink(&profile, game.root.join("mods")).unwrap();
    vfs::rename_local(&game, &rel("mods"), "mods_a").unwrap();
    assert!(game.root.join("mods_a").is_symlink() && !game.root.join("mods").exists());
    assert_eq!(std::fs::read(profile.join("sub/k.txt")).unwrap(), b"keep");
    vfs::rename_local(&game, &rel("mods_a"), "mods").unwrap();
    vfs::delete_local(&game, &[rel("mods")]).unwrap();
    assert!(std::fs::symlink_metadata(game.root.join("mods")).is_err());
    assert_eq!(std::fs::read(profile.join("sub/k.txt")).unwrap(), b"keep");
    assert_eq!(std::fs::read(profile.join("p.txt")).unwrap(), b"p");
    assert_clean(&game, "after deleting the mods link");
}

#[test]
fn a_hunt_step_that_moved_parts_and_failed_is_not_gone_on_from() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(&root).unwrap();
    for f in ["GTA5_Enhanced.exe", "A.asi", "B.asi", "C.asi", "D.asi"] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    let game = Game::open(&root).unwrap();
    let crash_hunt::Outcome::Test(hunt) = crash_hunt::start(&game).unwrap() else { panic!("no test to run") };
    // A step moved a part, but its change of the state failed: the state is the old one.
    let spare = ["A.asi", "B.asi", "C.asi", "D.asi"].into_iter().find(|p| !hunt.disabled.iter().any(|d| d == p)).unwrap();
    std::fs::rename(root.join(spare), root.join("mods_disabled").join(spare)).unwrap();
    let e = crash_hunt::answer(&game, None, true).unwrap_err().to_string();
    assert!(e.contains("interrupted") && e.contains("stop it"), "{e}");
    assert_eq!(crash_hunt::load(&game).as_ref(), Some(&hunt));
    crash_hunt::stop(&game, None).unwrap();
    assert!(["A.asi", "B.asi", "C.asi", "D.asi"].iter().all(|p| root.join(p).exists()) && crash_hunt::turned_off(&game).is_empty());
}

// ---- second review ----

#[test]
fn a_name_like_mods_in_another_case_is_not_the_mods_folder() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    std::fs::create_dir_all(game.root.join("MODS")).unwrap();
    std::fs::write(game.root.join("MODS/x.txt"), b"x").unwrap();
    let before = state(&game);
    for result in [
        vfs::delete_local(&game, &[rel("MODS")]),
        vfs::delete_local(&game, &[rel("Mods")]),
        vfs::rename_local(&game, &rel("MODS"), "other"),
        vfs::rename_local(&game, &rel("scripts"), "Mods"),
        vfs::rename_local(&game, &rel("mods"), "MODS"),
        vfs::mkdir_local(&game, &rel("mODS")),
    ] {
        let e = result.unwrap_err().to_string();
        assert!(e.contains("named like the mods folder"), "{e}");
    }
    // The real one is untouched by all of them, and says why when asked.
    assert_unchanged(&before, &state(&game));
    let (name, why) = vfs::local_refusal(&game.root, &["MODS".to_string()]).unwrap();
    assert!(name == "MODS" && why.contains("named like the mods folder"), "{why}");
}

#[test]
fn a_folder_that_cannot_be_removed_after_the_files_went_is_reported() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    std::fs::create_dir_all(game.root.join("mods/outer/inner")).unwrap();
    std::fs::write(game.root.join("mods/outer/inner/f.txt"), b"f").unwrap();
    // `inner` can be emptied, but `outer` cannot be changed, so `inner` cannot be removed.
    let _locked = ReadOnly::new(&game.root.join("mods/outer"));
    let e = vfs::delete_local(&game, &[rel("mods/outer/inner")]).unwrap_err().to_string();
    assert!(e.contains("the files were deleted") && e.contains("folder mods/outer/inner") && !e.contains("nothing was"), "{e}");
    assert!(!game.root.join("mods/outer/inner/f.txt").exists() && game.root.join("mods/outer/inner").is_dir());
}

#[test]
fn dots_in_a_folder_reached_through_a_link_go_where_the_link_leads() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_with_a_mods_folder(dir.path());
    let outside = dir.path().join("outside/sub");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(dir.path().join("outside/f.txt"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, game.root.join("mods/link")).unwrap();
    // Not `mods/f.txt` (what the name says), but `outside/f.txt`, which is not in the game.
    assert!(vfs::local_rel(&game.root.join("mods/link/../f.txt"), &game).is_err());
    assert!(vfs::delete_local(&game, &[vfs::local_rel(&game.root.join("mods/folder/../c.txt"), &game).unwrap()]).is_ok());
    assert!(!game.root.join("mods/c.txt").exists());
    assert_eq!(std::fs::read(dir.path().join("outside/f.txt")).unwrap(), b"outside");
}
