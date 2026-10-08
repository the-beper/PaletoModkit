//! The "Snapshots of the mods folder" window.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::App;

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
fn save_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    std::fs::write(root.join("mods/x64e.rpf"), b"working").unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_snapshots(&ctx);
    harness.run_steps(2);
    harness.get_by_label("No snapshots yet.");
    let field = harness.get_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.rect().left() > 200.0 && n.rect().top() > 100.0).unwrap();
    field.click();
    harness.run_steps(2);
    let field = harness.get_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.rect().left() > 200.0 && n.rect().top() > 100.0).unwrap();
    field.type_text("works");
    harness.run_steps(2);
    harness.get_by_label("Save a snapshot").click();
    settle(&mut harness);
    assert_eq!(harness.state().snapshot_labels().unwrap(), ["works"]);

    // A change breaks the game: go back, keeping the broken state too.
    std::fs::write(root.join("mods/x64e.rpf"), b"broken").unwrap();
    harness.get_by_label("Restore…").click();
    harness.run_steps(2);
    harness.get_by_label("The mods folder will be replaced by this snapshot.");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("snapshots.png")).unwrap();
    }
    harness.get_by_label("Replace the mods folder").click();
    settle(&mut harness);
    assert_eq!(std::fs::read(root.join("mods/x64e.rpf")).unwrap(), b"working");
    assert_eq!(harness.state().snapshot_labels().unwrap(), ["before restoring a snapshot", "works"]);

    harness.get_all_by_label("Delete").next().unwrap().click();
    settle(&mut harness);
    assert_eq!(harness.state().snapshot_labels().unwrap(), ["works"]);
}
