//! Changes an earlier run left interrupted: the notice at game selection, Undo, Later, Abandon and the
//! refusal of changes in between. The journal is left by a child process that stops in the middle of a
//! change (a crash), because the engine's fault points are not reachable from here.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::Game;
use paleto_core::relpath::RelPath;
use paleto_core::txn::{self, Txn};
use paleto_modkit::App;

const CHILD: &str = "PALETO_RECOVERY_TEST_CHILD";

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..500 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(3);
}

/// Run by the tests below in a child process: stages a change and dies without finishing or undoing it.
#[test]
fn crash_child() {
    let Ok(root) = std::env::var(CHILD) else { return };
    let game = Game::open(Path::new(&root)).unwrap();
    let roots = game.roots().unwrap();
    let mut txn = Txn::begin(&roots, "Probe change").unwrap();
    let mut dest = roots.dest(&RelPath::parse("common.dat").unwrap()).unwrap();
    txn.stage(&mut dest, |f| std::io::Write::write_all(f, b"new")).unwrap();
    // No Drop: the journal and the staged file stay, as after a power cut.
    std::process::exit(0);
}

/// A fake Enhanced game with `common.dat` and a journal of a change to it that was interrupted.
fn interrupted_game(dir: &Path) -> PathBuf {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"MZ\0\0").unwrap();
    std::fs::write(root.join("common.dat"), b"old").unwrap();
    let run = std::process::Command::new(std::env::current_exe().unwrap()).args(["crash_child", "--exact", "--nocapture", "--test-threads=1"]).env(CHILD, &root).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    root
}

fn start(root: &Path) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness
}

fn leftover_temporaries(root: &Path) -> usize {
    std::fs::read_dir(root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".paleto-")).count()
}

#[test]
fn the_notice_comes_first_and_later_keeps_the_change_pending() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted_game(dir.path());
    let before = leftover_temporaries(&root);
    assert!(before > 0);
    let mut harness = start(&root);
    harness.state_mut().offer_crash(SystemTime::now() - Duration::from_secs(60), "The crash happened in Trainer.asi.");
    harness.run_steps(3);
    let game = Game::open(&root).unwrap();
    assert_eq!(txn::pending(&game).len(), 1);
    assert_eq!(harness.state().interrupted_count(), 1);
    let text = harness.state().notice_text().unwrap();
    assert!(text.contains("An earlier change to this game was interrupted") && text.contains("Probe change"), "{text}");
    assert!(text.contains(".paleto-"), "the files are named: {text}");
    harness.get_by_label("Later").click();
    harness.run_steps(3);
    // The next notice is the crash one; the change is still pending.
    assert!(harness.state().notice_text().unwrap().starts_with("The game crashed"));
    harness.get_by_label("Dismiss").click();
    harness.run_steps(3);
    assert_eq!(txn::pending(&game).len(), 1);
    // A change is refused.
    let ctx = harness.ctx.clone();
    harness.state_mut().add_dlc_packs(&["dlcpack"], &ctx);
    harness.run_steps(3);
    assert_eq!(harness.state().status_error(), Some("An interrupted change must be undone first"));
    // The refusal brings the notice back, so Undo can still be chosen.
    let text = harness.state().notice_text().unwrap();
    assert!(text.contains("An earlier change to this game was interrupted"), "{text}");
    assert_eq!(leftover_temporaries(&root), before, "nothing was touched");
    assert_eq!(std::fs::read(root.join("common.dat")).unwrap(), b"old");
}

#[test]
fn undo_settles_the_change_and_lifts_the_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted_game(dir.path());
    let mut harness = start(&root);
    harness.get_by_label("Undo").click();
    settle(&mut harness);
    let text = harness.state().notice_text().unwrap();
    assert!(text.starts_with("Interrupted change settled"), "{text}");
    harness.get_by_label("OK").click();
    harness.run_steps(3);
    assert_eq!(harness.state().notice_text(), None);
    assert_eq!(harness.state().interrupted_count(), 0);
    assert!(txn::pending(&Game::open(&root).unwrap()).is_empty());
    assert_eq!(leftover_temporaries(&root), 0);
    assert_eq!(std::fs::read(root.join("common.dat")).unwrap(), b"old");
    let ctx = harness.ctx.clone();
    harness.state_mut().add_dlc_packs(&["dlcpack"], &ctx);
    harness.run_steps(3);
    assert_ne!(harness.state().status_error(), Some("An interrupted change must be undone first"));
}

#[test]
fn abandon_asks_first_and_sets_the_journal_aside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"MZ\0\0").unwrap();
    // A journal of a newer version: nothing can read it, so Undo cannot settle it.
    let journal = root.join(".paleto/journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(journal.join("0123456789abcdef.log"), "paleto-txn 99\n").unwrap();
    let mut harness = start(&root);
    assert_eq!(harness.state().interrupted_count(), 1);
    harness.get_by_label("Abandon…").click();
    harness.run_steps(3);
    let text = harness.state().notice_text().unwrap();
    assert!(text.contains("Nothing is deleted") && text.contains("0123456789abcdef.log"), "{text}");
    // Nothing happened yet.
    assert!(journal.join("0123456789abcdef.log").exists());
    harness.get_by_label("Set aside").click();
    settle(&mut harness);
    assert!(harness.state().notice_text().unwrap().starts_with("Interrupted change set aside"));
    assert!(!journal.join("0123456789abcdef.log").exists());
    assert!(journal.join("abandoned/0123456789abcdef.log").exists());
    assert_eq!(harness.state().interrupted_count(), 0);
}

#[test]
fn the_refusal_lifts_when_the_change_is_settled_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted_game(dir.path());
    let mut harness = start(&root);
    harness.get_by_label("Later").click();
    harness.run_steps(3);
    // Settled by the command line, say.
    txn::recover(&Game::open(&root).unwrap()).unwrap();
    let ctx = harness.ctx.clone();
    harness.state_mut().add_dlc_packs(&["dlcpack"], &ctx);
    harness.run_steps(3);
    assert_ne!(harness.state().status_error(), Some("An interrupted change must be undone first"));
    assert_eq!(harness.state().interrupted_count(), 0);
}

#[test]
fn the_refusal_does_not_follow_to_another_game() {
    let dir = tempfile::tempdir().unwrap();
    let root = interrupted_game(dir.path());
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = {
        let g = other.join("Grand Theft Auto V Enhanced");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("GTA5_Enhanced.exe"), b"MZ\0\0").unwrap();
        g
    };
    let mut harness = start(&root);
    harness.get_by_label("Later").click();
    harness.run_steps(3);
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&other, &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().interrupted_count(), 0);
    assert_eq!(harness.state().notice_text(), None);
    harness.state_mut().add_dlc_packs(&["dlcpack"], &ctx);
    harness.run_steps(3);
    assert_ne!(harness.state().status_error(), Some("An interrupted change must be undone first"));
    assert_eq!(txn::pending(&Game::open(&root).unwrap()).len(), 1);
}
