//! The layout at the smallest window, the sort headers from the keyboard, the side tree's rows as click targets, and names that line up after any icon.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use paleto_core::ArchivePath;
use paleto_modkit::settings::{SortBy, TextSize};
use paleto_modkit::{App, Location, Settings};
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

/// A game with two folders and an archive whose `common/data` folder holds one file for each of four icons, and a subfolder.
fn fake_game() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("x64")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut editor = ArchiveEditor::new("update.rpf");
    for name in ["a.txt", "b.png", "c.asi", "d.bin", "sub/e.txt"] {
        editor.add_file(&format!("common/data/{name}"), b"data\n".to_vec()).unwrap();
    }
    editor.save(&root.join("update/update.rpf")).unwrap();
    (dir, root)
}

/// Opens the game and its archive's `common/data` folder.
fn open_data(harness: &mut Harness<'_, App>, root: &Path) {
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("common").join("data"));
    settle(harness);
}

#[test]
fn the_file_list_keeps_its_room_at_the_smallest_window() {
    let (_dir, root) = fake_game();
    // A window of W pixels has W / zoom points: the minimum is 720 x 420 points at every text size.
    for (label, text_size, zoom) in [("normal text", TextSize::Normal, 1.0), ("large text", TextSize::Large, 1.15)] {
        let mut harness = Harness::builder().with_size([720.0 * zoom, 420.0 * zoom]).build_eframe(|cc| App::new(cc));
        let ctx = harness.ctx.clone();
        harness.state_mut().set_settings(Settings { text_size, ..Settings::default() }, &ctx);
        harness.step();
        if zoom != 1.0 {
            // The minimum window is sent again once the scale has changed: winit turns it into pixels with the scale it has then.
            let sent = harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::MinInnerSize(s) if *s == egui::vec2(720.0, 420.0))));
            assert!(sent, "{label}: the minimum window size was not sent after the text size changed");
        }
        open_data(&mut harness, &root);
        harness.state_mut().preview("a.txt", &ctx);
        settle(&mut harness);

        let window = harness.ctx.content_rect();
        assert!((window.width() - 720.0).abs() < 1.0 && (window.height() - 420.0).abs() < 1.0, "{label}: the window is {window:?} points, not 720 x 420");
        assert_eq!(harness.state().previewing(), Some("a.txt"), "{label}: the preview is not open");
        harness.get_by_label("⏷ Expand all");

        let name = harness.get_by_label("Sort by name").rect();
        let size = harness.get_by_label("Sort by size").rect();
        for (what, rect) in [("Name", name), ("Size", size)] {
            assert!(rect.left() >= 0.0 && rect.right() <= window.right() && rect.bottom() <= window.bottom(), "{label}: the {what} header {rect:?} is not inside the {window:?} window");
        }
        // The Size column starts a good way right of the Name column: all three columns fit.
        assert!(size.left() - name.left() >= 180.0, "{label}: the file list is squeezed, Name at {name:?} and Size at {size:?}");
    }
}

#[test]
fn sort_headers_have_a_name_and_work_from_the_keyboard() {
    let (_dir, root) = fake_game();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    open_data(&mut harness, &root);
    for name in ["Sort by name", "Sort by type", "Sort by size"] {
        assert_eq!(harness.get_by_label(name).accesskit_node().role(), egui::accesskit::Role::Button, "{name}");
    }
    assert!(harness.state().settings().sort_by == SortBy::Name);

    harness.get_by_label("Sort by size").focus();
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().settings().sort_by == SortBy::Size && !harness.state().settings().descending);

    // Space on the same header reverses the order.
    harness.key_press(egui::Key::Space);
    harness.run_steps(3);
    assert!(harness.state().settings().sort_by == SortBy::Size && harness.state().settings().descending);
}

#[test]
fn a_tree_row_is_a_click_target_across_the_panel() {
    let (_dir, root) = fake_game();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Folder(root.clone())));

    // "update" is in the tree and in the file list; the tree is at the left.
    let row = harness.get_all_by_label("update").map(|n| n.rect()).min_by(|a, b| a.left().total_cmp(&b.left())).unwrap();
    assert!(row.width() > 200.0, "the row is only as wide as its name: {row:?}");
    let at = egui::pos2(row.right() - 4.0, row.center().y);
    harness.hover_at(at);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Folder(root.join("update"))), "a click at {at:?} did not open the folder");
}

#[test]
fn names_start_at_the_same_x_after_any_icon() {
    let (_dir, root) = fake_game();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    open_data(&mut harness, &root);
    // A folder, a text file, a texture, a program and a file of no known kind.
    let names = ["sub", "a.txt", "b.png", "c.asi", "d.bin"];
    let lefts: Vec<f32> = names.iter().map(|name| harness.get_by_label(name).rect().left()).collect();
    for (name, left) in names.iter().zip(&lefts) {
        assert!((left - lefts[0]).abs() < 0.5, "{name} starts at {left}, {} at {}", names[0], lefts[0]);
    }
}

#[test]
fn an_idle_frame_sends_no_window_command_and_asks_for_no_repaint() {
    let (_dir, root) = fake_game();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    open_data(&mut harness, &root);
    harness.run_steps(5);
    for _ in 0..3 {
        harness.step();
        let output = harness.output().viewport_output.get(&egui::ViewportId::ROOT).expect("the root viewport");
        assert!(output.commands.iter().all(|c| !matches!(c, egui::ViewportCommand::MinInnerSize(_))), "a window command at rest: {:?}", output.commands);
        assert_eq!(output.repaint_delay, std::time::Duration::MAX, "a frame at rest asks for another");
    }
}

#[test]
fn the_arrow_of_a_tree_folder_opens_it_and_goes_nowhere() {
    let (_dir, root) = fake_game();
    std::fs::create_dir_all(root.join("update/inner")).unwrap();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    assert!(!harness.state().open_folders().contains(&root.join("update")));

    harness.get_by_label("Expand update").click();
    settle(&mut harness);
    assert!(harness.state().open_folders().contains(&root.join("update")), "the arrow did not open the folder");
    assert_eq!(harness.state().location(), Some(&Location::Folder(root.clone())), "the arrow also opened it in the list");
    harness.get_by_label("Collapse update");
}

/// Only archives and files are scrolled to (`reveal` in menus.rs): a folder row that becomes current stays where it is.
#[test]
fn the_tree_scrolls_to_the_archive_that_becomes_current() {
    let (_dir, root) = fake_game();
    for i in 0..60 {
        let mut editor = ArchiveEditor::new("a.rpf");
        editor.add_file("x.txt", b"x".to_vec()).unwrap();
        editor.save(&root.join(format!("a{i:02}.rpf"))).unwrap();
    }
    let mut harness = Harness::builder().with_size([1000.0, 500.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Archive(ArchivePath::new(root.join("a59.rpf"))));
    settle(&mut harness);
    harness.run_steps(3);
    let row = harness.get_all_by_label("a59.rpf").map(|n| n.rect()).min_by(|a, b| a.left().total_cmp(&b.left())).expect("an a59.rpf row in the tree");
    assert!(row.top() >= 0.0 && row.bottom() <= 480.0, "the current archive is out of view: {row:?}");
}
