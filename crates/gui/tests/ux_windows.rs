//! The favorites, export and hash windows: no button lower than the target size, and a name for the checkbox that has no visible label.
//! The permission gate of the prefix window (its status icons) opens only inside a Flatpak (`/.flatpak-info`), so it is not tested here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, Node};
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
    harness.run_steps(3);
}

/// An Enhanced game with `x64e.rpf` and a changed copy of it in the mods folder.
fn fake_game(base: &Path) -> PathBuf {
    let root = base.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut stock = ArchiveEditor::new("x64e.rpf");
    stock.add_file("data/timecycle/w_clear.xml", b"stock".to_vec()).unwrap();
    stock.add_file("data/handling.meta", b"stock".to_vec()).unwrap();
    stock.save(&root.join("x64e.rpf")).unwrap();
    let mut modded = ArchiveEditor::open(&root.join("x64e.rpf"), None).unwrap();
    modded.add_file("data/timecycle/w_clear.xml", b"sunny".to_vec()).unwrap();
    modded.add_file("data/handling.meta", b"tuned".to_vec()).unwrap();
    modded.save(&root.join("mods/x64e.rpf")).unwrap();
    root
}

fn app_with(root: &Path) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1200.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness
}

/// The controls of `role` inside the open dialog: the app behind it has its own.
fn in_dialog<'t>(harness: &'t Harness<'_, App>, role: Role) -> Vec<Node<'t>> {
    let mut found = Vec::new();
    for dialog in harness.query_all_by_role(Role::Dialog) {
        found.extend(dialog.query_all_by_role(role));
    }
    found
}

fn name(node: &Node<'_>) -> String {
    node.accesskit_node().label().unwrap_or_default()
}

fn assert_buttons_reach_the_target(harness: &Harness<'_, App>, window: &str) {
    let target = harness.ctx.global_style().spacing.interact_size.y;
    let buttons = in_dialog(harness, Role::Button);
    assert!(!buttons.is_empty(), "{window}: no button found in the dialog");
    let low: Vec<String> = buttons.iter().filter(|b| b.rect().height() < target - 0.01).map(|b| format!("{:?} {:.0}x{:.0}", name(b), b.rect().width(), b.rect().height())).collect();
    assert!(low.is_empty(), "{window}: buttons lower than {target} points: {low:?}");
}

/// The text boxes of the hash window, in their order on screen.
fn edit_boxes<'t>(harness: &'t Harness<'_, App>) -> Vec<Node<'t>> {
    let mut nodes: Vec<Node<'t>> = harness.query_all_by_role(Role::MultilineTextInput).collect();
    nodes.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()).then_with(|| a.rect().left().total_cmp(&b.rect().left())));
    nodes
}

#[test]
fn tool_windows_have_big_enough_buttons_and_named_checkboxes() {
    let dir = tempfile::tempdir().unwrap();
    // No game, keys, settings or names store of the user's: a scratch home, as the other window tests do.
    // SAFETY: the only test of this file, so no other thread reads the environment while it is changed.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::remove_var("PALETO_GAME");
    }
    let root = fake_game(dir.path());

    // Favorites: a recent location and a favorite, so that every kind of row has its button.
    let mut harness = app_with(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().open(ArchivePath::new(root.join("x64e.rpf")).join("data"));
    settle(&mut harness);
    harness.state_mut().open_favorites(&ctx);
    harness.run_steps(2);
    harness.get_by_label("Add the shown place").click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().favorite_places().len(), 1, "the shown place became a favorite");
    let forgets = in_dialog(&harness, Role::Button).iter().filter(|b| name(b).starts_with("Forget")).count();
    assert!(forgets >= 3, "a Forget button for the favorite and for the recent location, and Forget them all: {forgets}");
    assert_buttons_reach_the_target(&harness, "favorites");
    drop(harness);

    // Export: the group checkbox has no visible label, so it needs another name.
    let mut harness = app_with(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().open_export(&ctx);
    settle(&mut harness);
    assert!(harness.state().export_counts().is_some_and(|(listed, _)| listed > 0), "the window lists the changes");
    assert_buttons_reach_the_target(&harness, "export");
    let boxes = in_dialog(&harness, Role::CheckBox);
    assert!(!boxes.is_empty(), "the window has a checkbox per group");
    let unnamed = boxes.iter().filter(|b| name(b).is_empty()).count();
    assert_eq!(unnamed, 0, "every checkbox has an accessible name");
    assert!(boxes.iter().any(|b| name(b).contains("x64e.rpf")), "the group checkbox says which group it selects");
    drop(harness);

    // Hashes: typing a name makes the rows with the buttons that copy its values.
    let mut harness = app_with(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().open_hashes(&ctx);
    for _ in 0..100 {
        if edit_boxes(&harness).len() == 3 {
            break;
        }
        harness.step();
    }
    assert_eq!(edit_boxes(&harness).len(), 3, "the generate, resolve and paste boxes of the window");
    edit_boxes(&harness)[0].click();
    harness.run_steps(2);
    edit_boxes(&harness)[0].type_text("Adder_vehicle");
    harness.run_steps(3);
    let hex = paleto_core::hashes::as_hex(paleto_core::hashes::Mode::Lower.hash("Adder_vehicle"));
    assert!(in_dialog(&harness, Role::Button).iter().any(|b| name(b) == hex), "the value rows are shown");
    assert_buttons_reach_the_target(&harness, "hashes");
}
