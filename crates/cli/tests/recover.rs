//! `paleto recover`, and the recovery that every command changing a game does first. The journal is left
//! by a child process that stops in the middle of a change, as a crash does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use paleto_core::Game;
use paleto_core::relpath::RelPath;
use paleto_core::txn::{self, Txn};

const CHILD: &str = "PALETO_RECOVER_TEST_CHILD";

/// Run by the tests below in a child process: stages a change and dies without finishing or undoing it.
#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CHILD) else { return };
    let game = Game::open(Path::new(&root)).unwrap();
    let roots = game.roots().unwrap();
    let mut txn = Txn::begin(&roots, "Probe change").unwrap();
    let mut dest = roots.dest(&RelPath::parse("common.dat").unwrap()).unwrap();
    txn.stage(&mut dest, |f| std::io::Write::write_all(f, b"new")).unwrap();
    std::process::exit(0);
}

fn game(dir: &Path) -> PathBuf {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"MZ\0\0").unwrap();
    std::fs::write(root.join("common.dat"), b"old").unwrap();
    root
}

fn interrupted(dir: &Path) -> PathBuf {
    let root = game(dir);
    let run = Command::new(std::env::current_exe().unwrap()).args(["crash_child", "--exact", "--nocapture", "--test-threads=1"]).env(CHILD, &root).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(txn::pending(&Game::open(&root).unwrap()).len(), 1);
    root
}

fn paleto(dir: &Path, root: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or settings of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").env_remove("PALETO_GAME").current_dir(dir);
    cmd.arg("--game").arg(root).args(args).output().unwrap()
}

fn out(run: &Output) -> String {
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn status_lists_the_interrupted_change_and_the_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted(dir.path());
    let run = paleto(dir.path(), &root, &["recover", "--status"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = out(&run);
    assert!(text.contains("interrupted") && text.contains("Probe change") && text.contains(".paleto-"), "{text}");
    // Only a listing: still pending.
    assert_eq!(txn::pending(&Game::open(&root).unwrap()).len(), 1);
}

#[test]
fn status_with_nothing_pending() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    let run = paleto(dir.path(), &root, &["recover", "--status"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(out(&run).contains("no interrupted change"), "{}", out(&run));
}

#[test]
fn recover_prints_the_report_and_clears_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted(dir.path());
    let run = paleto(dir.path(), &root, &["recover"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = out(&run);
    assert!(text.lines().any(|l| l.starts_with("recovered: ")), "{text}");
    assert!(txn::pending(&Game::open(&root).unwrap()).is_empty());
    assert_eq!(std::fs::read(root.join("common.dat")).unwrap(), b"old");
    let again = paleto(dir.path(), &root, &["recover"]);
    assert!(out(&again).contains("no interrupted change"), "{}", out(&again));
}

#[test]
fn a_changing_command_recovers_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted(dir.path());
    let run = paleto(dir.path(), &root, &["clean", "--delete"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = out(&run);
    assert!(text.lines().any(|l| l.starts_with("recovered: ")), "{text}");
    assert!(txn::pending(&Game::open(&root).unwrap()).is_empty());
}

#[test]
fn a_reading_command_leaves_the_change_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted(dir.path());
    let run = paleto(dir.path(), &root, &["clean"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(!out(&run).contains("recovered:"));
    assert_eq!(txn::pending(&Game::open(&root).unwrap()).len(), 1);
}

#[test]
fn abandon_sets_a_journal_aside_and_keeps_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted(dir.path());
    let id = txn::pending(&Game::open(&root).unwrap())[0].journal.trim_end_matches(".log").to_string();
    let run = paleto(dir.path(), &root, &["recover", "--abandon", &id]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let text = out(&run);
    assert!(text.contains("nothing it names was touched or deleted") && text.contains("left as it is"), "{text}");
    assert!(txn::pending(&Game::open(&root).unwrap()).is_empty());
    assert!(std::fs::read_dir(&root).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".paleto-")));
}

#[test]
fn a_recovery_that_fails_stops_the_command_with_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    // A journal of a newer version: it cannot be read, so it cannot be undone.
    let journal = root.join(".paleto/journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(journal.join("0123456789abcdef.log"), "paleto-txn 99\n").unwrap();
    for args in [&["clean", "--delete"][..], &["recover"][..]] {
        let run = paleto(dir.path(), &root, args);
        assert_eq!(run.status.code(), Some(1), "{args:?}: {}", out(&run));
        let err = String::from_utf8_lossy(&run.stderr);
        assert!(err.contains("0123456789abcdef") && err.contains("could not be undone"), "{args:?}: {err}");
        assert!(!out(&run).contains("no interrupted change"));
    }
    // --status still lists it, and Abandon clears the way.
    assert!(out(&paleto(dir.path(), &root, &["recover", "--status"])).contains("unreadable"));
    let run = paleto(dir.path(), &root, &["recover", "--abandon", "0123456789abcdef"]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(paleto(dir.path(), &root, &["clean", "--delete"]).status.success());
}
