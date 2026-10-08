//! Keyboard, mouse and side-tree navigation on a fake game folder.

use std::path::Path;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::settings::Grouping;
use paleto_modkit::{App, Location};
use paleto_rpf::ArchiveEditor;

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..300 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(3);
}

fn fake_game(root: &Path) {
    for dir in ["update", "x64", "mods/inner_dir", "zz_folder"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("common/data/handling.meta", b"<x/>".to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    ArchiveEditor::new("common.rpf").save(&root.join("common.rpf")).unwrap();
}

fn folder(harness: &Harness<'_, App>) -> std::path::PathBuf {
    match harness.state().location() {
        Some(Location::Folder(p)) => p.clone(),
        other => panic!("not a folder: {other:?}"),
    }
}

#[test]
fn navigation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    assert_eq!(folder(&harness), root);

    // Grouping: folders first (default), by type, mixed.
    assert_eq!(harness.state().names(), ["mods", "update", "x64", "zz_folder", "common.rpf", "GTA5.exe"]);
    let mut s = harness.state().settings().clone();
    s.grouping = Grouping::ByType;
    harness.state_mut().set_settings(s.clone(), &ctx);
    assert_eq!(harness.state().names(), ["mods", "update", "x64", "zz_folder", "GTA5.exe", "common.rpf"]);
    s.grouping = Grouping::None;
    harness.state_mut().set_settings(s.clone(), &ctx);
    assert_eq!(harness.state().names(), ["common.rpf", "GTA5.exe", "mods", "update", "x64", "zz_folder"]);
    s.grouping = Grouping::FoldersFirst;
    harness.state_mut().set_settings(s, &ctx);

    // Typing filters and selects the first match; Enter opens it.
    harness.event(egui::Event::Text("upd".into()));
    harness.run_steps(3);
    assert_eq!(harness.state().filter(), "upd");
    assert_eq!(harness.state().selected(), ["update"]);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert_eq!(folder(&harness), root.join("update"));
    assert_eq!(harness.state().filter(), "");

    // Mouse back and forward buttons.
    let pointer = |button, pressed| egui::Event::PointerButton {
        pos: egui::pos2(500.0, 300.0),
        button,
        pressed,
        modifiers: Default::default(),
    };
    harness.event(pointer(egui::PointerButton::Extra1, true));
    harness.event(pointer(egui::PointerButton::Extra1, false));
    harness.run_steps(3);
    assert_eq!(folder(&harness), root);
    harness.event(pointer(egui::PointerButton::Extra2, true));
    harness.event(pointer(egui::PointerButton::Extra2, false));
    harness.run_steps(3);
    assert_eq!(folder(&harness), root.join("update"));

    // Clicking a folder name in the side tree opens it.
    harness.get_all_by_label("x64").next().expect("x64 in the tree").click();
    harness.run_steps(3);
    assert_eq!(folder(&harness), root.join("x64"));

    // ...without expanding it: its subfolder shows up once, in the main view.
    harness.get_all_by_label("mods").next().unwrap().click();
    harness.run_steps(3);
    assert_eq!(folder(&harness), root.join("mods"));
    assert_eq!(harness.get_all_by_label("inner_dir").count(), 1);

    // Opening an archive from the folder view, then a folder inside it.
    harness.state_mut().go(Location::Folder(root.join("update")));
    harness.event(egui::Event::Text("update.rpf".into()));
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert_eq!(harness.state().names(), ["common"]);
}

#[test]
fn tree_all_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    harness.state_mut().go(Location::Folder(root.join("x64")));
    settle(&mut harness);

    // Off by default: the .exe is not in the tree.
    assert_eq!(harness.query_all_by_label("GTA5.exe").count(), 0);
    let mut s = harness.state().settings().clone();
    s.tree_all_files = true;
    harness.state_mut().set_settings(s, &ctx);
    harness.run_steps(3);
    harness.get_all_by_label("GTA5.exe").next().expect("exe in the tree").click();
    harness.run_steps(3);
    assert_eq!(folder(&harness), root);
    assert_eq!(harness.state().selected(), ["GTA5.exe"]);
}

#[test]
fn unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let archive = paleto_core::ArchivePath::new(root.join("update/update.rpf"));
    let data = archive.join("common").join("data");
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Archive(data.clone()));
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(2);
    harness.state_mut().edit_preview("<changed/>");
    harness.run_steps(2);

    // Leaving asks first; Cancel stays.
    harness.get_all_by_label("x64").next().unwrap().click();
    harness.run_steps(3);
    assert!(harness.state().asking_unsaved());
    harness.get_by_label("Cancel").click();
    harness.run_steps(3);
    assert!(!harness.state().asking_unsaved());
    assert_eq!(harness.state().previewing(), Some("handling.meta"));
    assert!(matches!(harness.state().location(), Some(Location::Archive(_))));

    // Save writes the file (to the mods copy), then carries on.
    harness.get_all_by_label("x64").next().unwrap().click();
    harness.run_steps(3);
    harness.get_all_by_label("💾 Save").last().unwrap().click();
    settle(&mut harness);
    assert_eq!(folder(&harness), root.join("x64"));
    let saved = paleto_core::ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data").join("handling.meta");
    assert_eq!(saved.read(None).unwrap(), b"<changed/>");

    // Discard drops the edit and carries on.
    harness.state_mut().go(Location::Archive(data));
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(2);
    harness.state_mut().edit_preview("<lost/>");
    harness.run_steps(2);
    harness.get_all_by_label("x64").next().unwrap().click();
    harness.run_steps(3);
    harness.get_by_label("Discard").click();
    harness.run_steps(3);
    assert_eq!(folder(&harness), root.join("x64"));
    assert_eq!(saved.read(None).unwrap(), b"<changed/>");
}

/// Double-clicks the last widget with this label (the main list is drawn
/// after the side tree) with real pointer events.
fn double_click(harness: &mut Harness<'_, App>, label: &str) {
    let pos = harness.get_all_by_label(label).last().unwrap_or_else(|| panic!("{label} not shown")).rect().center();
    harness.event(egui::Event::PointerMoved(pos));
    harness.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
            harness.step();
        }
    }
    harness.step();
}

#[test]
fn quick_double_clicks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    // Real-time frames, so that clicks are as fast as a user's.
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).with_step_dt(1.0 / 60.0).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);

    // Three double clicks in a row, well within egui's triple-click window.
    double_click(&mut harness, "update");
    assert_eq!(folder(&harness), root.join("update"));
    double_click(&mut harness, "update.rpf");
    assert_eq!(harness.state().names(), ["common"]);
    double_click(&mut harness, "common");
    assert_eq!(harness.state().names(), ["data"]);
}

#[test]
fn open_from_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let mut harness = Harness::builder().build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();

    harness.state_mut().open_path(&root.join("update"), &ctx);
    assert_eq!(folder(&harness), root.join("update"));
    harness.state_mut().open_path(&root.join("update/update.rpf"), &ctx);
    assert_eq!(harness.state().names(), ["common"]);
    harness.state_mut().open_path(&root.join("GTA5.exe"), &ctx);
    assert_eq!(folder(&harness), root);
    assert_eq!(harness.state().selected(), ["GTA5.exe"]);
}

/// In-memory stand-in for eframe's settings file.
#[derive(Default)]
struct Store(std::collections::HashMap<String, String>);

impl eframe::Storage for Store {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

#[test]
fn expand_collapse_and_remember() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    std::fs::create_dir_all(root.join("mods/inner_dir/deeper")).unwrap();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    harness.state_mut().go(Location::Folder(root.join("x64")));
    settle(&mut harness);
    assert_eq!(harness.query_all_by_label("deeper").count(), 0);

    // Expand all reaches nested folders; collapse all closes everything.
    harness.get_by_label("⏷ Expand all").click();
    harness.run_steps(10);
    assert_eq!(harness.query_all_by_label("deeper").count(), 1);
    harness.get_by_label("⏶ Collapse all").click();
    harness.run_steps(10);
    assert_eq!(harness.query_all_by_label("deeper").count(), 0);
    assert!(harness.state().open_folders().is_empty());

    // Open folders are only remembered when the option is on.
    harness.get_by_label("⏷ Expand all").click();
    harness.run_steps(3);
    let open = harness.state().open_folders();
    let mut store = Store::default();
    eframe::App::save(harness.state_mut(), &mut store);
    let mut fresh = Harness::builder().build_eframe(|cc| App::new(cc));
    fresh.state_mut().restore(&store, &ctx);
    assert!(fresh.state().open_folders().is_empty());

    let mut s = harness.state().settings().clone();
    s.remember_open_folders = true;
    harness.state_mut().set_settings(s, &ctx);
    eframe::App::save(harness.state_mut(), &mut store);
    fresh.state_mut().restore(&store, &ctx);
    assert_eq!(fresh.state().open_folders(), open);
    assert!(fresh.state().settings().remember_open_folders);
}

#[test]
fn help_and_about() {
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    harness.run_steps(2);
    harness.get_by_label("Help").click();
    harness.run_steps(2);
    harness.get_by_label("About").click();
    harness.run_steps(3);
    harness.get_by_label(&format!("Version {}", env!("CARGO_PKG_VERSION")));
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label(&format!("Version {}", env!("CARGO_PKG_VERSION"))).is_none());

    harness.get_by_label("Help").click();
    harness.run_steps(2);
    harness.get_by_label("Keyboard and mouse shortcuts").click();
    harness.run_steps(3);
    harness.get_by_label("Ctrl+S");
}

#[test]
fn preview_follows_selection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let mut ed = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
    ed.add_file("common/data/dlclist.xml", b"<list/>".to_vec()).unwrap();
    ed.add_file("common/data/zz_blob.bin", vec![0, 1, 2, 0, 255]).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    let data = paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common").join("data");
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Archive(data));
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(3);

    // Clicking another file, or moving with the arrows, previews it.
    harness.get_by_label("dlclist.xml").click();
    harness.run_steps(3);
    assert_eq!(harness.state().previewing(), Some("dlclist.xml"));
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(3);
    assert_eq!(harness.state().previewing(), Some("handling.meta"));

    // A file without a preview says so.
    harness.get_by_label("zz_blob.bin").click();
    harness.run_steps(3);
    assert_eq!(harness.state().previewing(), Some("zz_blob.bin"));
    harness.get_by_label("No preview for .bin files");

    // With unsaved edits it asks once; declining keeps the preview.
    harness.get_by_label("handling.meta").click();
    harness.run_steps(3);
    harness.state_mut().edit_preview("<edited/>");
    harness.run_steps(2);
    harness.get_by_label("dlclist.xml").click();
    harness.run_steps(3);
    assert!(harness.state().asking_unsaved());
    harness.get_by_label("Cancel").click();
    harness.run_steps(5);
    assert!(!harness.state().asking_unsaved());
    assert_eq!(harness.state().previewing(), Some("handling.meta"));
}

#[test]
fn tree_highlights_the_archive_opened() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::copy(root.join("update/update.rpf"), root.join("mods/update/update.rpf")).unwrap();
    let (original, copy) = (root.join("update/update.rpf"), root.join("mods/update/update.rpf"));

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    let archive = |path: &Path| Location::Archive(paleto_core::ArchivePath::new(path.to_path_buf()));
    let shown = |harness: &Harness<'_, App>| match harness.state().location() {
        Some(Location::Archive(a)) => a.disk.clone(),
        other => panic!("not an archive: {other:?}"),
    };

    // The original shows its mods copy, but stays the one highlighted, also inside it.
    harness.state_mut().go(archive(&original));
    settle(&mut harness);
    assert_eq!(shown(&harness), copy);
    assert_eq!(harness.state().tree_selection(), Some(original.clone()));
    harness.event(egui::Event::Text("common".into()));
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["data"]);
    assert_eq!(harness.state().tree_selection(), Some(original.clone()));
    harness.key_press(egui::Key::Backspace);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["common"]);
    assert_eq!(harness.state().tree_selection(), Some(original.clone()));

    // The mods copy itself, also inside it.
    harness.state_mut().go(archive(&copy));
    settle(&mut harness);
    assert_eq!(shown(&harness), copy);
    assert_eq!(harness.state().tree_selection(), Some(copy.clone()));
    harness.event(egui::Event::Text("common".into()));
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["data"]);
    assert_eq!(harness.state().tree_selection(), Some(copy.clone()));

    // History keeps which one was opened.
    let pointer = |button, pressed| egui::Event::PointerButton { pos: egui::pos2(500.0, 300.0), button, pressed, modifiers: Default::default() };
    let back = |harness: &mut Harness<'_, App>| {
        harness.event(pointer(egui::PointerButton::Extra1, true));
        harness.event(pointer(egui::PointerButton::Extra1, false));
        settle(harness);
    };
    back(&mut harness);
    assert_eq!(harness.state().tree_selection(), Some(copy.clone()));
    back(&mut harness);
    assert_eq!(harness.state().names(), ["common"]);
    assert_eq!(harness.state().tree_selection(), Some(original.clone()));
    harness.event(pointer(egui::PointerButton::Extra2, true));
    harness.event(pointer(egui::PointerButton::Extra2, false));
    settle(&mut harness);
    assert_eq!(harness.state().tree_selection(), Some(copy));
}

#[test]
fn menus_follow_the_pointer() {
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    harness.run_steps(2);
    // Pointing at a menu title opens nothing until a menu is open.
    harness.get_by_label("Help").hover();
    harness.run_steps(3);
    assert!(harness.query_by_label("About").is_none());

    harness.get_by_label("File").click();
    harness.run_steps(3);
    harness.get_by_label("Open archive…");
    harness.get_by_label("Help").hover();
    harness.run_steps(3);
    harness.get_by_label("About");
    assert!(harness.query_by_label("Open archive…").is_none());
}

#[test]
fn show_original_of_one_archive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    let mut ed = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
    ed.add_file("common/data/carcols.meta", b"<c/>".to_vec()).unwrap();
    ed.save(&root.join("mods/update/update.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    // The game's update.rpf is marked as read from the mods folder.
    harness.state_mut().go(Location::Folder(root.join("update")));
    harness.run_steps(2);
    harness.get_by_label("↪");

    harness.state_mut().go(Location::Archive(paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common").join("data")));
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["carcols.meta", "handling.meta"]);
    harness.get_by_label("MODS");

    harness.get_by_label("↩ Show original").click();
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["handling.meta"]);
    harness.get_by_label("ORIGINAL");
    // It stays on the original inside the same archive…
    harness.state_mut().go(Location::Archive(paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common")));
    settle(&mut harness);
    harness.get_by_label("ORIGINAL");
    harness.get_by_label("↪ Show mods copy").click();
    settle(&mut harness);
    harness.get_by_label("MODS");

    // …and leaving the archive ends it.
    harness.get_by_label("↩ Show original").click();
    settle(&mut harness);
    harness.state_mut().go(Location::Folder(root.join("update")));
    harness.run_steps(2);
    harness.state_mut().go(Location::Archive(paleto_core::ArchivePath::new(root.join("update/update.rpf"))));
    settle(&mut harness);
    harness.get_by_label("MODS");
}

#[test]
fn search_in_a_text() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let mut ed = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
    ed.add_file("common/data/notes.txt", b"alpha beta\nBeta gamma\nlast\nbeta".to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    let data = paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common").join("data");
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Archive(data));
    harness.state_mut().preview("notes.txt", &ctx);
    harness.run_steps(3);
    // A plain file has nothing to add to what the list says.
    assert_eq!(harness.state().preview_info(), Some(""));

    // Ctrl+F opens the search with the keyboard in it; the matches count either case.
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    harness.run_steps(3);
    harness.event(egui::Event::Text("beta".into()));
    harness.run_steps(3);
    harness.get_by_label("1 of 3");
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    harness.get_by_label("2 of 3");
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    harness.run_steps(3);
    harness.get_by_label("1 of 3");

    // Nothing found says so; Esc closes the search, and the text is untouched.
    harness.event(egui::Event::Text("zzz".into()));
    harness.run_steps(3);
    harness.get_by_label("No match");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("No match").is_none());
    assert_eq!(harness.state().preview_text(), Some("alpha beta\nBeta gamma\nlast\nbeta"));
    assert_eq!(harness.state_mut().search_preview("gamma"), Some(1));
}

#[test]
fn side_tree_can_be_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    fake_game(&root);
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.get_by_label("⏷ Expand all");

    let mut s = harness.state().settings().clone();
    s.show_tree = false;
    harness.state_mut().set_settings(s, &ctx);
    harness.run_steps(3);
    assert!(harness.query_by_label("⏷ Expand all").is_none());
    // The file list is still there.
    harness.get_by_label("update");

    let mut s = harness.state().settings().clone();
    s.show_tree = true;
    harness.state_mut().set_settings(s, &ctx);
    harness.run_steps(3);
    harness.get_by_label("⏷ Expand all");
}
