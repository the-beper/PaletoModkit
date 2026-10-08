//! The "Export mods as an OIV package" window.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
fn lists_and_picks_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut x64e = ArchiveEditor::new("x64e.rpf");
    x64e.add_file("data/timecycle/w_clear.xml", b"stock".to_vec()).unwrap();
    x64e.add_file("data/handling.meta", b"stock".to_vec()).unwrap();
    x64e.save(&root.join("x64e.rpf")).unwrap();
    let mut x64e = ArchiveEditor::open(&root.join("x64e.rpf"), None).unwrap();
    x64e.add_file("data/timecycle/w_clear.xml", b"sunny".to_vec()).unwrap();
    x64e.add_file("data/handling.meta", b"tuned".to_vec()).unwrap();
    x64e.save(&root.join("mods/x64e.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_export(&ctx);
    settle(&mut harness);
    assert_eq!(harness.state().export_counts(), Some((2, 2)));
    harness.get_by_label_contains("x64e.rpf  (2 of 2)");

    let field = harness.get_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.rect().top() > 100.0).unwrap();
    field.click();
    harness.run_steps(2);
    harness.get_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.rect().top() > 100.0).unwrap().type_text("timecycle");
    harness.run_steps(2);
    harness.get_by_label("Unselect shown").click();
    harness.run_steps(2);
    assert_eq!(harness.state().export_counts(), Some((2, 1)));
    harness.get_by_label("Save 1 change as…");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("export.png")).unwrap();
    }
}
