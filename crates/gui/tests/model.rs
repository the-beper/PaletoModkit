//! The model viewer: exporting a model as CodeWalker XML.

use std::path::Path;
use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::ArchivePath;
use paleto_modkit::App;
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

/// A Legacy `.ydr` whose drawable has no shaders, skeleton or models: one empty
/// system page (the drawable's header is all zeros), stored uncompressed.
fn empty_drawable() -> Vec<u8> {
    let version = 165u32;
    let one_page = 1u32 << 27; // one page of 0x200 bytes
    let mut data = b"RSC7".to_vec();
    for word in [version, ((version >> 4) << 28) | one_page, ((version & 0xF) << 28) | one_page] {
        data.extend(word.to_le_bytes());
    }
    let payload = vec![0u8; 0x400];
    data.push(0x01); // a final, stored deflate block
    data.extend((payload.len() as u16).to_le_bytes());
    data.extend((!(payload.len() as u16)).to_le_bytes());
    data.extend(payload);
    data
}

/// A game folder whose `update.rpf` holds `models/prop_probe.ydr`, with that model previewed.
fn preview_probe(harness: &mut Harness<'_, App>, dir: &Path) {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("models/prop_probe.ydr", empty_drawable()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("models"));
    settle(harness);
    harness.state_mut().preview("prop_probe.ydr", &ctx);
    settle(harness);
}

/// Writes an executable script for a test. A file this process wrote can fail with "Text file busy" while another thread
/// forks (the fork holds it open for writing until it runs its program): `cp` writes it instead.
fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let draft = path.with_extension("draft");
    std::fs::write(&draft, text).unwrap();
    assert!(std::process::Command::new("cp").arg(&draft).arg(path).status().unwrap().success());
    std::fs::remove_file(&draft).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
fn export_model_as_xml() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    preview_probe(&mut harness, dir.path());
    harness.get_by_label_contains("Export as XML");

    // The folder's name has spaces; the program is set, so Blender is not looked for.
    harness.state_mut().set_model_program("paleto-no-such-editor {file}");
    let out = dir.path().join("out dir");
    harness.state_mut().export_model(&out, &ctx);
    settle(&mut harness);
    let xml = std::fs::read_to_string(out.join("prop_probe.ydr.xml")).unwrap();
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Drawable>\n"), "{xml}");
    assert!(xml.contains("<Name>prop_probe</Name>"), "{xml}");
    let note = std::fs::read_to_string(out.join("prop_probe.ydr.export.txt")).unwrap();
    // Named from the game folder, not by the user's folders.
    assert!(note.contains("Model: update/update.rpf/models/prop_probe.ydr\n") && !note.contains(&dir.path().display().to_string()), "{note}");
    assert!(note.contains("Texture dictionaries searched: none"), "{note}");

    harness.get_by_label("Model exported");
    // The dialog says it apart from the status bar ("exported …").
    harness.get_by_label("prop_probe.ydr.xml and 0 textures in");
    harness.get_by_label(&out.display().to_string());
    harness.get_by_label("Open with");

    harness.get_by_role_and_label(Role::Button, "Close").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Model exported").is_none());
}

#[test]
fn exported_model_opens_in_the_program_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    preview_probe(&mut harness, dir.path());

    // The script and the export folder have spaces in their names: each stays one argument.
    let bin = dir.path().join("my programs");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("edit.sh");
    let args = bin.join("args.txt");
    write_script(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.tmp\"\nmv \"$0.tmp\" \"$(dirname \"$0\")/args.txt\"\n");
    harness.state_mut().set_model_program(&format!("'{}' {{file}}", script.display()));
    let out = dir.path().join("out dir");
    harness.state_mut().export_model(&out, &ctx);
    settle(&mut harness);
    harness.get_by_label("Model exported");

    harness.get_by_role_and_label(Role::Button, "Open").click();
    harness.run_steps(3);
    harness.get_by_label("Started");
    let mut got = None;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&args) {
            got = Some(text);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(got.as_deref(), Some(format!("{}\n", out.join("prop_probe.ydr.xml").display()).as_str()));
}

#[test]
fn a_missing_program_is_said_in_the_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    preview_probe(&mut harness, dir.path());
    harness.state_mut().set_model_program("paleto-no-such-editor {file}");
    harness.state_mut().export_model(&dir.path().join("out"), &ctx);
    settle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "Open").click();
    harness.run_steps(3);
    harness.get_by_label_contains("paleto-no-such-editor is not installed");
    harness.get_by_label("Model exported");
}

#[test]
fn the_program_is_kept_between_sessions() {
    let mut harness = Harness::builder().build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().set_model_program("blender --python-use-system-env {dir}");
    let mut store = Store::default();
    eframe::App::save(harness.state_mut(), &mut store);
    assert_eq!(store.0.get("model_program").map(String::as_str), Some("blender --python-use-system-env {dir}"));
    let mut fresh = Harness::builder().build_eframe(|cc| App::new(cc));
    assert_eq!(fresh.state().model_program(), "");
    fresh.state_mut().restore(&store, &ctx);
    assert_eq!(fresh.state().model_program(), "blender --python-use-system-env {dir}");
}
