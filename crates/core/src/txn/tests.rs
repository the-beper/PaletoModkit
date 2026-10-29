use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use super::journal::{self, Mode as JMode, OpKind, ParseError};
use super::*;
use crate::fixtures::{Entry, ReadOnly, is_root, snapshot};
use crate::fsroot::Caps;
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
            assert_eq!(state(&game), want, "{what}: committed={committed}");
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
        assert_eq!(state(&game), want, "k={k}");
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
    let clean = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: true, failures: Vec::new(), journaled: true };
    assert_eq!(clean.to_string(), "package: boom (nothing was installed)");
    let left = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: false, failures: vec!["/g/a.asi: gone (backup: /g/.paleto-1-0.old)".into()], journaled: true };
    let text = left.to_string();
    assert!(!text.contains("nothing was installed") && text.contains("/g/.paleto-1-0.old") && text.contains("journal"), "{text}");
    let odd = TxnFailure { cause: Error::Package("boom".into()), log: Vec::new(), undone: true, failures: vec!["x".into()], journaled: true };
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
