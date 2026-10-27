//! Files that differ from the original game files are marked in a mods copy.

use std::time::Duration;

use egui_kittest::Harness;
use paleto_core::ArchivePath;
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
    harness.run_steps(2);
}

#[test]
fn replaced_and_added_files_are_marked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut common = ArchiveEditor::new("common.rpf");
    common.add_file("data/ai/vehicleaihandlinginfo.meta", b"<original/>".to_vec()).unwrap();
    common.add_file("data/ai/other.meta", b"<other/>".to_vec()).unwrap();
    common.save(&root.join("common.rpf")).unwrap();
    let mine = dir.path().join("mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("vehicleaihandlinginfo.meta"), b"<mine/>").unwrap();
    std::fs::write(mine.join("extra.meta"), b"<extra/>").unwrap();
    // SAFETY: this test binary runs this single test. No cached keys, as in
    // CI: the fake game's keys cannot be found.
    unsafe { std::env::set_var("XDG_DATA_HOME", dir.path().join("data")) };

    let mut harness = Harness::builder().build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    let ai = ArchivePath::new(root.join("common.rpf")).join("data").join("ai");
    harness.state_mut().open(ai.clone());
    settle(&mut harness);
    assert!(harness.state().changed_names().is_empty());

    // Replacing a file: the mods copy is made, and the file shows as changed.
    harness.state_mut().add_files(vec![mine.join("vehicleaihandlinginfo.meta")], &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().changed_names(), [("vehicleaihandlinginfo.meta".to_string(), false)]);

    // A new file shows as added.
    harness.state_mut().add_files(vec![mine.join("extra.meta")], &ctx);
    settle(&mut harness);
    let mut changed = harness.state().changed_names();
    changed.sort();
    assert_eq!(changed, [("extra.meta".to_string(), true), ("vehicleaihandlinginfo.meta".to_string(), false)]);

    // Putting the original content back: no longer marked.
    std::fs::write(mine.join("vehicleaihandlinginfo.meta"), b"<original/>").unwrap();
    harness.state_mut().add_files(vec![mine.join("vehicleaihandlinginfo.meta")], &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().changed_names(), [("extra.meta".to_string(), true)]);

    // Compared without the keys, the result is not kept:
    // coming back compares again.
    let mut common = ArchiveEditor::open(&root.join("common.rpf"), None).unwrap();
    common.add_file("data/ai/extra.meta", b"<extra/>".to_vec()).unwrap();
    common.save(&root.join("common.rpf")).unwrap();
    harness.state_mut().open(ai.clone());
    settle(&mut harness);
    assert!(harness.state().changed_names().is_empty());
}
