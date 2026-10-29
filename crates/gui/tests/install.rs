//! What the install dialog shows of a failed install (what it did and undid, and what is left), and a batch of add-on packs.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use paleto_core::Error;
use paleto_core::txn::TxnFailure;
use paleto_modkit::App;
use paleto_rpf::ArchiveEditor;

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

fn start() -> Harness<'static, App> {
    Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc))
}

fn failure(undone: bool, failures: &[&str], journaled: bool) -> Error {
    let log = vec!["add mods/a.rpf".to_string(), "undone: mods/a.rpf".to_string()];
    Error::Txn(Box::new(TxnFailure { cause: Error::Package("second command failed".into()), log, undone, failures: failures.iter().map(|f| f.to_string()).collect(), journaled, verb: "installed" }))
}

fn labels(harness: &Harness<'_, App>) -> Vec<String> {
    harness.query_all_by_label_contains("").filter_map(|n| n.accesskit_node().label()).collect()
}

#[test]
fn a_clean_undo_shows_the_error_and_the_log() {
    let mut harness = start();
    harness.state_mut().offer_install_failure(&failure(true, &[], true));
    harness.run_steps(3);
    harness.get_by_label_contains("second command failed (nothing was installed)");
    harness.get_by_label("add mods/a.rpf");
    harness.get_by_label("undone: mods/a.rpf");
    assert!(!labels(&harness).iter().any(|l| l.contains("Left behind")));
}

#[test]
fn an_unclean_undo_names_what_is_left_and_never_says_nothing_was_installed() {
    let mut harness = start();
    harness.state_mut().offer_install_failure(&failure(false, &["/g/a.asi: gone (backup: /g/.paleto-1-0.old)"], true));
    harness.run_steps(3);
    harness.get_by_label_contains("second command failed: the changes could not be fully undone");
    harness.get_by_label("Left behind (not undone):");
    harness.get_by_label("  /g/a.asi: gone (backup: /g/.paleto-1-0.old)");
    harness.get_by_label_contains("run `paleto recover`");
    harness.get_by_label("add mods/a.rpf");
    assert!(!labels(&harness).iter().any(|l| l.contains("nothing was installed")), "{:?}", labels(&harness));

    // Without a journal the files stay as they are.
    let mut harness = start();
    harness.state_mut().offer_install_failure(&failure(false, &["/g/a.asi: gone"], false));
    harness.run_steps(3);
    harness.get_by_label_contains("left as they are, with their backups next to them");
    harness.get_by_label("Close").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Left behind (not undone):").is_none());
}

#[test]
fn two_add_on_packs_install_in_one_go() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut update = ArchiveEditor::new("update.rpf");
    update.add_file("common/data/dlclist.xml", b"<SMandatoryPacksData>\n\t<Paths>\n\t\t<Item>dlcpacks:/mpheist/</Item>\n\t</Paths>\n</SMandatoryPacksData>\n".to_vec()).unwrap();
    std::fs::create_dir_all(root.join("update")).unwrap();
    update.save(&root.join("update/update.rpf")).unwrap();
    let download = dir.path().join("Two Cars");
    for name in ["supercar", "minivan"] {
        std::fs::create_dir_all(download.join("dlcpacks").join(name)).unwrap();
        ArchiveEditor::new("dlc.rpf").save(&download.join("dlcpacks").join(name).join("dlc.rpf")).unwrap();
    }

    let mut harness = start();
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(download)));
    harness.run_steps(3);
    harness.get_by_label_contains("add-on pack");
    harness.get_by_label("Install").click();
    settle(&mut harness);

    assert_eq!(harness.state().status_error(), None);
    for name in ["supercar", "minivan"] {
        assert!(root.join("mods/update/x64/dlcpacks").join(name).join("dlc.rpf").is_file(), "{name}");
    }
    let modded = paleto_core::ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data").join("dlclist.xml");
    let modded = String::from_utf8(modded.read(None).unwrap()).unwrap();
    assert!(modded.contains("dlcpacks:/supercar/") && modded.contains("dlcpacks:/minivan/"), "{modded}");
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}
