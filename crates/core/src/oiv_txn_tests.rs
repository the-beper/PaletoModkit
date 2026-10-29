//! Acceptance tests of work package A4 (OIV installs through the Transaction Engine): an install is one
//! transaction, so every failure leaves the folders as they were, every crash is recovered, overlay commands
//! see what earlier commands of the package did, and an existing archive file is never written in place.
//! Tests marked `#[ignore]` fail on the interim installer and say what A4 must provide.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use paleto_rpf::ArchiveEditor;

use crate::fixtures::*;
use crate::fsroot::faults::{self, Mode as Fault, Point};
use crate::game::{Edition, Game};
use crate::txn::{self, PendingState, Txn};
use crate::{Error, Package};

fn install(game: &Game, oiv: &Path) -> Result<Vec<String>, Error> {
    Package::open(oiv)?.install_with(game, None)
}

fn state(game: &Game) -> BTreeMap<String, Entry> {
    snapshot(&game.root)
}

fn journals(game: &Game) -> Vec<String> {
    std::fs::read_dir(game.root.join(".paleto/journal")).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

fn journal_has(game: &Game, record: &str) -> bool {
    journals(game).iter().any(|n| std::fs::read_to_string(game.root.join(".paleto/journal").join(n)).unwrap_or_default().lines().any(|l| l.split_whitespace().next() == Some(record)))
}

fn assert_clean(game: &Game, what: &str) {
    assert_eq!(journals(game), Vec::<String>::new(), "{what}: a journal is left");
    assert_eq!(leftovers(&game.root), Vec::<String>::new(), "{what}: temporary names are left");
}

fn without_temps(mut s: BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    s.retain(|k, _| !k.rsplit('/').next().unwrap().starts_with(".paleto-"));
    s
}

/// Fails with the names that differ, never with the contents (they are whole archives).
fn assert_same(what: &str, want: &BTreeMap<String, Entry>, got: &BTreeMap<String, Entry>) {
    let mut diffs: Vec<String> = want.iter().filter(|(k, v)| got.get(*k) != Some(*v)).map(|(k, _)| format!("{} {k}", if got.contains_key(k) { "changed" } else { "missing" })).collect();
    diffs.extend(got.keys().filter(|k| !want.contains_key(*k)).map(|k| format!("extra {k}")));
    assert!(diffs.is_empty(), "{what}: {diffs:?}");
}

/// An error that claims nothing false: the plain cause (nothing had been changed), or a failure that says the undo was complete.
fn honest(e: &Error) -> bool {
    e.to_string().contains("nothing was installed") || !matches!(e, Error::Txn(_))
}

/// An Enhanced game with the loose files, the text, the XML and the archive copy the package works on.
fn setup(dir: &Path) -> Game {
    let game = game_folder(dir, Edition::Enhanced);
    let root = &game.root;
    std::fs::write(root.join("scripts/gone.asi"), b"gone").unwrap();
    std::fs::write(root.join("scripts/settings.ini"), b"a=1\n").unwrap();
    std::fs::create_dir_all(root.join("mods/cfg")).unwrap();
    std::fs::write(root.join("mods/cfg/conf.xml"), b"<root><a/></root>").unwrap();
    std::fs::create_dir_all(root.join("mods/x64")).unwrap();
    std::fs::copy(root.join("x64/a.rpf"), root.join("mods/x64/a.rpf")).unwrap();
    game
}

const REALISTIC: &str = r#"
    <add source="x">scripts\new.asi</add>
    <add source="y">scripts\orig.asi</add>
    <delete>scripts\gone.asi</delete>
    <text path="scripts\settings.ini"><add>b=2</add></text>
    <xml path="mods\cfg\conf.xml"><add xpath="/root" append="Last"><b/></add></xml>
    <archive path="x64\a.rpf"><add source="x">b.txt</add></archive>
    <archive path="x64\new.rpf" createIfNotExist="true"><add source="x">n.txt</add><archive path="inner.rpf" createIfNotExist="true"><add source="y">i.txt</add></archive></archive>"#;

fn realistic(dir: &Path) -> PathBuf {
    oiv(dir, "x.oiv", REALISTIC, &[("content/x", b"new content"), ("content/y", b"other content")], true)
}

/// The folder before and after an unfaulted install.
fn references() -> (BTreeMap<String, Entry>, BTreeMap<String, Entry>) {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = realistic(dir.path());
    let old = state(&game);
    install(&game, &package).unwrap();
    assert_clean(&game, "unfaulted");
    let new = state(&game);
    assert!(old != new, "the install changed nothing");
    assert!(matches!(new.get("mods/x64/new.rpf"), Some(Entry::File(_))));
    assert_eq!(new["scripts/settings.ini"], Entry::File(b"a=1\nb=2\n".to_vec()));
    // Installed again over its own result, the package works too: what the crash tests do after a recovery.
    install(&game, &package).unwrap_or_else(|e| panic!("the second install of the package failed: {e}"));
    assert_clean(&game, "installed twice");
    (old, new)
}

/// Fails (or crashes at) the k-th system call of the install, for every k until none is left to hit.
fn exhaust(mode: Fault) -> u64 {
    let (old, new) = references();
    let crash = matches!(mode, Fault::Crash);
    let (mut saw_error, mut saw_committed) = (false, false);
    for k in 1..20_000 {
        let dir = tempfile::tempdir().unwrap();
        let game = setup(dir.path());
        let package = realistic(dir.path());
        faults::arm(k, mode);
        let result = install(&game, &package);
        if !faults::disarm() {
            result.unwrap();
            assert_same("no fault", &new, &state(&game));
            assert!(saw_error, "the install hit no fault point at all");
            assert!(!crash || saw_committed, "no crash happened after `committed`");
            return k - 1;
        }
        let what = format!("{mode:?} at {k}");
        let mut installed = false;
        if crash {
            let committed = journal_has(&game, "committed");
            let report = txn::recover(&game).unwrap();
            assert!(report.failures.is_empty(), "{what}: {report:?}");
            let now = state(&game);
            // The journal may be gone too, if the crash was at the last call of the install: then everything was in place.
            installed = committed || now == new;
            assert_same(&format!("{what}: committed={committed}"), if installed { &new } else { &old }, &now);
            saw_committed |= installed;
            saw_error |= !installed;
        } else {
            match result {
                Err(e) => {
                    assert_unchanged(&old, &state(&game));
                    assert!(honest(&e), "{what}: {e}");
                    saw_error = true;
                }
                Ok(_) => {
                    // The fault hit the cleanup after the commit: the files are in place and recovery finishes it.
                    assert_same(&what, &new, &without_temps(state(&game)));
                    installed = true;
                    txn::recover(&game).unwrap();
                }
            }
        }
        assert_clean(&game, &what);
        // Whatever happened, the next install works; after an undo it gives what an unfaulted one gives.
        install(&game, &package).unwrap_or_else(|e| panic!("{what}: the next install failed: {e}"));
        if !installed {
            assert_same(&format!("{what}: the next install differs from an unfaulted one"), &new, &state(&game));
        }
        assert_clean(&game, &what);
    }
    unreachable!()
}

#[test]
#[ignore = "A4: the install is one journaled transaction: every failure is undone and what a fault in the cleanup leaves is finished by recovery"]
fn every_failure_point_leaves_the_folders_as_they_were() {
    let n = exhaust(Fault::Fail(std::io::ErrorKind::Other));
    assert!(n > 100, "only {n} fault points");
}

#[test]
#[ignore = "A4: one journal for the whole install, which txn::recover rolls back or finishes"]
fn every_crash_point_is_recovered() {
    exhaust(Fault::Crash);
}

#[test]
#[ignore = "A4: one journal for the whole install, so recovery puts every target back or finishes them all"]
fn a_crash_after_the_commit_began_leaves_every_target_old_or_new() {
    let (old, new) = references();
    let (mut finished, mut undone) = (0, 0);
    for k in 1..20_000 {
        let dir = tempfile::tempdir().unwrap();
        let game = setup(dir.path());
        let package = realistic(dir.path());
        faults::arm(k, Fault::Crash);
        let _ = install(&game, &package);
        if !faults::disarm() {
            break;
        }
        // Before recovery: the names the package changes hold their old or their new content, never a mix (temporary names aside).
        let crashed = without_temps(state(&game));
        for name in old.keys().chain(new.keys()).chain(crashed.keys()) {
            let e = crashed.get(name);
            assert!(e == old.get(name) || e == new.get(name), "k={k}: {name} is neither fully old nor fully new at the crash");
        }
        let began = journal_has(&game, "commit");
        // The crash may come after the operation was over (only its journal was left to remove): recovery then has nothing to do.
        let over = journal_has(&game, "end");
        let report = txn::recover(&game).unwrap();
        assert!(report.failures.is_empty(), "k={k}: {report:?}");
        let now = state(&game);
        for name in old.keys().chain(new.keys()).chain(now.keys()) {
            let e = now.get(name);
            assert!(e == old.get(name) || e == new.get(name), "k={k}: {name} is neither fully old nor fully new");
        }
        if began && !over {
            // One journal for the whole install: the report counts one operation, and every target went the same way.
            assert_eq!(report.finished + report.rolled_back, 1, "k={k}: the report does not say what happened: {report:?}");
            if report.finished == 1 {
                assert_same(&format!("k={k}: reported as finished"), &new, &now);
                finished += 1;
            } else {
                assert_same(&format!("k={k}: reported as undone"), &old, &now);
                undone += 1;
            }
        }
    }
    assert!(finished > 0 && undone > 0, "finished {finished}, undone {undone}: the crashes did not cover both sides of the commit");
}

fn text_of(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn text_after_add_applies_to_the_added_content() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = oiv(dir.path(), "x.oiv", r#"<add source="x">scripts\o.ini</add><text path="scripts\o.ini"><add>b=2</add></text>"#, &[("content/x", b"a=1\n")], false);
    install(&game, &package).unwrap();
    assert_eq!(text_of(&game.root.join("scripts/o.ini")), "a=1\nb=2\n");
    assert_clean(&game, "after");
}

#[test]
fn an_archive_command_edits_an_rpf_added_earlier() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    rpf(&dir.path().join("p.rpf"), "p.rpf", &[("p.txt", b"p")]);
    let content = std::fs::read(dir.path().join("p.rpf")).unwrap();
    let package = oiv(dir.path(), "x.oiv", r#"<add source="p">x64\added.rpf</add><archive path="x64\added.rpf"><add source="x">b.txt</add></archive>"#, &[("content/p", &content), ("content/x", b"bee")], false);
    install(&game, &package).unwrap();
    let added = ArchiveEditor::open(&game.root.join("mods/x64/added.rpf"), None).unwrap();
    assert_eq!(added.read("p.txt", None).unwrap(), b"p");
    assert_eq!(added.read("b.txt", None).unwrap(), b"bee");
    assert!(!game.root.join("x64/added.rpf").exists());
    assert_clean(&game, "after");
}

#[test]
fn an_add_after_an_archive_edit_replaces_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    rpf(&dir.path().join("p.rpf"), "p.rpf", &[("z.txt", b"z")]);
    let content = std::fs::read(dir.path().join("p.rpf")).unwrap();
    let package = oiv(dir.path(), "x.oiv", r#"<archive path="x64\a.rpf"><add source="x">b.txt</add></archive><add source="p">x64\a.rpf</add>"#, &[("content/p", &content), ("content/x", b"bee")], false);
    let original = std::fs::read(game.root.join("x64/a.rpf")).unwrap();
    install(&game, &package).unwrap();
    assert!(std::fs::read(game.root.join("mods/x64/a.rpf")).unwrap() == content, "the add wins");
    assert!(std::fs::read(game.root.join("x64/a.rpf")).unwrap() == original, "the original archive changed");
    assert_clean(&game, "after");
}

#[test]
fn an_add_after_a_delete_of_the_same_name_leaves_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = oiv(dir.path(), "x.oiv", r#"<delete>scripts\gone.asi</delete><add source="x">scripts\gone.asi</add>"#, &[("content/x", b"back")], false);
    install(&game, &package).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/gone.asi")).unwrap(), b"back");
    assert_clean(&game, "after");
}

/// (device, inode, bytes) of a file.
fn identity(path: &Path) -> (u64, u64, Vec<u8>) {
    let m = std::fs::metadata(path).unwrap();
    (m.dev(), m.ino(), std::fs::read(path).unwrap())
}

#[test]
fn an_existing_archive_file_is_replaced_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = realistic(dir.path());
    let target = game.root.join("mods/x64/a.rpf");
    let kept = dir.path().join("kept.rpf");
    std::fs::hard_link(&target, &kept).unwrap();
    let (dev, ino, bytes) = identity(&target);
    install(&game, &package).unwrap();
    assert!(identity(&kept) == (dev, ino, bytes.clone()), "the old inode was written to");
    let (dev2, ino2, bytes2) = identity(&target);
    assert_eq!(dev2, dev);
    assert_ne!(ino2, ino, "the path still has the old inode");
    assert!(bytes2 != bytes, "the archive was not edited");
    assert_eq!(ArchiveEditor::open(&target, None).unwrap().read("b.txt", None).unwrap(), b"new content");
}

#[test]
fn a_failed_install_leaves_the_archive_file_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let failing = format!(r#"{REALISTIC}<xml path="mods\cfg\missing.xml"><remove xpath="/a" /></xml>"#);
    let package = oiv(dir.path(), "x.oiv", &failing, &[("content/x", b"new content"), ("content/y", b"other content")], true);
    let target = game.root.join("mods/x64/a.rpf");
    let before = identity(&target);
    let snap = state(&game);
    let err = install(&game, &package).unwrap_err();
    assert!(err.to_string().contains("nothing was installed"), "{err}");
    assert!(identity(&target) == before, "the archive file changed");
    assert_unchanged(&snap, &state(&game));
    assert_clean(&game, "after");
}

/// How many renames an unfaulted install makes: with the engine, one per file it changes.
fn renames_of_an_install() -> u64 {
    for n in 1..2_000 {
        let dir = tempfile::tempdir().unwrap();
        let game = setup(dir.path());
        let package = realistic(dir.path());
        faults::arm_at(Point::Rename, n, Fault::Fail(std::io::ErrorKind::Other));
        let _ = install(&game, &package);
        if !faults::disarm() {
            return n - 1;
        }
    }
    unreachable!()
}

#[test]
#[ignore = "A4: the install runs as a txn::Txn, whose rollback is where txn::ON_ROLLBACK fires"]
fn a_rollback_step_that_fails_is_reported_and_kept_until_recovered() {
    let (old, _) = references();
    // The last rename of an install is the last file of the commit: failing there, everything else is in place and has to go back.
    let renames = renames_of_an_install();
    assert!(renames > 3, "{renames} renames");
    let (mut stuck, mut named_backup) = (0, false);
    for k in 1..2_000 {
        let dir = tempfile::tempdir().unwrap();
        let game = setup(dir.path());
        let package = realistic(dir.path());
        // The install fails at its last rename; when its rollback begins, the k-th call of the rollback fails too.
        let rolled_back = Rc::new(Cell::new(false));
        let seen = rolled_back.clone();
        txn::ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || {
            seen.set(true);
            faults::arm(k, Fault::Fail(std::io::ErrorKind::Other));
        })));
        faults::arm_at(Point::Rename, renames, Fault::Fail(std::io::ErrorKind::Other));
        let result = install(&game, &package);
        let fired = faults::disarm();
        txn::ON_ROLLBACK.with(|h| h.borrow_mut().take());
        assert!(rolled_back.get(), "the failed install was not rolled back by the transaction engine");
        let err = result.unwrap_err();
        let what = format!("rollback fault at {k}: {err}");
        let text = err.to_string();
        if text.contains("nothing was installed") {
            // Everything is back; a fault in the last steps may only have left the journal, which the next run removes.
            assert_unchanged(&old, &state(&game));
            let report = txn::recover(&game).unwrap();
            assert!(report.failures.is_empty(), "{what}: {report:?}");
            assert_clean(&game, &what);
        } else {
            let Error::Txn(failure) = &err else { panic!("{what}") };
            assert!(!failure.undone && !failure.failures.is_empty(), "{what}");
            // Every file that could not be put back is named with its full path; some name their backup too.
            assert!(text.contains("could not be fully undone") && failure.failures.iter().all(|f| f.contains(&*game.root.to_string_lossy())), "{what}");
            named_backup |= text.contains(".paleto-") && text.contains(".old");
            let pending = txn::pending(&game);
            assert!(pending.iter().any(|p| p.state == PendingState::Stuck), "{what}: {pending:?}");
            let roots = game.roots().unwrap();
            assert!(matches!(Txn::begin(&roots, "next"), Err(Error::RecoveryNeeded(_))), "{what}");
            let report = txn::recover(&game).unwrap();
            assert!(report.failures.is_empty(), "{what}: {report:?}");
            assert_unchanged(&old, &state(&game));
            assert_clean(&game, &what);
            assert!(txn::pending(&game).is_empty());
            drop(Txn::begin(&roots, "next").unwrap());
            stuck += 1;
        }
        if !fired {
            break;
        }
    }
    assert!(stuck > 0 && named_backup, "stuck {stuck}, backup named {named_backup}: no rollback step made the rollback fail");
}

#[test]
fn new_modified_and_deleted_files_are_installed_by_one_install() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = realistic(dir.path());
    install(&game, &package).unwrap();
    let root = &game.root;
    assert_eq!(std::fs::read(root.join("scripts/new.asi")).unwrap(), b"new content", "a new file");
    assert_eq!(std::fs::read(root.join("scripts/orig.asi")).unwrap(), b"other content", "a replaced file");
    assert!(!root.join("scripts/gone.asi").exists(), "a deleted file");
    assert_eq!(text_of(&root.join("scripts/settings.ini")), "a=1\nb=2\n");
    assert!(text_of(&root.join("mods/cfg/conf.xml")).contains("<b/>"));
    let edited = ArchiveEditor::open(&root.join("mods/x64/a.rpf"), None).unwrap();
    assert_eq!((edited.read("a.txt", None).unwrap(), edited.read("b.txt", None).unwrap()), (b"a".to_vec(), b"new content".to_vec()));
    let mut created = ArchiveEditor::open(&root.join("mods/x64/new.rpf"), None).unwrap();
    assert_eq!(created.read("n.txt", None).unwrap(), b"new content");
    assert_eq!(created.archive("inner.rpf", false, None).unwrap().read("i.txt", None).unwrap(), b"other content");
    assert_clean(&game, "after");
}

/// `x64/o.rpf` holds `o.txt` and `inner.rpf` (`i.txt` and `deep.rpf` with `d.txt`).
fn nested_archive(path: &Path) {
    let mut outer = ArchiveEditor::new("o.rpf");
    outer.add_file("o.txt", b"o".to_vec()).unwrap();
    let inner = outer.archive("inner.rpf", true, None).unwrap();
    inner.add_file("i.txt", b"i\n".to_vec()).unwrap();
    inner.archive("deep.rpf", true, None).unwrap().add_file("d.txt", b"d".to_vec()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    outer.save(path).unwrap();
}

/// Edits at every level of `nested_archive`, and an archive added to the outer one and edited at once.
const NESTED: &str = r#"
    <archive path="x64\o.rpf">
        <add source="y">top.txt</add>
        <add source="p">side.rpf</add>
        <archive path="side.rpf"><add source="x">s.txt</add></archive>
        <archive path="inner.rpf">
            <add source="x">j.txt</add>
            <text path="i.txt"><add>j</add></text>
            <archive path="deep.rpf"><delete>d.txt</delete><add source="y">e.txt</add></archive>
        </archive>
    </archive>"#;

#[test]
fn nested_archives_are_edited_inside_one_new_top_level_file() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let original = game.root.join("x64/o.rpf");
    nested_archive(&original);
    rpf(&dir.path().join("p.rpf"), "p.rpf", &[("p.txt", b"p")]);
    let side = std::fs::read(dir.path().join("p.rpf")).unwrap();
    let package = oiv(dir.path(), "x.oiv", NESTED, &[("content/x", b"new content"), ("content/y", b"other content"), ("content/p", &side)], false);
    let before = identity(&original);
    install(&game, &package).unwrap();
    assert!(identity(&original) == before, "the original archive was written");
    // The edit of archives within archives is one new file in the mods folder, and nothing else changes.
    let made: Vec<String> = state(&game).into_keys().filter(|k| k.starts_with("mods")).collect();
    assert_eq!(made, ["mods", "mods/x64", "mods/x64/o.rpf"]);
    let mut outer = ArchiveEditor::open(&game.root.join("mods/x64/o.rpf"), None).unwrap();
    assert_eq!(outer.read("o.txt", None).unwrap(), b"o");
    assert_eq!(outer.read("top.txt", None).unwrap(), b"other content");
    let side = outer.archive("side.rpf", false, None).unwrap();
    assert_eq!((side.read("p.txt", None).unwrap(), side.read("s.txt", None).unwrap()), (b"p".to_vec(), b"new content".to_vec()));
    let inner = outer.archive("inner.rpf", false, None).unwrap();
    assert_eq!((inner.read("i.txt", None).unwrap(), inner.read("j.txt", None).unwrap()), (b"i\nj\n".to_vec(), b"new content".to_vec()));
    let deep = inner.archive("deep.rpf", false, None).unwrap();
    assert!(!deep.exists("d.txt"));
    assert_eq!(deep.read("e.txt", None).unwrap(), b"other content");
    assert_clean(&game, "after");
}

#[test]
fn a_failing_command_in_a_nested_archive_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    nested_archive(&game.root.join("x64/o.rpf"));
    // The loose file and the first edits work; the last command, two levels down, names a file that is not there.
    let package = oiv(
        dir.path(),
        "x.oiv",
        r#"<add source="x">scripts\new.asi</add>
        <archive path="x64\o.rpf"><add source="x">top.txt</add><archive path="inner.rpf"><add source="x">j.txt</add><archive path="deep.rpf"><text path="missing.txt"><add>z</add></text></archive></archive></archive>"#,
        &[("content/x", b"new content")],
        false,
    );
    let before = state(&game);
    let err = install(&game, &package).unwrap_err();
    assert!(err.to_string().contains("nothing was installed"), "{err}");
    assert_unchanged(&before, &state(&game));
    assert_clean(&game, "after");
}

#[test]
fn the_original_files_are_byte_identical_after_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let root = &game.root;
    std::fs::write(root.join("update/notes.txt"), b"a\n").unwrap();
    nested_archive(&root.join("x64/o.rpf"));
    // None of the archives and the text has a copy in the mods folder yet: the edits make them there.
    let package = oiv(
        dir.path(),
        "x.oiv",
        r#"<add source="x">scripts\new.asi</add>
        <archive path="x64\a.rpf"><add source="x">b.txt</add></archive>
        <archive path="update\update.rpf"><add source="y">extra.txt</add></archive>
        <archive path="x64\o.rpf"><archive path="inner.rpf"><add source="x">j.txt</add></archive></archive>
        <text path="update\notes.txt"><add>b</add></text>"#,
        &[("content/x", b"new content"), ("content/y", b"other content")],
        false,
    );
    let originals = ["x64/a.rpf", "x64/o.rpf", "update/update.rpf", "update/notes.txt"];
    let before: Vec<_> = originals.iter().map(|p| identity(&root.join(p))).collect();
    let mut expected = state(&game);
    install(&game, &package).unwrap();
    for (name, was) in originals.iter().zip(&before) {
        assert!(identity(&root.join(name)) == *was, "{name}: the original file was written or replaced");
    }
    // Outside the mods folder only the loose file the package adds is different.
    let mut after = state(&game);
    let in_mods: Vec<String> = after.keys().filter(|k| *k == "mods" || k.starts_with("mods/")).cloned().collect();
    after.retain(|k, _| !in_mods.contains(k));
    expected.insert("scripts/new.asi".to_string(), Entry::File(b"new content".to_vec()));
    assert_same("outside the mods folder", &expected, &after);
    assert_eq!(in_mods, ["mods", "mods/update", "mods/update/notes.txt", "mods/update/update.rpf", "mods/x64", "mods/x64/a.rpf", "mods/x64/o.rpf"]);
    assert_eq!(text_of(&root.join("mods/update/notes.txt")), "a\nb\n");
    assert_eq!(ArchiveEditor::open(&root.join("mods/x64/a.rpf"), None).unwrap().read("b.txt", None).unwrap(), b"new content");
    let list = ArchiveEditor::open(&root.join("mods/update/update.rpf"), None).unwrap();
    assert_eq!(list.read("extra.txt", None).unwrap(), b"other content");
    assert!(list.exists("common/data/dlclist.xml"));
    assert_clean(&game, "after");
}

#[test]
fn several_commands_on_one_name_compose_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    let package = oiv(
        dir.path(),
        "x.oiv",
        r#"<text path="scripts\settings.ini"><add>b=2</add></text>
        <text path="scripts\settings.ini"><add>c=3</add></text>
        <add source="x">scripts\twice.asi</add>
        <add source="y">scripts\twice.asi</add>
        <add source="x">scripts\short.asi</add>
        <delete>scripts\short.asi</delete>
        <delete>scripts\gone.asi</delete>
        <delete>scripts\gone.asi</delete>
        <archive path="x64\a.rpf"><add source="x">one.txt</add></archive>
        <archive path="x64\a.rpf"><add source="y">two.txt</add></archive>"#,
        &[("content/x", b"first"), ("content/y", b"second")],
        false,
    );
    install(&game, &package).unwrap();
    let root = &game.root;
    assert_eq!(text_of(&root.join("scripts/settings.ini")), "a=1\nb=2\nc=3\n");
    assert_eq!(std::fs::read(root.join("scripts/twice.asi")).unwrap(), b"second");
    assert!(!root.join("scripts/short.asi").exists() && !root.join("scripts/gone.asi").exists());
    let archive = ArchiveEditor::open(&root.join("mods/x64/a.rpf"), None).unwrap();
    assert_eq!((archive.read("a.txt", None).unwrap(), archive.read("one.txt", None).unwrap(), archive.read("two.txt", None).unwrap()), (b"a".to_vec(), b"first".to_vec(), b"second".to_vec()));
    assert_clean(&game, "after");
}

#[test]
#[ignore = "A4: archives are staged in the same transaction as the loose files, so a failure undoes them all"]
fn a_failure_while_an_archive_is_staged_leaves_the_loose_files_as_they_were() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = setup(dir.path());
    // The loose files come first; the archive is the last thing to be written, in a folder that cannot be written to.
    let package = oiv(
        dir.path(),
        "x.oiv",
        r#"<add source="x">scripts\new.asi</add>
        <text path="scripts\settings.ini"><add>b=2</add></text>
        <delete>scripts\gone.asi</delete>
        <archive path="x64\a.rpf"><add source="x">b.txt</add></archive>"#,
        &[("content/x", b"new")],
        false,
    );
    let before = state(&game);
    let target = game.root.join("mods/x64/a.rpf");
    let archive = identity(&target);
    let _guard = ReadOnly::new(&game.root.join("mods/x64"));
    let err = install(&game, &package).unwrap_err();
    assert!(honest(&err), "{err}");
    assert_same("after the failure", &before, &state(&game));
    assert!(identity(&target) == archive, "the archive file changed");
    assert_clean(&game, "after");
}

#[test]
#[ignore = "A4: every changed file, archives included, is put in place by the commit of the transaction"]
fn a_rename_that_fails_during_the_commit_undoes_the_files_already_in_place() {
    let (old, new) = references();
    let renames = renames_of_an_install();
    // The package changes 7 files (2 created, 4 replaced, 1 deleted) and each one is put in place by a rename.
    assert!(renames >= 7, "{renames} renames: the changed files are not all put in place by the commit of one transaction");
    for n in 1..=renames {
        let dir = tempfile::tempdir().unwrap();
        let game = setup(dir.path());
        let package = realistic(dir.path());
        faults::arm_at(Point::Rename, n, Fault::Fail(std::io::ErrorKind::Other));
        let result = install(&game, &package);
        assert!(faults::disarm(), "rename {n} of {renames} was not reached");
        let err = result.unwrap_err();
        let what = format!("rename {n} of {renames} fails: {err}");
        assert!(err.to_string().contains("nothing was installed"), "{what}");
        assert_same(&what, &old, &state(&game));
        assert_clean(&game, &what);
        install(&game, &package).unwrap_or_else(|e| panic!("{what}: the next install failed: {e}"));
        assert_same(&format!("{what}: the next install"), &new, &state(&game));
    }
}
