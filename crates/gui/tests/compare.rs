//! "Compare with original": a file of the mods folder against the game's own, read-only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::ArchivePath;
use paleto_modkit::app::Info;
use paleto_modkit::{App, Location};
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

fn archive(path: &Path, files: &[(&str, &[u8])]) {
    let mut editor = ArchiveEditor::new("update.rpf");
    for (name, data) in files {
        editor.add_file(name, data.to_vec()).unwrap();
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    editor.save(path).unwrap();
}

fn blob() -> Vec<u8> {
    (0..64).collect()
}

fn changed_blob() -> Vec<u8> {
    let mut data = blob();
    data[3] = 0xFF;
    data[4] = 0xFE;
    data[40] = 0xEE;
    data
}

/// A game whose `update/update.rpf` has a text file, a binary file and (in the mods copy only) a new file, and a `common.rpf` that has no mods copy.
fn fake_game() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    archive(&root.join("update/update.rpf"), &[("common/data/a.txt", b"one\ntwo\n"), ("common/data/blob.bin", &blob())]);
    archive(&root.join("mods/update/update.rpf"), &[("common/data/a.txt", b"one\n2\n"), ("common/data/blob.bin", &changed_blob()), ("common/data/new.txt", b"fresh\n")]);
    archive(&root.join("common.rpf"), &[("data/b.txt", b"stock\n")]);
    (dir, root)
}

/// The app with the mods copy's `common/data` folder open and `file` previewed.
fn preview(root: &Path, file: &str) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1800.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data"));
    settle(&mut harness);
    harness.state_mut().preview(file, &ctx);
    settle(&mut harness);
    harness
}

/// The preview's button: its label, or only its icon when the panel is narrow.
fn click_compare(harness: &mut Harness<'_, App>) {
    let button = harness.query_by_label("⬌ Compare with original").or_else(|| harness.query_by_label("⬌")).expect("the Compare with original button");
    button.click();
    harness.run_steps(2);
    settle(harness);
}

fn has_compare_button(harness: &Harness<'_, App>) -> bool {
    harness.query_by_label("⬌ Compare with original").is_some() || harness.query_by_label("⬌").is_some()
}

fn has_label(harness: &Harness<'_, App>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

fn window_rect(harness: &Harness<'_, App>) -> egui::Rect {
    harness.get_by_role_and_label(Role::Window, "Compare with original").rect()
}

/// The window's own close button: other controls of the app can have the same label.
fn close_rect(harness: &Harness<'_, App>) -> egui::Rect {
    let window = window_rect(harness);
    harness.query_all_by_label("✖").map(|n| n.rect()).find(|r| window.contains(r.center())).expect("the window's close button")
}

#[test]
fn a_changed_text_file_shows_its_removed_and_added_lines() {
    let (_dir, root) = fake_game();
    let (original, copy) = (root.join("update/update.rpf"), root.join("mods/update/update.rpf"));
    let (before, before_copy) = (std::fs::read(&original).unwrap(), std::fs::read(&copy).unwrap());
    let mut harness = preview(&root, "a.txt");
    assert_eq!(harness.state().compare_summary(), None);
    click_compare(&mut harness);

    let summary = harness.state().compare_summary().expect("the window is open");
    for expected in ["a.txt", "Mods copy: mods/update/update.rpf/common/data/a.txt", "Original: update/update.rpf/common/data/a.txt", "Kind: text", "1 removed, 1 added (unified)", "- two", "+ 2"] {
        assert!(summary.contains(expected), "{expected:?} not in {summary}");
    }
    harness.get_by_label("Side by side").click();
    harness.run_steps(2);
    assert!(harness.state().compare_summary().unwrap().contains("(side by side)"));

    // The hook does what the button does, and the window shows the result again, in the unified view.
    let ctx = harness.ctx.clone();
    harness.state_mut().compare_with_original(&ctx);
    settle(&mut harness);
    assert!(harness.state().compare_summary().unwrap().contains("(unified)"));

    assert_eq!(std::fs::read(&original).unwrap(), before);
    assert_eq!(std::fs::read(&copy).unwrap(), before_copy);
}

#[test]
fn a_file_only_the_mods_folder_has_is_added() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "new.txt");
    click_compare(&mut harness);
    let summary = harness.state().compare_summary().expect("the window is open");
    assert!(summary.contains("The game has no original of this file: the mods folder adds it"), "{summary}");
    assert!(summary.contains("Original: none"), "{summary}");
}

#[test]
fn a_changed_binary_file_shows_sizes_stretches_and_both_buffers() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "blob.bin");
    click_compare(&mut harness);
    let summary = harness.state().compare_summary().expect("the window is open");
    for expected in ["Kind: binary", "Original: 64 bytes, SHA-256 ", "Mods copy: 64 bytes, SHA-256 ", "Differs at 0x00000003: 2 bytes", "Differs at 0x00000028: 1 byte", "Hex view from 0x00000000"] {
        assert!(summary.contains(expected), "{expected:?} not in {summary}");
    }
    harness.get_by_label("0x00000003  2 bytes");
    // The original's bytes only show in the window: the preview has the mods copy's.
    harness.get_by_label_contains("00 01 02 03 04 05 06 07");

    harness.get_by_label("⏷ Next").click();
    harness.run_steps(2);
    assert!(harness.state().compare_summary().unwrap().contains("Hex view from 0x00000020"));
    assert!(harness.query_by_label_contains("00 01 02 03 04 05 06 07").is_none());
    harness.get_by_label("⏶ Previous").click();
    harness.run_steps(2);
    assert!(harness.state().compare_summary().unwrap().contains("Hex view from 0x00000000"));
    harness.get_by_label("0x00000028  1 byte").click();
    harness.run_steps(2);
    assert!(harness.state().compare_summary().unwrap().contains("Hex view from 0x00000020"));
}

#[test]
fn a_loose_file_of_the_mods_folder_is_compared_with_the_games() {
    let (_dir, root) = fake_game();
    std::fs::write(root.join("loose.txt"), b"a\nb\n").unwrap();
    std::fs::write(root.join("mods/loose.txt"), b"a\nc\n").unwrap();
    let mut harness = Harness::builder().with_size([1800.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Folder(root.join("mods")));
    settle(&mut harness);
    harness.state_mut().preview("loose.txt", &ctx);
    settle(&mut harness);
    click_compare(&mut harness);
    let summary = harness.state().compare_summary().expect("the window is open");
    for expected in ["Mods copy: mods/loose.txt", "Original: loose.txt", "- b", "+ c"] {
        assert!(summary.contains(expected), "{expected:?} not in {summary}");
    }
}

#[test]
fn a_file_of_the_original_archive_has_no_button() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    assert!(has_compare_button(&harness));
    let ctx = harness.ctx.clone();
    harness.state_mut().open(ArchivePath::new(root.join("common.rpf")).join("data"));
    settle(&mut harness);
    harness.state_mut().preview("b.txt", &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().preview_text(), Some("stock\n"));
    assert!(!has_compare_button(&harness));
}

#[test]
fn the_side_by_side_view_marks_changed_lines_with_a_sign() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    click_compare(&mut harness);
    harness.get_by_label("Side by side").click();
    harness.run_steps(3);
    assert!(harness.state().compare_summary().unwrap().contains("(side by side)"));
    // The removed line on the left and the added one on the right, each with its sign.
    harness.get_by_label("- two | + 2");
}

#[test]
fn the_close_button_is_a_full_size_control_and_closes_the_window() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    click_compare(&mut harness);
    let side = harness.ctx.global_style().spacing.interact_size.y;
    let close = close_rect(&harness);
    assert!(close.height() >= side - 0.01 && close.width() >= side - 0.01, "the close button is {:?}, controls are {side} high", close.size());

    let window = window_rect(&harness);
    harness.query_all_by_label("✖").find(|n| window.contains(n.rect().center())).expect("the window's close button").click();
    harness.run_steps(3);
    assert_eq!(harness.state().compare_summary(), None);
}

#[test]
fn escape_closes_the_window_but_a_dialog_has_it_first() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    click_compare(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().show_info(Some(Info::Shortcuts), &ctx);
    harness.run_steps(3);
    assert!(has_label(&harness, "Keyboard and mouse shortcuts"));

    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(!has_label(&harness, "Keyboard and mouse shortcuts"), "Esc closes the dialog");
    assert!(harness.state().compare_summary().is_some(), "and leaves the window");

    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert_eq!(harness.state().compare_summary(), None);
}

/// egui takes the focus off a text field on Esc before the frame runs, so the window must go by the focus of the frame before.
#[test]
fn escape_in_the_browser_filter_leaves_the_window_open() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    click_compare(&mut harness);
    let filter = harness.get_by_role(egui::accesskit::Role::TextInput);
    filter.click();
    harness.run_steps(3);
    harness.get_by_role(egui::accesskit::Role::TextInput).type_text("zz");
    harness.run_steps(3);

    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().compare_summary().is_some(), "Esc cleared the filter and closed the window as well");

    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert_eq!(harness.state().compare_summary(), None, "with no text field focused Esc closes it");
}

#[test]
fn the_window_opens_in_the_middle_and_moves_by_its_header() {
    let (_dir, root) = fake_game();
    let mut harness = preview(&root, "a.txt");
    click_compare(&mut harness);
    let before = window_rect(&harness);
    let middle = harness.ctx.content_rect().center();
    assert!((before.center() - middle).length() < 10.0, "the window is at {before:?}, the middle is {middle:?}");

    // Between the title and the close button: nothing there takes the drag.
    let grab = egui::pos2(before.center().x, close_rect(&harness).center().y);
    harness.hover_at(grab);
    harness.run_steps(2);
    harness.drag_at(grab);
    harness.run_steps(2);
    for step in 1..=5 {
        harness.hover_at(grab + egui::vec2(20.0, 10.0) * step as f32);
        harness.run_steps(2);
    }
    harness.drop_at(grab + egui::vec2(100.0, 50.0));
    harness.run_steps(3);
    let after = window_rect(&harness);
    assert!(after.min.x > before.min.x + 80.0 && after.min.y > before.min.y + 30.0, "the window was at {before:?} and is at {after:?}");
}
