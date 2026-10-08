//! Editing an archive through the GUI, end to end (no game files needed).

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
fn add_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("test.rpf");
    ArchiveEditor::new("test.rpf").save(&archive).unwrap();
    std::fs::create_dir_all(dir.path().join("pack/sub")).unwrap();
    std::fs::write(dir.path().join("one.txt"), "one").unwrap();
    std::fs::write(dir.path().join("pack/sub/two.xml"), "<two/>").unwrap();

    let mut harness = Harness::builder().build_eframe(|cc| App::new(cc));
    settle(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().open(ArchivePath::new(archive.clone()));
    harness.state_mut().add_files(vec![dir.path().join("one.txt"), dir.path().join("pack")], &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["pack", "one.txt"]); // folders first

    harness.state_mut().open(ArchivePath::new(archive.clone()).join("pack").join("sub"));
    assert_eq!(harness.state().names(), ["two.xml"]);
    assert_eq!(ArchivePath::new(archive.clone()).join("pack").join("sub").join("two.xml").read(None).unwrap(), b"<two/>");

    harness.state_mut().open(ArchivePath::new(archive.clone()));
    harness.state_mut().delete_now(&["pack"], &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["one.txt"]);
}

#[test]
fn rename_and_new_folder() {
    use egui_kittest::kittest::Queryable;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("common/data/handling.meta", b"<handling/>".to_vec()).unwrap();
    ed.add_file("common/data/dlclist.xml", b"<list/>".to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    let original = std::fs::read(root.join("update/update.rpf")).unwrap();
    let modded = ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data");

    let mut harness = Harness::builder().with_size([1100.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("common").join("data"));
    harness.run_steps(2);

    // F2 on the selected file selects the name without its extension, so
    // typing replaces just that part.
    harness.get_by_label("handling.meta").click();
    harness.run_steps(2);
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    harness.event(egui::Event::Text("Carcols".into()));
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["carcols.meta", "dlclist.xml"]);
    assert_eq!(harness.state().selected(), ["carcols.meta"]);
    assert_eq!(modded.join("carcols.meta").read(None).unwrap(), b"<handling/>");
    assert_eq!(std::fs::read(root.join("update/update.rpf")).unwrap(), original, "original archive untouched");

    // A name that is already taken cannot be confirmed.
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(1);
    harness.event(egui::Event::Text("dlclist.xml".into()));
    harness.run_steps(1);
    assert_eq!(harness.state().name_dialog(), Some("dlclist.xml"));
    harness.get_by_label("An item with this name already exists here.");
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert_eq!(harness.state().names(), ["carcols.meta", "dlclist.xml"]);

    // Ctrl+Shift+N creates an empty folder, listed first and selected.
    harness.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::N);
    harness.run_steps(2);
    harness.event(egui::Event::Text("My Stuff".into()));
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["my stuff", "carcols.meta", "dlclist.xml"]);
    assert_eq!(harness.state().selected(), ["my stuff"]);
    let (archive, idx) = modded.join("my stuff").open(None).unwrap();
    assert!(archive.entries[idx].is_dir() && archive.children(idx).is_empty());
}

#[test]
fn format_xml() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("update.rpf");
    let messy = "<Paths>\r\n        <Item>a</Item>\n<Item>b</Item>\r\n  \t\t<Item>c</Item>\r\n\t</Paths>\r\n";
    let mut editor = ArchiveEditor::new("update.rpf");
    editor.add_file("dlclist.xml", messy.as_bytes().to_vec()).unwrap();
    editor.save(&archive).unwrap();

    let mut harness = Harness::builder().build_eframe(|cc| App::new(cc));
    settle(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().open(ArchivePath::new(archive.clone()));
    harness.state_mut().preview("dlclist.xml", &ctx);
    harness.run_steps(2);
    harness.state_mut().format_preview(&ctx);
    // Only whitespace changes, line endings included, and only once saved.
    let tidy = "<Paths>\r\n\t<Item>a</Item>\n\t<Item>b</Item>\r\n\t<Item>c</Item>\r\n</Paths>\r\n";
    assert_eq!(harness.state().preview_text(), Some(tidy));
    assert_eq!(ArchivePath::new(archive.clone()).join("dlclist.xml").read(None).unwrap(), messy.as_bytes());
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    assert_eq!(ArchivePath::new(archive).join("dlclist.xml").read(None).unwrap(), tidy.as_bytes());
}

#[test]
fn rename_and_delete_on_disk() {
    use egui_kittest::kittest::Queryable;
    use paleto_modkit::Location;

    let dir = tempfile::tempdir().unwrap();
    // A game of its own: without one the window shows its welcome page.
    let root = dir.path().join("game");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let folder = root.join("mods");
    std::fs::create_dir_all(folder.join("old/sub")).unwrap();
    std::fs::write(folder.join("old/sub/a.txt"), "a").unwrap();
    std::fs::write(folder.join("notes.txt"), "notes").unwrap();

    let mut harness = Harness::builder().with_size([1100.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().go(Location::Folder(folder.clone()));
    harness.run_steps(2);
    assert_eq!(harness.state().names(), ["old", "notes.txt"]);

    // On disk, a change of case alone is a rename.
    harness.get_by_label("notes.txt").click();
    harness.run_steps(2);
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    harness.event(egui::Event::Text("Notes".into()));
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["old", "Notes.txt"]);
    assert_eq!(harness.state().selected(), ["Notes.txt"]);
    assert_eq!(std::fs::read(folder.join("Notes.txt")).unwrap(), b"notes");

    harness.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::N);
    harness.run_steps(2);
    harness.event(egui::Event::Text("New Stuff".into()));
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["New Stuff", "old", "Notes.txt"]);
    assert!(folder.join("New Stuff").is_dir());

    // Folders go with all they hold; the list refreshes by itself.
    harness.state_mut().delete_now(&["old", "Notes.txt"], &ctx);
    settle(&mut harness);
    assert_eq!(harness.state().names(), ["New Stuff"]);
    assert!(!folder.join("old").exists() && !folder.join("Notes.txt").exists());
}
