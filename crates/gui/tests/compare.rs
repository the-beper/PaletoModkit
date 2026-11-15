//! "Compare with original": a file of the mods folder against the game's own, read-only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::ArchivePath;
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
