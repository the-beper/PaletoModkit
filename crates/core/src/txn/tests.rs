use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use super::journal::{self, Mode as JMode, OpKind, ParseError};
use super::*;
use crate::fixtures::{Entry, ReadOnly, is_root, snapshot};
use crate::fsroot::{Caps, Owned};
use crate::fsroot::faults::{self, Mode as Fault, Point};
use crate::game::Edition;

fn rel(p: &str) -> RelPath {
    RelPath::parse(p).unwrap()
}

/// A synthetic game folder: loose files, originals in `x64`, and (if asked) a mods folder with a copy.
fn fixture(dir: &Path, with_mods: bool) -> Game {
    let root = dir.join("game");
    for d in ["scripts", "x64", "update"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join(Edition::Enhanced.exe_name()), b"exe").unwrap();
    std::fs::write(root.join("scripts/orig.asi"), b"orig").unwrap();
    std::fs::write(root.join("scripts/old.asi"), b"old").unwrap();
    std::fs::write(root.join("x64/a.rpf"), b"the original archive").unwrap();
    if with_mods {
        std::fs::create_dir_all(root.join("mods/x64")).unwrap();
        std::fs::write(root.join("mods/x64/a.rpf"), b"a copy of the archive").unwrap();
    }
    Game::open(&root).unwrap()
}

/// Everything under the game folder except the tool's own `.paleto` folder.
fn state(game: &Game) -> BTreeMap<String, Entry> {
    snapshot(&game.root).into_iter().filter(|(k, _)| k != ".paleto" && !k.starts_with(".paleto/")).collect()
}

fn journals(game: &Game) -> Vec<String> {
    std::fs::read_dir(game.root.join(".paleto/journal")).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// No journal and no `.paleto-*` name anywhere.
fn assert_clean(game: &Game, what: &str) {
    assert_eq!(journals(game), Vec::<String>::new(), "{what}: a journal is left");
    assert_eq!(leftovers(game), Vec::<PathBuf>::new(), "{what}: temporary names are left");
}

/// A create with two new folders, a replace, a delete, and in the mods folder a replace and a nested create.
fn scenario(game: &Game) -> Result<Vec<String>, Error> {
    let roots = game.roots()?;
    let mut t = Txn::begin(&roots, "test")?;
    let mut d = roots.dest(&rel("plugins/deep/cfg.ini"))?;
    t.stage(&mut d, |f| f.write_all(b"new cfg"))?;
    let mut d = roots.dest(&rel("scripts/orig.asi"))?;
    t.stage(&mut d, |f| f.write_all(b"replaced content"))?;
    let d = roots.dest(&rel("scripts/old.asi"))?;
    t.delete(&d)?;
    let mut d = roots.dest(&rel("x64/a.rpf"))?;
    t.stage(&mut d, |f| f.write_all(b"a new archive"))?;
    let mut d = roots.dest(&rel("update/x64/dlcpacks/p/dlc.rpf"))?;
    t.stage(&mut d, |f| f.write_all(b"a pack"))?;
    t.commit(vec!["done".to_string()])
}

/// The folder before and after an unfaulted run.
fn references(with_mods: bool) -> (BTreeMap<String, Entry>, BTreeMap<String, Entry>) {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), with_mods);
    let old = state(&game);
    assert_eq!(scenario(&game).unwrap(), ["done"]);
    assert_clean(&game, "unfaulted");
    let new = state(&game);
    assert_ne!(old, new);
    (old, new)
}

fn journal_says_committed(game: &Game) -> bool {
    journals(game).iter().any(|n| std::fs::read_to_string(game.root.join(".paleto/journal").join(n)).unwrap_or_default().lines().any(|l| l.starts_with("committed ")))
}

/// What a crashed run must come to: with a journal, the old files unless it says `committed`;
/// without one, nothing was begun or everything was finished, so what is there (which must be one of the two).
fn expected(game: &Game, old: &BTreeMap<String, Entry>, new: &BTreeMap<String, Entry>) -> (BTreeMap<String, Entry>, bool) {
    if journals(game).is_empty() {
        let now = state(game);
        assert!(now == *old || now == *new, "no journal, yet the files are half way: {now:?}");
        return (now, false);
    }
    let committed = journal_says_committed(game);
    (if committed { new.clone() } else { old.clone() }, committed)
}

fn without_temps(mut s: BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    s.retain(|k, _| !k.rsplit('/').next().unwrap().starts_with(".paleto-"));
    s
}

/// A crash between making a folder and journaling that it was made leaves that empty folder behind: a rollback
/// removes only a folder its journal says was made by it, since one without that record may be another program's.
/// Everything else must be exactly as it should be.
fn minus_leaked_dirs(mut now: BTreeMap<String, Entry>, want: &BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    loop {
        let leaked: Vec<String> = now.iter().filter(|(k, e)| **e == Entry::Dir && !want.contains_key(*k) && !now.keys().any(|o| o.starts_with(&format!("{k}/")))).map(|(k, _)| k.clone()).collect();
        if leaked.is_empty() {
            return now;
        }
        for k in leaked {
            now.remove(&k);
        }
    }
}

/// Fails (or crashes at) the k-th system call of the scenario, for every k until none is left to hit.
/// Returns the number of points hit.
fn exhaust(mode: Fault, caps: Caps, with_mods: bool) -> u64 {
    let (old, new) = references(with_mods);
    let crash = matches!(mode, Fault::Crash);
    let (mut saw_committed, mut saw_error) = (false, false);
    for k in 1.. {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), with_mods);
        let _caps = faults::caps(caps);
        faults::arm(k, mode);
        let result = scenario(&game);
        if !faults::disarm() {
            result.unwrap();
            assert_eq!(state(&game), new, "no fault");
            assert!(saw_error, "the scenario hit no fault point at all");
            assert!(!crash || saw_committed, "no crash happened after `committed`");
            return k - 1;
        }
        let what = format!("{mode:?} at {k}, caps {caps:?}");
        if crash {
            let (want, committed) = expected(&game, &old, &new);
            let report = recover(&game).unwrap();
            assert!(report.failures.is_empty(), "{what}: {report:?}");
            assert_eq!(minus_leaked_dirs(state(&game), &want), want, "{what}: committed={committed}");
            saw_committed |= committed;
            saw_error |= want == old;
        } else {
            match result {
                Err(e) => {
                    assert_eq!(state(&game), old, "{what}: {e}");
                    saw_error = true;
                    assert!(e.to_string().contains("nothing was installed") || !matches!(e, Error::Txn(_)), "{what}: {e}");
                }
                Ok(_) => {
                    // The fault hit the cleanup after `committed`: the files are in place and the next run finishes it.
                    assert_eq!(without_temps(state(&game)), new, "{what}");
                    drop(Txn::begin(&game.roots().unwrap(), "next").unwrap());
                }
            }
        }
        assert_clean(&game, &what);
        // Whatever happened, the next transaction works.
        scenario(&game).unwrap_or_else(|e| panic!("{what}: the next transaction failed: {e}"));
    }
    unreachable!()
}

#[test]
fn every_failure_point_leaves_the_files_as_they_were() {
    let n = exhaust(Fault::Fail(std::io::ErrorKind::Other), Caps::ALL, true);
    assert!(n > 100, "only {n} fault points: the engine is not announcing its system calls");
}

#[test]
fn every_failure_point_with_no_mods_folder_yet() {
    exhaust(Fault::Fail(std::io::ErrorKind::Other), Caps::ALL, false);
}

#[test]
fn every_failure_point_without_hard_links() {
    exhaust(Fault::Fail(std::io::ErrorKind::Other), Caps { hard_links: false, ..Caps::ALL }, true);
}

#[test]
fn every_failure_point_without_renameat2_and_openat2() {
    exhaust(Fault::Fail(std::io::ErrorKind::Other), Caps { noreplace: false, openat2: false, ..Caps::ALL }, true);
}

#[test]
fn every_crash_point_is_recovered() {
    exhaust(Fault::Crash, Caps::ALL, true);
}

#[test]
fn every_crash_point_with_no_mods_folder_yet() {
    exhaust(Fault::Crash, Caps::ALL, false);
}

#[test]
fn every_crash_point_without_hard_links() {
    exhaust(Fault::Crash, Caps { hard_links: false, ..Caps::ALL }, true);
}

#[test]
fn every_crash_point_without_renameat2_and_openat2() {
    exhaust(Fault::Crash, Caps { noreplace: false, openat2: false, ..Caps::ALL }, true);
}

#[test]
fn a_recovery_that_crashes_again_is_recovered_by_the_next_one() {
    let (old, new) = references(true);
    for k in (1..).step_by(5) {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        faults::arm(k, Fault::Crash);
        let _ = scenario(&game);
        if !faults::disarm() {
            break;
        }
        let (want, _) = expected(&game, &old, &new);
        // Each attempt dies at its j-th call, having got further than the one before.
        for j in 1.. {
            faults::arm(j, Fault::Crash);
            let _ = recover(&game);
            if !faults::disarm() {
                break;
            }
        }
        assert_eq!(minus_leaked_dirs(state(&game), &want), want, "k={k}");
        assert_clean(&game, &format!("k={k}"));
    }
}

#[test]
fn a_crashed_transaction_is_recovered_by_the_next_begin() {
    let (old, _) = references(true);
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    // The 2nd rename of the commit: some files are in place, others not.
    faults::arm_at(Point::Rename, 2, Fault::Crash);
    assert!(scenario(&game).is_err());
    assert!(faults::disarm());
    assert_ne!(state(&game), old, "the crash left the files half way");
    let roots = game.roots().unwrap();
    let t = Txn::begin(&roots, "next").unwrap();
    assert!(t.recovered().iter().any(|l| l.contains("undid")), "{:?}", t.recovered());
    drop(t);
    assert_eq!(state(&game), old);
    assert_clean(&game, "after begin");
}

#[test]
fn a_dropped_transaction_is_undone() {
    let (old, _) = references(true);
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    {
        let mut t = Txn::begin(&roots, "dropped").unwrap();
        let mut d = roots.dest(&rel("plugins/x/y.ini")).unwrap();
        t.stage(&mut d, |f| f.write_all(b"never")).unwrap();
        assert!(game.root.join("plugins/x").is_dir());
    }
    assert_eq!(state(&game), old);
    assert_clean(&game, "dropped");
}

#[test]
fn a_panic_in_the_middle_releases_the_lock_and_undoes_the_work() {
    let (old, _) = references(true);
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let g = game.clone();
    let r = std::thread::spawn(move || {
        let roots = g.roots().unwrap();
        let mut t = Txn::begin(&roots, "panics").unwrap();
        let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
        t.stage(&mut d, |f| f.write_all(b"half")).unwrap();
        panic!("the installer died");
    })
    .join();
    assert!(r.is_err());
    assert_eq!(state(&game), old);
    assert_clean(&game, "after the panic");
    scenario(&game).unwrap();
}

fn unfinished_rollback() -> (tempfile::TempDir, Game, Error) {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "damaged").unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"replaced content")).unwrap();
    let d = roots.dest(&rel("scripts/old.asi")).unwrap();
    t.delete(&d).unwrap();
    // The delete is the second rename: the replace is in place when it fails.
    faults::arm_at(Point::Rename, 2, Fault::Fail(std::io::ErrorKind::Other));
    let scripts = game.root.join("scripts");
    ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || damage(&scripts))));
    let e = t.commit(vec!["log".into()]).unwrap_err();
    assert!(faults::disarm());
    (dir, game, e)
}

thread_local! {
    static DAMAGE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Makes the rollback fail: 0 the folder becomes read-only, 1 the backup of the replaced file disappears.
fn damage(scripts: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if DAMAGE.with(|d| d.get()) == 0 {
        std::fs::set_permissions(scripts, std::fs::Permissions::from_mode(0o555)).unwrap();
    } else {
        for e in std::fs::read_dir(scripts).unwrap().flatten() {
            if e.file_name().to_string_lossy().ends_with(".old") && std::fs::read(e.path()).unwrap() == b"orig" {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }
}

fn assert_stuck(game: &Game, e: &Error) {
    let Error::Txn(f) = e else { panic!("not a transaction failure: {e:?}") };
    assert!(!f.undone && !f.failures.is_empty(), "{f:?}");
    assert_eq!(f.log, ["log"]);
    let text = e.to_string();
    assert!(!text.contains("nothing was installed"), "{text}");
    assert!(text.contains("orig.asi") && text.contains(".paleto-") && text.contains(".old"), "names what is left and where the backup is: {text}");
    assert_eq!(journals(game).len(), 1, "the journal is kept");
    let pending = pending(game);
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].state.clone(), pending[0].label.as_str()), (PendingState::Stuck, "damaged"));
    assert!(pending[0].files.iter().any(|p| p.to_string_lossy().ends_with(".old")), "{:?}", pending[0].files);
    let roots = game.roots().unwrap();
    match Txn::begin(&roots, "again") {
        Err(Error::RecoveryNeeded(m)) => assert!(m.contains("orig.asi"), "{m}"),
        other => panic!("expected RecoveryNeeded, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_rollback_that_cannot_finish_is_reported_and_kept_until_it_is_fixed() {
    if is_root() {
        return;
    }
    DAMAGE.with(|d| d.set(0));
    let (_dir, game, e) = unfinished_rollback();
    let (old, _) = references(true);
    assert_stuck(&game, &e);
    // Still read-only: recovery says so and keeps everything.
    let report = recover(&game).unwrap();
    assert!(!report.failures.is_empty() && report.rolled_back == 0, "{report:?}");
    assert_eq!(journals(&game).len(), 1);
    drop(ReadOnly::new(&game.root.join("scripts")));
    // Fixed (the folder is writable again): recovery finishes.
    let report = recover(&game).unwrap();
    assert!(report.failures.is_empty() && report.rolled_back == 1, "{report:?}");
    assert_eq!(state(&game), old);
    assert_clean(&game, "recovered");
    scenario(&game).unwrap();
}

#[test]
fn a_vanished_backup_is_reported_and_the_new_file_is_kept() {
    DAMAGE.with(|d| d.set(1));
    let (_dir, game, e) = unfinished_rollback();
    assert_stuck(&game, &e);
    // The replaced file stays as the transaction wrote it, since the original cannot be put back.
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"replaced content");
    let report = recover(&game).unwrap();
    assert!(!report.failures.is_empty(), "{report:?}");
    assert_eq!(journals(&game).len(), 1);
    // Whoever restores the original by hand (the same bytes and time) lets recovery finish.
    let path = game.root.join("scripts/orig.asi");
    let f = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
    (&f).write_all(b"orig").unwrap();
    // The original's modification time is no longer known to the test: the journal's record of it is what counts.
    let journal = std::fs::read_to_string(game.root.join(".paleto/journal").join(&journals(&game)[0])).unwrap();
    let want = journal.lines().find(|l| l.starts_with("op 0 replace")).unwrap().split(' ').nth(6).unwrap().to_string();
    let mtime_ns: i128 = want.strip_prefix("f:").unwrap().split('.').nth(3).unwrap().parse().unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(mtime_ns as u64)).unwrap();
    drop(f);
    let report = recover(&game).unwrap();
    assert!(report.failures.is_empty() && report.rolled_back == 1, "{report:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"orig");
    assert_clean(&game, "recovered by hand");
}

#[test]
fn a_target_changed_after_staging_is_a_conflict_and_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "conflict").unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"replaced content")).unwrap();
    let mut d = roots.dest(&rel("plugins/x.ini")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"new")).unwrap();
    // Another program (OpenIV under Wine, say) edits the file in the meantime.
    std::fs::write(game.root.join("scripts/orig.asi"), b"edited elsewhere").unwrap();
    let e = t.commit(Vec::new()).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e:?}");
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"edited elsewhere");
    assert!(!game.root.join("plugins").exists());
    assert_clean(&game, "conflict");
    // A file created meanwhile at a new name is a conflict too.
    let mut t = Txn::begin(&roots, "conflict 2").unwrap();
    let mut d = roots.dest(&rel("scripts/new.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"new")).unwrap();
    std::fs::write(game.root.join("scripts/new.asi"), b"somebody's").unwrap();
    assert!(matches!(t.commit(Vec::new()).unwrap_err(), Error::Conflict(_)));
    assert_eq!(std::fs::read(game.root.join("scripts/new.asi")).unwrap(), b"somebody's");
    assert_clean(&game, "conflict 2");
}

#[test]
fn a_file_that_is_replaced_by_a_link_or_deleted_is_noticed_too() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "x").unwrap();
    let d = roots.dest(&rel("scripts/old.asi")).unwrap();
    t.delete(&d).unwrap();
    std::fs::remove_file(game.root.join("scripts/old.asi")).unwrap();
    std::os::unix::fs::symlink("orig.asi", game.root.join("scripts/old.asi")).unwrap();
    assert!(matches!(t.commit(Vec::new()).unwrap_err(), Error::Conflict(_)));
    assert!(std::fs::symlink_metadata(game.root.join("scripts/old.asi")).unwrap().is_symlink());
    assert_clean(&game, "x");
}

#[test]
fn the_validation_hook_sees_the_staged_file_and_can_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "v").unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    let s = t.stage(&mut d, |f| f.write_all(b"replaced content")).unwrap();
    let mut buf = [0u8; 8];
    t.staged(s).read_at(&mut buf, 0).unwrap();
    assert_eq!(&buf, b"replaced");
    let e = t.commit_validated(Vec::new(), |_, f| {
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut &*f, &mut b)?;
        assert_eq!(b, b"replaced content");
        Err(Error::Package("the pack is not in the list".into()))
    });
    assert_eq!(e.unwrap_err().to_string(), "package: the pack is not in the list");
    assert_eq!(state(&game), before);
    assert_clean(&game, "refused");
}

#[test]
fn links_are_replaced_and_restored_as_links() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    symlink("orig.asi", game.root.join("scripts/alias.asi")).unwrap();
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "link").unwrap();
    let mut d = roots.dest(&rel("scripts/alias.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"now a file")).unwrap();
    drop(t);
    assert_eq!(state(&game), before);
    let mut t = Txn::begin(&roots, "link").unwrap();
    let mut d = roots.dest(&rel("scripts/alias.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"now a file")).unwrap();
    t.commit(Vec::new()).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/alias.asi")).unwrap(), b"now a file");
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"orig", "written through the link");
    assert_clean(&game, "link");
}

#[test]
fn what_the_policy_refuses_is_refused_by_the_engine_and_nothing_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "p").unwrap();
    let mut d = roots.dest(&rel("GTA5_Enhanced.exe")).unwrap();
    assert!(t.stage(&mut d, |f| f.write_all(b"evil")).is_err());
    assert_eq!(state(&game), before);
    assert!(t.stage(&mut roots.dest(&rel("scripts/orig.asi")).unwrap(), |f| f.write_all(b"x")).is_err(), "the transaction ended with the error");
    drop(t);
    // A folder where a file should go, and the same file twice.
    let mut t = Txn::begin(&roots, "p").unwrap();
    let mut d = roots.dest(&rel("scripts")).unwrap();
    assert!(t.stage(&mut d, |f| f.write_all(b"x")).is_err());
    drop(t);
    let mut t = Txn::begin(&roots, "p").unwrap();
    t.stage(&mut roots.dest(&rel("scripts/orig.asi")).unwrap(), |f| f.write_all(b"1")).unwrap();
    let e = t.stage(&mut roots.dest(&rel("SCRIPTS/ORIG.ASI")).unwrap(), |f| f.write_all(b"2")).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e:?}");
    assert_eq!(state(&game), before);
    assert_clean(&game, "refused");
}

#[test]
fn a_write_that_fails_undoes_the_transaction_and_says_what_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "w").unwrap();
    t.stage(&mut roots.dest(&rel("plugins/a.ini")).unwrap(), |f| f.write_all(b"1")).unwrap();
    let e = t.stage(&mut roots.dest(&rel("plugins/b.ini")).unwrap(), |_| Err(std::io::Error::other("disk full"))).unwrap_err();
    assert_eq!(e.to_string(), "disk full");
    assert_eq!(state(&game), before);
    assert_clean(&game, "failed write");
}

#[test]
fn replace_one_is_a_one_operation_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    replace_one(&roots, &rel("scripts/orig.asi"), "one", |f| f.write_all(b"one")).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"one");
    replace_one(&roots, &rel("new/dir/file.cfg"), "one", |f| f.write_all(b"two")).unwrap();
    assert_eq!(std::fs::read(game.root.join("new/dir/file.cfg")).unwrap(), b"two");
    assert!(replace_one(&roots, &rel("scripts/orig.asi"), "one", |_| Err(std::io::Error::other("no"))).is_err());
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"one");
    assert_clean(&game, "replace_one");
    assert!(std::fs::read(game.root.join("x64/a.rpf")).unwrap() == b"the original archive", "the original was never touched");
}

#[test]
fn only_one_transaction_is_open_per_game_handle() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let t = Txn::begin(&roots, "outer").unwrap();
    assert!(matches!(Txn::begin(&roots, "inner"), Err(Error::Busy(m)) if m.contains("outer")));
    // Helpers that only lock may run meanwhile.
    drop(roots.lock("helper").unwrap());
    drop(t);
    drop(Txn::begin(&roots, "after").unwrap());
}

#[test]
fn leftovers_lists_temporary_names_that_no_journal_accounts_for() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    assert!(leftovers(&game).is_empty());
    std::fs::write(game.root.join("scripts/.paleto-save-1a2b.new"), b"x").unwrap();
    std::fs::write(game.root.join("mods/x64/.paleto-0123456789abcdef-3.old"), b"x").unwrap();
    std::fs::write(game.root.join("x64/.paleto-in-the-originals"), b"x").unwrap();
    std::fs::write(game.root.join("scripts/not-ours.new"), b"x").unwrap();
    let found: Vec<String> = leftovers(&game).iter().map(|p| p.strip_prefix(&game.root).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, ["mods/x64/.paleto-0123456789abcdef-3.old", "scripts/.paleto-save-1a2b.new"]);
    // The names of a journal that is waiting for recovery belong to it.
    faults::arm_at(Point::Rename, 2, Fault::Crash);
    assert!(scenario(&game).is_err());
    faults::disarm();
    let found: Vec<String> = leftovers(&game).iter().map(|p| p.strip_prefix(&game.root).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, ["mods/x64/.paleto-0123456789abcdef-3.old", "scripts/.paleto-save-1a2b.new"], "{found:?}");
    assert_eq!(pending(&game).len(), 1);
    assert_eq!(pending(&game)[0].state, PendingState::Interrupted);
}

#[test]
fn the_failure_message_says_nothing_was_installed_only_when_that_is_true() {
    let clean = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: true, failures: Vec::new(), journaled: true, verb: "installed" };
    assert_eq!(clean.to_string(), "package: boom (nothing was installed)");
    let left = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: false, failures: vec!["/g/a.asi: gone (backup: /g/.paleto-1-0.old)".into()], journaled: true, verb: "installed" };
    let text = left.to_string();
    assert!(!text.contains("nothing was installed") && text.contains("/g/.paleto-1-0.old") && text.contains("journal"), "{text}");
    let odd = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: true, failures: vec!["x".into()], journaled: true, verb: "installed" };
    assert!(!odd.to_string().contains("nothing was installed"));
}

// ---- the journal file ----

fn sample() -> String {
    let fp = Fingerprint { dev: 7, ino: 99, size: 12, mtime_ns: -5, head_tail: [0xAB; 32] };
    let op = |kind, name: &str, expect| journal::JOp { kind, anchor: RootKind::Mods, dir: vec!["a b".into(), "%x".into()], name: name.into(), expect, mode: JMode::Link, made: None, kept: false, did: false };
    [
        journal::header(),
        journal::begin("0123456789abcdef", 42, 1700000000, "install A \u{e9} 100%\nfoo"),
        journal::root(&journal::RootRec { kind: RootKind::Mods, fp, shown: "/games/my game/mods".into() }),
        journal::op(0, &op(OpKind::Replace, "name with space.rpf", Some(Existing::File(fp)))),
        journal::made(0, &fp),
        journal::op(1, &op(OpKind::Delete, "l", Some(Existing::Link("../t\u{e9} t".into())))),
        journal::op(2, &journal::JOp { dir: Vec::new(), ..op(OpKind::Mkdir, "d", None) }),
        journal::kept(2),
        journal::commit(),
        journal::did(0),
    ]
    .concat()
}

#[test]
fn the_journal_round_trips_awkward_names() {
    let j = journal::parse(sample().as_bytes()).unwrap();
    assert_eq!((j.txn.as_str(), j.pid, j.started, j.label.as_str()), ("0123456789abcdef", 42, 1700000000, "install A \u{e9} 100%\nfoo"));
    assert_eq!(j.roots[0].shown, "/games/my game/mods");
    assert_eq!(j.ops.len(), 3);
    assert_eq!((j.ops[0].dir.clone(), j.ops[0].name.as_str(), j.ops[0].kind, j.ops[0].mode), (vec!["a b".to_string(), "%x".into()], "name with space.rpf", OpKind::Replace, JMode::Link));
    assert!(matches!(&j.ops[0].expect, Some(Existing::File(fp)) if fp.mtime_ns == -5 && fp.head_tail == [0xAB; 32]));
    assert!(j.ops[0].made.is_some() && j.ops[0].did);
    assert!(matches!(&j.ops[1].expect, Some(Existing::Link(t)) if t == "../t\u{e9} t"));
    assert!(j.ops[2].kept && j.ops[2].dir.is_empty());
    assert!(j.commit && !j.committed && j.end.is_none());
    assert_eq!(j.state(), journal::State::Interrupted);
}

#[test]
fn a_torn_last_line_is_ignored() {
    let full = sample();
    let last = full.trim_end().rsplit('\n').next().unwrap().len();
    for cut in 1..=last {
        // The record that was being written when the process died: any prefix of the last line.
        let torn = &full.as_bytes()[..full.len() - cut];
        let j = journal::parse(torn).unwrap_or_else(|e| panic!("cut {cut}: {e:?}"));
        // Without only its newline the last record is still whole.
        assert_eq!(j.ops[0].did, cut == 1, "cut {cut}");
        assert!(j.commit);
    }
    let j = journal::parse(&full.as_bytes()[..full.len() - last - 1]).unwrap();
    assert!(j.commit && !j.ops[0].did, "a line removed whole");
    // Garbage after the last good line is a torn line too (blocks zeroed by a crash).
    let mut zeroed = full.clone().into_bytes();
    zeroed.extend([0u8; 64]);
    assert!(journal::parse(&zeroed).unwrap().ops[0].did);
    // Only the header torn: nothing was ever recorded.
    assert_eq!(journal::parse(&journal::header().as_bytes()[..10]).unwrap_err(), ParseError::Empty);
    assert_eq!(journal::parse(b"").unwrap_err(), ParseError::Empty);
}

#[test]
fn a_damaged_record_in_the_middle_makes_the_journal_unreadable() {
    let full = sample();
    for line in 1..full.lines().count() - 1 {
        let mut lines: Vec<String> = full.lines().map(String::from).collect();
        let bytes = unsafe { lines[line].as_bytes_mut() };
        let at = bytes.len() / 2;
        bytes[at] = if bytes[at] == b'x' { b'y' } else { b'x' };
        let damaged = lines.join("\n") + "\n";
        assert!(matches!(journal::parse(damaged.as_bytes()), Err(ParseError::Corrupt(_))), "line {line}: {damaged}");
    }
    // A record whose checksum is right but whose meaning is not.
    let mut forged = full.clone();
    forged.push_str(&journal::did(9));
    forged.push_str(&journal::commit());
    assert!(matches!(journal::parse(forged.as_bytes()), Err(ParseError::Corrupt(_))));
}

#[test]
fn an_unknown_version_is_refused() {
    let v2 = sample().replacen("paleto-txn 1", "paleto-txn 2", 1);
    assert_eq!(journal::parse(v2.as_bytes()).unwrap_err(), ParseError::Version("2".into()));
    // Even with a checksum of some future kind.
    assert_eq!(journal::parse(b"paleto-txn 2 ~whatever\nsomething new\n").unwrap_err(), ParseError::Version("2".into()));
}

#[test]
fn a_journal_that_cannot_be_read_blocks_nothing_it_names_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    faults::arm_at(Point::Rename, 2, Fault::Crash);
    assert!(scenario(&game).is_err());
    faults::disarm();
    let before = state(&game);
    let jpath = game.root.join(".paleto/journal").join(&journals(&game)[0]);
    // Damage a record in the middle.
    let text = std::fs::read_to_string(&jpath).unwrap();
    let damaged = text.replacen("op 1 ", "op 7 ", 1);
    assert_ne!(damaged, text);
    std::fs::write(&jpath, damaged).unwrap();
    let report = recover(&game).unwrap();
    assert!(report.failures.len() == 1 && report.failures[0].contains("damaged"), "{report:?}");
    assert_eq!(state(&game), before, "nothing was deleted or restored on the word of a damaged journal");
    assert!(leftovers(&game).len() >= 2, "the temporary names are still there for a person to look at");
    let roots = game.roots().unwrap();
    assert!(matches!(Txn::begin(&roots, "x"), Err(Error::RecoveryNeeded(_))));
    let p = pending(&game);
    assert!(matches!(&p[0].state, PendingState::Unreadable(_)), "{p:?}");
    // The same with a version this program does not know.
    std::fs::write(&jpath, text.replacen("paleto-txn 1", "paleto-txn 9", 1)).unwrap();
    let report = recover(&game).unwrap();
    assert!(report.failures[0].contains("newer version"), "{report:?}");
    assert_eq!(state(&game), before);
    // A journal that is only a torn header is nothing: removed.
    std::fs::write(&jpath, &journal::header().as_bytes()[..9]).unwrap();
    assert!(recover(&game).unwrap().failures.is_empty());
    assert!(journals(&game).is_empty());
}

#[test]
fn many_files_in_one_folder_share_a_handle_and_can_be_released() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "many").unwrap();
    for n in 0..300 {
        let mut d = roots.dest(&rel(&format!("scripts/many/f{n}.asi"))).unwrap();
        let s = t.stage(&mut d, |f| f.write_all(format!("file {n}").as_bytes())).unwrap();
        t.release(s);
    }
    assert!(t.dirs.len() >= 300 && t.dir_cache.len() <= 3, "{} folders for {} operations", t.dir_cache.len(), t.dirs.len());
    t.commit(Vec::new()).unwrap();
    assert_eq!(std::fs::read(game.root.join("scripts/many/f299.asi")).unwrap(), b"file 299");
    assert_eq!(std::fs::read_dir(game.root.join("scripts/many")).unwrap().count(), 300);
    assert_clean(&game, "many");
}

#[test]
fn a_replaced_file_keeps_its_permission_bits_and_a_new_file_gets_0644() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let mode = |p: &str| std::fs::metadata(game.root.join(p)).unwrap().permissions().mode() & 0o777;
    std::fs::set_permissions(game.root.join("scripts/orig.asi"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(game.root.join("mods/x64/a.rpf"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let roots = game.roots().unwrap();
    for (path, was) in [("scripts/orig.asi", 0o755), ("x64/a.rpf", 0o600)] {
        replace_one(&roots, &rel(path), "test", |f| f.write_all(b"new")).unwrap();
        let real = if path.starts_with("x64") { "mods/x64/a.rpf" } else { path };
        assert_eq!((std::fs::read(game.root.join(real)).unwrap(), mode(real)), (b"new".to_vec(), was), "{path}");
    }
    replace_one(&roots, &rel("scripts/brand_new.asi"), "test", |f| f.write_all(b"x")).unwrap();
    assert_eq!(mode("scripts/brand_new.asi"), 0o644);
    assert_clean(&game, "modes");
}

#[test]
fn a_replacement_never_carries_setuid_and_the_owner_can_always_read_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let mode = |p: &str| std::fs::metadata(game.root.join(p)).unwrap().permissions().mode() & 0o7777;
    std::fs::set_permissions(game.root.join("scripts/orig.asi"), std::fs::Permissions::from_mode(0o4755)).unwrap();
    // A file the owner may only write: the new file is opened again to be validated.
    std::fs::set_permissions(game.root.join("scripts/old.asi"), std::fs::Permissions::from_mode(0o200)).unwrap();
    let roots = game.roots().unwrap();
    for path in ["scripts/orig.asi", "scripts/old.asi"] {
        replace_one(&roots, &rel(path), "test", |f| f.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read(game.root.join(path)).unwrap(), b"new", "{path}");
    }
    assert_eq!((mode("scripts/orig.asi"), mode("scripts/old.asi")), (0o755, 0o600));
    assert_clean(&game, "modes");
}

// ---- the security review of the engine ----

/// A transaction with a replace in the game folder and one in the mods folder, stopped by a crash at the
/// `at`-th record of the journal after staging (1 is `commit`, then one `did` per file, then `committed`).
fn crashed_two_file_change(game: &Game, at: u64) {
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "two files").unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"new script")).unwrap();
    let mut d = roots.dest(&rel("x64/a.rpf")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"new archive")).unwrap();
    faults::arm_at(Point::Journal, at, Fault::Crash);
    assert!(t.commit(Vec::new()).is_err());
    assert!(faults::disarm());
}

#[test]
fn a_journal_for_a_mods_folder_that_was_deleted_does_not_block_every_later_change() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    crashed_two_file_change(&game, 1);
    assert_eq!(journals(&game).len(), 1);
    std::fs::remove_dir_all(game.root.join("mods")).unwrap();
    // Nothing was put in place yet: what is left of it in the game folder is undone, and the change goes on.
    let roots = game.roots().unwrap();
    let t = Txn::begin(&roots, "next").unwrap();
    assert!(t.recovered().iter().any(|l| l.contains("undid")), "{:?}", t.recovered());
    drop(t);
    assert_clean(&game, "after the mods folder was deleted");
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"orig");
}

#[test]
fn an_operation_that_cannot_be_recovered_is_reported_and_can_be_abandoned_without_deleting_anything() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    // Both files are in place; only `committed` is missing.
    crashed_two_file_change(&game, 4);
    std::fs::remove_dir_all(game.root.join("mods")).unwrap();
    let roots = game.roots().unwrap();
    // The game file is put back; the file in the mods folder, which is gone, cannot be, and that is said.
    match Txn::begin(&roots, "next") {
        Err(Error::RecoveryNeeded(m)) => assert!(m.contains("two files") && m.contains("gone"), "{m}"),
        other => panic!("expected RecoveryNeeded, got {:?}", other.map(|_| ())),
    }
    assert!(matches!(Txn::begin(&roots, "again"), Err(Error::RecoveryNeeded(_))), "it stays stuck");
    let p = pending(&game);
    assert_eq!((p.len(), p[0].state.clone()), (1, PendingState::Stuck), "{p:?}");
    let before = state(&game);
    let report = abandon(&game, &p[0].journal).unwrap();
    // Every file and backup the journal names is listed, and none was touched.
    let said = report.log.join("\n");
    for f in &p[0].files {
        assert!(said.contains(&f.display().to_string()), "{} is not listed in {said}", f.display());
    }
    assert_eq!(state(&game), before);
    assert!(journals(&game).iter().all(|n| n == "abandoned"), "{:?}", journals(&game));
    let kept: Vec<String> = std::fs::read_dir(game.root.join(".paleto/journal/abandoned")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(kept, [p[0].journal.clone()]);
    assert!(pending(&game).is_empty());
    drop(Txn::begin(&roots, "now it works").unwrap());
    // What is not a journal's name is refused, and a name that is not there is not found.
    assert!(abandon(&game, "../x").is_err() && abandon(&game, "0123456789abcdef").is_err());
}

#[test]
fn the_fallback_home_holds_the_lock_and_journals_of_a_game_folder_that_cannot_be_written() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let data = dir.path().join("data");
    let roots_of = || {
        let mut roots = game.roots().unwrap();
        roots.set_data_dir(data.clone());
        roots
    };
    let ro = ReadOnly::new(&game.root);
    // The mods folder (here inside the read-only game folder, but a folder of its own) is still written.
    let roots = roots_of();
    let mut t = Txn::begin(&roots, "mods on another drive").unwrap();
    let mut d = roots.dest(&rel("x64/a.rpf")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"edited")).unwrap();
    t.commit(Vec::new()).unwrap();
    assert_eq!(std::fs::read(game.root.join("mods/x64/a.rpf")).unwrap(), b"edited");
    assert!(!game.root.join(".paleto").exists(), "nothing was made in the game folder");
    // A crash leaves its journal there, and recovery and `pending` look there.
    let mut t = Txn::begin(&roots, "interrupted").unwrap();
    let mut d = roots.dest(&rel("x64/a.rpf")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"never")).unwrap();
    faults::arm_at(Point::Rename, 1, Fault::Crash);
    assert!(t.commit(Vec::new()).is_err());
    assert!(faults::disarm());
    drop(roots);
    let roots = roots_of();
    let p = pending_in(&roots);
    assert_eq!((p.len(), p[0].state.clone(), p[0].label.as_str()), (1, PendingState::Interrupted, "interrupted"), "{p:?}");
    assert_eq!(leftovers_in(&roots), Vec::<PathBuf>::new(), "the names a journal there accounts for are not leftovers");
    let report = recover_in(&roots).unwrap();
    assert!(report.failures.is_empty() && report.rolled_back == 1, "{report:?}");
    assert_eq!(std::fs::read(game.root.join("mods/x64/a.rpf")).unwrap(), b"edited");
    assert!(pending_in(&roots).is_empty());
    assert_eq!(leftovers_in(&roots), Vec::<PathBuf>::new());
    drop(ro);
}

#[test]
fn a_folder_that_somebody_else_made_is_never_removed_by_a_rollback_even_if_the_crash_came_before_kept() {
    for k in 1.. {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        let roots = game.roots().unwrap();
        let mut t = Txn::begin(&roots, "foreign").unwrap();
        let mut d = roots.dest(&rel("plugins/x.ini")).unwrap();
        // Another program makes the folder after the destination was resolved and before this operation makes it.
        std::fs::create_dir(game.root.join("plugins")).unwrap();
        faults::arm_at(Point::Journal, k, Fault::Crash);
        let r = t.stage(&mut d, |f| f.write_all(b"x"));
        if !faults::disarm() {
            r.unwrap();
            assert!(k > 3, "the crash was tried before `kept`, at the `kept` record and after it");
            return;
        }
        drop(t);
        let report = recover(&game).unwrap();
        assert!(report.failures.is_empty(), "k={k}: {report:?}");
        assert!(game.root.join("plugins").is_dir(), "k={k}: the folder of the other program was removed");
        assert!(!game.root.join("plugins/x.ini").exists(), "k={k}");
        assert_clean(&game, &format!("k={k}"));
    }
}

#[test]
fn a_created_file_changed_after_it_was_put_in_place_is_a_failure_not_silence() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "create then fail").unwrap();
    let mut d = roots.dest(&rel("scripts/made.cfg")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"as made")).unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"replaced")).unwrap();
    // The replace is the second rename and fails; before the rollback begins, another program rewrites the created file.
    faults::arm_at(Point::Rename, 2, Fault::Fail(io::ErrorKind::Other));
    let made = game.root.join("scripts/made.cfg");
    ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || std::fs::write(&made, b"somebody else wrote a longer text").unwrap())));
    let e = t.commit(Vec::new()).unwrap_err();
    assert!(faults::disarm());
    let Error::Txn(f) = &e else { panic!("{e:?}") };
    assert!(!f.undone && f.failures.iter().any(|m| m.contains("made.cfg") && m.contains("changed by something else")), "{f:?}");
    assert!(!e.to_string().contains("nothing was installed"), "{e}");
    assert_eq!(std::fs::read(game.root.join("scripts/made.cfg")).unwrap(), b"somebody else wrote a longer text", "left as it is");
    assert_eq!(std::fs::read(game.root.join("scripts/orig.asi")).unwrap(), b"orig", "the rest was undone");
    assert_eq!(journals(&game).len(), 1, "the journal is kept");
}

#[test]
fn recovery_does_not_touch_what_the_policy_forbids_whatever_a_journal_says() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let exe = Edition::Enhanced.exe_name();
    let id = "0123456789abcdef";
    // What a planted journal could say: delete an archive of the game and its program, with matching "backups" next to them.
    let archive = roots.game.dir.open_dir("x64").unwrap().stat("a.rpf").unwrap();
    let program = roots.game.dir.stat(exe).unwrap();
    std::fs::hard_link(game.root.join("x64/a.rpf"), game.root.join(format!("x64/.paleto-{id}-0.old"))).unwrap();
    std::fs::hard_link(game.root.join(exe), game.root.join(format!(".paleto-{id}-1.old"))).unwrap();
    let op = |i, dir: Vec<String>, name: &str, expect| journal::op(i, &journal::JOp { kind: OpKind::Delete, anchor: RootKind::Game, dir, name: name.into(), expect, mode: JMode::Move, made: None, kept: false, did: false });
    let text = [
        journal::header(),
        journal::begin(id, 1, 1, "planted"),
        journal::root(&journal::RootRec { kind: RootKind::Game, fp: roots.game.dir.fingerprint().unwrap(), shown: roots.game.shown.display().to_string() }),
        op(0, vec!["x64".into()], "a.rpf", archive),
        op(1, Vec::new(), exe, program),
        journal::commit(),
    ]
    .concat();
    std::fs::create_dir_all(game.root.join(".paleto/journal")).unwrap();
    std::fs::write(game.root.join(".paleto/journal").join(format!("{id}.log")), text).unwrap();
    let before = state(&game);
    let report = recover(&game).unwrap();
    assert!(report.failures.len() == 2 && report.failures.iter().all(|f| f.contains("not touched")), "{report:?}");
    assert!(report.failures.iter().any(|f| f.contains("programs")), "{report:?}");
    assert_eq!(state(&game), before, "not a name was removed, renamed or created");
    // It stays, stuck, for a person to look at, and blocks other changes until it is abandoned.
    assert!(matches!(Txn::begin(&roots, "x"), Err(Error::RecoveryNeeded(_))));
    abandon(&game, id).unwrap();
    drop(Txn::begin(&roots, "x").unwrap());
}

/// A new top-level foo.dll, renamed into place, and then a crash before `did` is journaled.
fn crashed_after_a_new_dll_was_put_in_place(game: &Game) {
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "new dll").unwrap();
    let mut d = roots.dest(&rel("foo.dll")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"our new library")).unwrap();
    // The first record of the commit is `commit`, the second `did`.
    faults::arm_at(Point::Journal, 2, Fault::Crash);
    assert!(t.commit(Vec::new()).is_err());
    assert!(faults::disarm());
    assert_eq!(std::fs::read(game.root.join("foo.dll")).unwrap(), b"our new library");
}

#[test]
fn the_rule_for_the_games_own_dll_and_txt_files_does_not_stop_recovery_removing_a_file_it_made() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let old = state(&game);
    crashed_after_a_new_dll_was_put_in_place(&game);
    let report = recover(&game).unwrap();
    assert!(report.failures.is_empty() && report.rolled_back == 1, "{report:?}");
    assert_eq!(state(&game), old, "our file is removed and the originals were never touched");
    assert_clean(&game, "recovered");
    // A foo.dll that is not the file this operation made is somebody else's: reported, never touched.
    crashed_after_a_new_dll_was_put_in_place(&game);
    std::fs::write(game.root.join("foo.dll"), b"a library of somebody else, longer than ours").unwrap();
    let before = state(&game);
    let report = recover(&game).unwrap();
    assert!(report.failures.len() == 1 && report.failures[0].contains("not touched") && report.failures[0].contains("foo.dll"), "{report:?}");
    assert_eq!(state(&game), before);
    assert_eq!(std::fs::read(game.root.join("foo.dll")).unwrap(), b"a library of somebody else, longer than ours");
    assert_eq!(std::fs::read(game.root.join("x64/a.rpf")).unwrap(), b"the original archive");
}

#[test]
fn the_end_record_goes_at_the_end_of_the_last_whole_record_not_after_torn_bytes() {
    DAMAGE.with(|d| d.set(1));
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "torn").unwrap();
    let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
    t.stage(&mut d, |f| f.write_all(b"replaced content")).unwrap();
    let d = roots.dest(&rel("scripts/old.asi")).unwrap();
    t.delete(&d).unwrap();
    // What a failed append whose cut-off also failed leaves behind the known end of the journal.
    let path = game.root.join(".paleto/journal").join(&journals(&game)[0]);
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"half a record that was never cut off ~00000000\nand more").unwrap();
    drop(f);
    faults::arm_at(Point::Rename, 2, Fault::Fail(io::ErrorKind::Other));
    let scripts = game.root.join("scripts");
    ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || damage(&scripts))));
    let e = t.commit(vec!["log".into()]).unwrap_err();
    assert!(faults::disarm());
    assert!(matches!(&e, Error::Txn(f) if !f.undone), "{e:?}");
    let read = || journal::parse(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("the journal is damaged: {e:?}"));
    assert_eq!(read().state(), journal::State::Stuck);
    // The same for an earlier run's journal that ends in a torn record, tried again twice.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"torn tail without a newline").unwrap();
    drop(f);
    for _ in 0..2 {
        let report = recover(&game).unwrap();
        assert!(!report.failures.is_empty() && !report.failures.iter().any(|f| f.contains("damaged")), "{report:?}");
        assert_eq!(read().state(), journal::State::Stuck);
    }
}

#[test]
fn a_transaction_stops_before_it_commits_if_its_lock_or_journal_was_removed() {
    let (old, _) = references(true);
    for what in ["lock", "journal", "paleto folder"] {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        let roots = game.roots().unwrap();
        let mut t = Txn::begin(&roots, what).unwrap();
        let mut d = roots.dest(&rel("scripts/orig.asi")).unwrap();
        t.stage(&mut d, |f| f.write_all(b"replaced")).unwrap();
        let mut d = roots.dest(&rel("x64/a.rpf")).unwrap();
        t.stage(&mut d, |f| f.write_all(b"new archive")).unwrap();
        let paleto = game.root.join(".paleto");
        match what {
            "lock" => std::fs::remove_file(paleto.join("lock")).unwrap(),
            "journal" => std::fs::remove_file(paleto.join("journal").join(&journals(&game)[0])).unwrap(),
            _ => std::fs::rename(&paleto, dir.path().join("moved")).unwrap(),
        }
        let e = t.commit(Vec::new()).unwrap_err();
        assert!(e.to_string().contains("removed or replaced"), "{what}: {e}");
        assert_eq!(state(&game), old, "{what}: nothing was installed");
        assert_eq!(leftovers(&game), Vec::<PathBuf>::new(), "{what}");
    }
}

// ---- what the installers call: abort, failure, stage_sized, the tool's own folders ----

#[test]
fn abort_undoes_what_was_staged_and_says_nothing_was_installed_only_when_the_undo_was_clean() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "abort").unwrap();
    t.stage(&mut roots.dest(&rel("plugins/deep/a.ini")).unwrap(), |f| f.write_all(b"1")).unwrap();
    t.stage(&mut roots.dest(&rel("scripts/orig.asi")).unwrap(), |f| f.write_all(b"2")).unwrap();
    let e = t.abort(Error::Package("a command failed".into()), vec!["did this".into()]);
    let Error::Txn(f) = &e else { panic!("not a transaction failure: {e:?}") };
    assert!(f.undone && f.failures.is_empty(), "{f:?}");
    assert_eq!(f.log, ["did this"]);
    assert_eq!(e.to_string(), "package: a command failed (nothing was installed)");
    assert_eq!(state(&game), before);
    assert_clean(&game, "after the abort");
    drop(Txn::begin(&roots, "next").unwrap());
}

#[test]
fn an_abort_whose_undo_fails_keeps_the_journal_and_the_backups_and_never_says_nothing_was_installed() {
    if is_root() {
        return;
    }
    DAMAGE.with(|d| d.set(0));
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "abort").unwrap();
    t.stage(&mut roots.dest(&rel("scripts/orig.asi")).unwrap(), |f| f.write_all(b"replaced")).unwrap();
    let scripts = game.root.join("scripts");
    ON_ROLLBACK.with(|h| *h.borrow_mut() = Some(Box::new(move || damage(&scripts))));
    let e = t.abort(Error::Package("a command failed".into()), vec!["log".into()]);
    let Error::Txn(f) = &e else { panic!("not a transaction failure: {e:?}") };
    assert!(!f.undone && !f.failures.is_empty() && f.journaled, "{f:?}");
    assert_eq!(f.log, ["log"]);
    let text = e.to_string();
    assert!(!text.contains("nothing was") && text.contains("a command failed") && text.contains(".paleto-"), "{text}");
    assert_eq!(journals(&game).len(), 1, "the journal is kept");
    let kept: Vec<String> = std::fs::read_dir(game.root.join("scripts")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".paleto-")).collect();
    assert!(kept.iter().any(|n| n.ends_with(".old")), "the backup is kept: {kept:?}");
    assert!(matches!(Txn::begin(&roots, "again"), Err(Error::RecoveryNeeded(_))));
    // Fixed (the folder is writable again): recovery finishes the undo.
    drop(ReadOnly::new(&game.root.join("scripts")));
    let report = recover(&game).unwrap();
    assert!(report.failures.is_empty() && report.rolled_back == 1, "{report:?}");
    assert_eq!(state(&game), before);
    assert_clean(&game, "recovered");
}

#[test]
fn failure_gives_the_callers_log_to_a_plain_error_and_passes_a_transaction_failure_through() {
    let plain = failure(Error::Package("boom".into()), vec!["a".into()], "changed");
    let Error::Txn(f) = &plain else { panic!("not wrapped: {plain:?}") };
    assert!(f.undone && f.failures.is_empty() && f.log == ["a"], "{f:?}");
    assert_eq!(plain.to_string(), "package: boom (nothing was changed)");
    // A real error of `stage`: the undo was clean.
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "w").unwrap();
    let e = t.stage(&mut roots.dest(&rel("plugins/b.ini")).unwrap(), |_| Err(std::io::Error::other("disk full"))).unwrap_err();
    assert_eq!(failure(e, vec!["x".into()], "installed").to_string(), "disk full (nothing was installed)");
    assert_clean(&game, "after the failed stage");
    // One that already says what is left passes through: the caller's log goes in front of its own, the verb is the caller's.
    let left = Error::Txn(Box::new(TxnFailure { cause: Error::Package("inner".into()), log: vec!["b".into()], undone: false, failures: vec!["/g/x: gone (backup: /g/.paleto-1-0.old)".into()], journaled: true, verb: "installed" }));
    let e = failure(left, vec!["a".into()], "changed");
    let Error::Txn(f) = &e else { panic!("not a transaction failure: {e:?}") };
    assert!(!f.undone && f.log == ["a", "b"] && f.verb == "changed", "{f:?}");
    let text = e.to_string();
    assert!(!text.contains("nothing was") && text.contains("/g/.paleto-1-0.old") && text.contains("inner"), "{text}");
    let clean = failure(failure(Error::Package("boom".into()), vec!["b".into()], "installed"), vec!["a".into()], "changed");
    assert_eq!(clean.to_string(), "package: boom (nothing was changed)");
    assert!(matches!(&clean, Error::Txn(f) if f.log == ["a", "b"]));
}

#[test]
fn stage_sized_refuses_what_would_not_leave_a_gigabyte_free_before_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let game = fixture(dir.path(), true);
    let before = state(&game);
    let roots = game.roots().unwrap();
    // An impossible size, and the smallest one whose sum with the margin overflows.
    for need in [u64::MAX, u64::MAX - FREE_MARGIN + 1] {
        let mut t = Txn::begin(&roots, "big").unwrap();
        t.stage(&mut roots.dest(&rel("scripts/first.asi")).unwrap(), |f| f.write_all(b"1")).unwrap();
        let mut d = roots.dest(&rel("plugins/deep/big.bin")).unwrap();
        let e = t.stage_sized(&mut d, need, |_| panic!("nothing may be written")).unwrap_err();
        let text = e.to_string();
        assert!(matches!(&e, Error::Io(io) if io.kind() == io::ErrorKind::StorageFull), "{e:?}");
        assert!(text.contains("plugins/deep") && text.contains("GiB") && text.contains("free"), "{text}");
        assert_eq!(state(&game), before, "no file and no folder was made, and the file staged before was undone");
        assert_clean(&game, "after the refusal");
        assert!(t.stage(&mut d, |f| f.write_all(b"x")).is_err(), "the transaction is over");
    }
    // A modest file is staged, when this filesystem has the space.
    if roots.game.dir.free_space().unwrap() > 2 * FREE_MARGIN {
        let mut t = Txn::begin(&roots, "small").unwrap();
        let mut d = roots.dest(&rel("plugins/deep/small.bin")).unwrap();
        t.stage_sized(&mut d, 1024, |f| f.write_all(b"small")).unwrap();
        t.commit(Vec::new()).unwrap();
        assert_eq!(std::fs::read(game.root.join("plugins/deep/small.bin")).unwrap(), b"small");
    }
}

/// The folders and files `owned_scenario` works on: a replaced part and a deleted one in `mods_disabled`
/// (and, if `folders`, the folders it would make).
fn owned_game(dir: &Path, folders: bool) -> Game {
    let game = crate::fixtures::game_folder(dir, Edition::Enhanced);
    std::fs::create_dir_all(game.root.join("mods_disabled")).unwrap();
    std::fs::write(game.root.join("mods_disabled/old.asi"), b"old").unwrap();
    std::fs::write(game.root.join("mods_disabled/gone.asi"), b"gone").unwrap();
    if folders {
        std::fs::create_dir_all(game.root.join("mods_legacy/20260101/x64")).unwrap();
    }
    game
}

/// A backup created below `mods_legacy`, a part replaced and a part deleted in `mods_disabled`.
fn owned_scenario(game: &Game) -> Result<Vec<String>, Error> {
    let roots = game.roots()?;
    let mut t = Txn::begin(&roots, "owned")?;
    t.stage(&mut roots.owned_dest(Owned::Legacy, &rel("20260101/x64/a.rpf"))?, |f| f.write_all(b"backup of a"))?;
    t.stage(&mut roots.owned_dest(Owned::Disabled, &rel("old.asi"))?, |f| f.write_all(b"new"))?;
    t.delete(&roots.owned_dest(Owned::Disabled, &rel("gone.asi"))?)?;
    t.commit(vec!["done".to_string()])
}

#[test]
fn a_transaction_writes_in_the_tools_own_folders_and_makes_the_ones_that_are_missing() {
    let dir = tempfile::tempdir().unwrap();
    let game = owned_game(dir.path(), false);
    assert_eq!(owned_scenario(&game).unwrap(), ["done"]);
    let root = &game.root;
    assert_eq!(std::fs::read(root.join("mods_legacy/20260101/x64/a.rpf")).unwrap(), b"backup of a");
    assert_eq!(std::fs::read(root.join("mods_disabled/old.asi")).unwrap(), b"new");
    assert!(!root.join("mods_disabled/gone.asi").exists());
    assert_clean(&game, "after");
    // A name changed twice in one transaction is a conflict, as for any file.
    let roots = game.roots().unwrap();
    let mut t = Txn::begin(&roots, "twice").unwrap();
    t.stage(&mut roots.owned_dest(Owned::Legacy, &rel("20260101/x64/a.rpf")).unwrap(), |f| f.write_all(b"again")).unwrap();
    let e = t.stage(&mut roots.owned_dest(Owned::Legacy, &rel("20260101/x64/a.rpf")).unwrap(), |f| f.write_all(b"twice")).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e:?}");
    assert_eq!(std::fs::read(root.join("mods_legacy/20260101/x64/a.rpf")).unwrap(), b"backup of a");
}

#[test]
fn a_crash_after_the_folders_were_made_is_rolled_back_by_recovery_and_the_folders_go() {
    // The three renames of the commit: the backup is created, the part replaced, the part deleted.
    for k in 1..=3 {
        let dir = tempfile::tempdir().unwrap();
        let game = owned_game(dir.path(), false);
        let before = state(&game);
        faults::arm_at(Point::Rename, k, Fault::Crash);
        let _ = owned_scenario(&game);
        assert!(faults::disarm(), "no rename {k}");
        let report = recover(&game).unwrap();
        assert!(report.failures.is_empty() && report.rolled_back == 1, "k={k}: {report:?}");
        assert_eq!(state(&game), before, "k={k}: the tool's folders, the ones made included, are as they were");
        assert_clean(&game, &format!("k={k}"));
    }
}

#[test]
fn every_failure_point_of_a_change_in_the_tools_own_folders_leaves_them_as_they_were() {
    let setup = |dir: &Path| (owned_game(dir, true), ());
    for mode in [Fault::Fail(std::io::ErrorKind::Other), Fault::Crash] {
        // Whatever happened, the next run works.
        let check = |game: &Game, _: &(), what: &str, _: bool| drop(owned_scenario(game).unwrap_or_else(|e| panic!("{what}: the next run failed: {e}")));
        let n = crate::fixtures::exhaust(mode, setup, |game, ()| owned_scenario(game), check);
        assert!(n > 30, "only {n} fault points in {mode:?}");
    }
}

/// A committed `Delete` of `dir/name`, as a journal that somebody planted would say it, with its backup next to it.
fn plant_delete(game: &Game, id: &str, dir: &[&str], name: &str) -> PathBuf {
    let roots = game.roots().unwrap();
    let logical: Vec<String> = dir.iter().map(|d| d.to_string()).collect();
    let at = dir.iter().fold(game.root.clone(), |p, d| p.join(d));
    std::fs::create_dir_all(&at).unwrap();
    if !at.join(name).exists() {
        std::fs::write(at.join(name), b"data").unwrap();
    }
    let backup = at.join(format!(".paleto-{id}-0.old"));
    std::fs::hard_link(at.join(name), &backup).unwrap();
    let expect = roots.open_anchor(RootKind::Game, &logical).unwrap().stat(name).unwrap();
    let delete = journal::JOp { kind: OpKind::Delete, anchor: RootKind::Game, dir: logical, name: name.into(), expect, mode: JMode::Move, made: None, kept: false, did: false };
    let text = [journal::header(), journal::begin(id, 1, 1, "planted"), journal::root(&journal::RootRec { kind: RootKind::Game, fp: roots.game.dir.fingerprint().unwrap(), shown: roots.game.shown.display().to_string() }), journal::op(0, &delete), journal::commit()].concat();
    std::fs::create_dir_all(game.root.join(".paleto/journal")).unwrap();
    std::fs::write(game.root.join(".paleto/journal").join(format!("{id}.log")), text).unwrap();
    backup
}

#[test]
fn recovery_accepts_the_two_owned_folders_and_refuses_every_other_name_of_the_tool_and_the_originals() {
    let id = "0123456789abcdef";
    // The tool's own folders for parts and backups: the journal's delete of a part is undone, from its backup.
    for top in ["mods_disabled", "mods_legacy"] {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        let backup = plant_delete(&game, id, &[top, "sub"], "f.bin");
        let target = game.root.join(top).join("sub/f.bin");
        std::fs::remove_file(&target).unwrap();
        let report = recover(&game).unwrap();
        assert!(report.failures.is_empty() && report.rolled_back == 1, "{top}: {report:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"data", "{top}: put back from its backup");
        assert!(!backup.exists(), "{top}");
        assert_clean(&game, top);
    }
    // Anything else of the tool's, the game's own data and a file named like an owned folder: reported, nothing touched.
    let cases: [(&[&str], &str); 5] = [(&[".paleto"], "f.bin"), (&["mods_snapshots", "s1"], "f.bin"), (&[".mods.x"], "f.bin"), (&["x64"], "a.rpf"), (&[], "mods_disabled")];
    for (folders, name) in cases {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        let backup = plant_delete(&game, id, folders, name);
        let target = folders.iter().fold(game.root.clone(), |p, f| p.join(f)).join(name);
        let content = std::fs::read(&target).unwrap();
        let report = recover(&game).unwrap();
        let what = format!("{folders:?} {name}");
        assert!(report.failures.len() == 1 && report.failures[0].contains("not touched"), "{what}: {report:?}");
        assert!(report.rolled_back == 0, "{what}: {report:?}");
        assert_eq!(std::fs::read(&target).unwrap(), content, "{what}: the target");
        assert!(backup.exists(), "{what}: the backup was removed on the word of a planted journal");
        assert_eq!(journals(&game).len(), 1, "{what}: the journal stays for a person to look at");
        // It blocks other changes until it is abandoned, as before.
        assert!(matches!(Txn::begin(&game.roots().unwrap(), "x"), Err(Error::RecoveryNeeded(_))), "{what}");
    }
}

/// Makes a file in two new folders; `plant` also puts a file of somebody else's in the new folder, before the commit.
fn new_folders(game: &Game, plant: bool) -> Result<Vec<String>, Error> {
    let roots = game.roots()?;
    let mut t = Txn::begin(&roots, "folders")?;
    t.stage(&mut roots.dest(&rel("plugins/deep/x.ini"))?, |f| f.write_all(b"x"))?;
    if plant {
        let _ = std::fs::write(game.root.join("plugins/deep/planted.txt"), b"theirs");
    }
    t.commit(Vec::new())
}

#[test]
fn the_fault_loop_accepts_a_folder_a_crash_left_empty_and_still_reports_a_file_in_it() {
    // A crash between the record that a folder is made and the one that it was leaves it, empty: recovery cannot prove it made it.
    let mut leaked = false;
    for k in 1..40 {
        let dir = tempfile::tempdir().unwrap();
        let game = fixture(dir.path(), true);
        faults::arm_at(Point::Journal, k, Fault::Crash);
        let _ = new_folders(&game, false);
        if !faults::disarm() {
            break;
        }
        assert!(recover(&game).unwrap().failures.is_empty());
        leaked |= game.root.join("plugins").is_dir() && !game.root.join("plugins/deep/x.ini").exists();
    }
    assert!(leaked, "no crash point left an empty folder");
    let n = crate::fixtures::exhaust(Fault::Crash, |dir| (fixture(dir, true), ()), |game, ()| new_folders(game, false), |_, (), _, _| {});
    assert!(n > 10, "only {n} fault points");
    // Not a tool-owned name, not a folder with something in it, not one that was there before.
    let entry = |s: &str| (s.to_string(), Entry::Dir);
    let old: BTreeMap<String, Entry> = [entry("a")].into();
    let now: BTreeMap<String, Entry> = [entry("a"), entry("b"), entry("b/c"), entry("d"), ("d/f".into(), Entry::File(b"x".to_vec())), entry("mods_legacy"), entry(".mods.x")].into();
    let kept: Vec<String> = crate::fixtures::without_leaked_dirs(now, &old, &old).into_keys().collect();
    assert_eq!(kept, [".mods.x", "a", "d", "d/f", "mods_legacy"]);
}

#[test]
#[should_panic(expected = "extra plugins")]
fn the_fault_loop_reports_a_file_planted_in_a_new_folder() {
    crate::fixtures::exhaust(Fault::Crash, |dir| (fixture(dir, true), ()), |game, ()| new_folders(game, true), |_, (), _, _| {});
}
