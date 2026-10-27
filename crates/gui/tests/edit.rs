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

    // In the game folder, original game data is neither deleted nor renamed;
    // other files (plugins, scripts) are.
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("x64a.rpf"), b"rpf").unwrap();
    std::fs::write(root.join("plugin.asi"), b"asi").unwrap();
    harness.state_mut().go(Location::Folder(root.clone()));
    harness.run_steps(2);
    harness.state_mut().delete_now(&["plugin.asi", "x64a.rpf"], &ctx);
    settle(&mut harness);
    assert!(harness.state().status_error().is_some_and(|e| e.contains("x64a.rpf is an original game file")));
    assert!(root.join("x64a.rpf").is_file() && root.join("plugin.asi").is_file());
    harness.get_by_label("update").click();
    harness.run_steps(2);
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    assert_eq!(harness.state().name_dialog(), None);
    harness.state_mut().delete_now(&["plugin.asi"], &ctx);
    settle(&mut harness);
    assert!(!root.join("plugin.asi").exists() && root.join("update").is_dir());
}

/// A fake game, and an archive of its own outside it, opened with
/// `handling.meta` previewed.
fn previewed_archive(dir: &std::path::Path) -> (Harness<'static, App>, std::path::PathBuf) {
    let root = dir.join("game");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let archive = dir.join("loose.rpf");
    let mut editor = ArchiveEditor::new("loose.rpf");
    editor.add_file("handling.meta", b"<handling/>".to_vec()).unwrap();
    editor.add_file("notes.txt", b"notes".to_vec()).unwrap();
    editor.save(&archive).unwrap();

    let mut harness = Harness::builder().with_size([1100.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(archive.clone()));
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(2);
    assert_eq!(harness.state().preview_text(), Some("<handling/>"));
    (harness, archive)
}

/// A named pipe: a task reading it waits until the test writes to it, so it
/// stays running for as long as the test needs, without timing.
fn pipe(path: &std::path::Path) {
    assert!(std::process::Command::new("mkfifo").arg(path).status().unwrap().success());
}

/// Lets the task reading the pipe finish. From a thread of its own: should no
/// task read it, the test then fails instead of waiting forever.
fn release(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || std::fs::write(path, b"slow"));
}

/// Whether the preview holds unsaved edits: closing it then asks first (and
/// is cancelled). Without edits it closes.
fn unsaved(harness: &mut Harness<'_, App>) -> bool {
    use egui_kittest::kittest::Queryable;

    let ctx = harness.ctx.clone();
    harness.state_mut().close_preview(&ctx);
    harness.run_steps(2);
    let asking = harness.state().asking_unsaved();
    if asking {
        harness.get_by_label("Cancel").click();
        harness.run_steps(3);
        assert!(!harness.state().asking_unsaved());
    }
    asking
}

/// Asks to close the window, as its close button does; whether the app
/// cancelled the close.
fn close_window(harness: &mut Harness<'_, App>) -> bool {
    harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    harness.step();
    harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
}

#[test]
fn one_operation_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, archive) = previewed_archive(dir.path());
    let ctx = harness.ctx.clone();
    harness.state_mut().edit_preview("<edited/>");
    let slow = dir.path().join("slow.txt");
    pipe(&slow);
    harness.state_mut().add_files(vec![slow.clone()], &ctx);
    harness.run_steps(2);
    assert!(!harness.state().idle());

    // Another operation is refused instead of replacing the running one, also from Ctrl+S.
    harness.state_mut().delete_now(&["notes.txt"], &ctx);
    assert!(harness.state().status_error().is_some_and(|e| e.contains("Wait for the current operation")));
    harness.state_mut().cancel_dialogs();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    harness.run_steps(3);

    // The running one carries on to its end; the others did nothing.
    release(&slow);
    settle(&mut harness);
    assert_eq!(harness.state().status_error(), None);
    assert_eq!(harness.state().names(), ["handling.meta", "notes.txt", "slow.txt"]);
    let inside = ArchivePath::new(archive);
    assert_eq!(inside.join("slow.txt").read(None).unwrap(), b"slow");
    assert_eq!(inside.join("handling.meta").read(None).unwrap(), b"<handling/>");
    assert_eq!(harness.state().preview_text(), Some("<edited/>"));
    assert!(unsaved(&mut harness));
}

#[test]
fn closing_while_busy_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _archive) = previewed_archive(dir.path());
    let ctx = harness.ctx.clone();
    // Nothing to lose: the window closes at once.
    assert!(!close_window(&mut harness));

    let slow = dir.path().join("slow.txt");
    pipe(&slow);
    harness.state_mut().add_files(vec![slow.clone()], &ctx);
    harness.run_steps(2);
    // The first request only warns; a second one closes.
    assert!(close_window(&mut harness));
    assert!(harness.state().status_error().is_some_and(|e| e.contains("Adding files is not finished")));
    assert!(!close_window(&mut harness));

    // Once the operation is over, closing no longer asks.
    release(&slow);
    settle(&mut harness);
    assert!(!close_window(&mut harness));
}

#[test]
fn failed_save_keeps_the_edits_unsaved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, archive) = previewed_archive(dir.path());
    let ctx = harness.ctx.clone();
    harness.state_mut().edit_preview("<edited/>");
    // The archive is gone by the time it is saved.
    let original = std::fs::read(&archive).unwrap();
    std::fs::remove_file(&archive).unwrap();
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    assert!(harness.state().status_error().is_some());
    assert_eq!(harness.state().preview_text(), Some("<edited/>"));
    assert!(unsaved(&mut harness));

    // Saved once it can be.
    std::fs::write(&archive, original).unwrap();
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    assert_eq!(harness.state().status_error(), None);
    assert_eq!(ArchivePath::new(archive).join("handling.meta").read(None).unwrap(), b"<edited/>");
    assert!(!unsaved(&mut harness));
}

#[test]
fn previewing_the_edited_file_again_keeps_the_edits() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _archive) = previewed_archive(dir.path());
    let ctx = harness.ctx.clone();
    harness.state_mut().edit_preview("<edited/>");
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(2);
    assert_eq!(harness.state().preview_text(), Some("<edited/>"));
    assert!(unsaved(&mut harness));
}
