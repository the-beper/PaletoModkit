//! The texture dictionary viewer: listing, selection, export and editing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::ArchivePath;
use paleto_modkit::{App, Export, TextureEdit};
use paleto_rpf::ArchiveEditor;

/// Two 8x8 textures: "red_probe" (opaque red) and "blue_probe" (half-transparent blue).
const PROBE: &[u8] = include_bytes!("data/probe.ytd");

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

#[test]
fn preview_select_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("textures/probe.ytd", PROBE.to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("textures"));
    harness.state_mut().preview("probe.ytd", &ctx);
    settle(&mut harness);

    let names = || vec!["red_probe".to_string(), "blue_probe".to_string()];
    assert_eq!(harness.state().preview_textures(), Some((names(), 0)));
    harness.get_by_label_contains("blue_probe").click();
    harness.run_steps(3);
    assert_eq!(harness.state().preview_textures(), Some((names(), 1)));

    // Export all as DDS: one file per texture, raw data after the 128-byte header.
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    harness.state_mut().export_textures(Export::All { png: false }, &out, &ctx);
    settle(&mut harness);
    let dds = std::fs::read(out.join("blue_probe.dds")).unwrap();
    assert_eq!(&dds[..4], b"DDS ");
    assert_eq!(&dds[128..132], &[255, 0, 0, 128]);
    assert!(out.join("red_probe.dds").is_file());

    // Export the selected texture as PNG.
    let png = dir.path().join("picked.png");
    harness.state_mut().export_textures(Export::Selected { png: true }, &png, &ctx);
    settle(&mut harness);
    let image = image::open(&png).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (8, 8));
    assert_eq!(image.get_pixel(3, 3).0, [0, 0, 255, 128]);

    // Replace blue_probe with red_probe's DDS: the mods copy changes, the
    // original archive does not, and the preview reloads on the same texture.
    let original = std::fs::read(root.join("update/update.rpf")).unwrap();
    let replace = |file: PathBuf| TextureEdit::Replace { name: "blue_probe".into(), file, format: None };
    harness.state_mut().edit_texture(replace(out.join("red_probe.dds")), &ctx);
    settle(&mut harness);
    assert_eq!(std::fs::read(root.join("update/update.rpf")).unwrap(), original);
    assert!(root.join("mods/update/update.rpf").is_file());
    assert_eq!(harness.state().preview_textures(), Some((names(), 1)));
    harness.state_mut().export_textures(Export::Selected { png: true }, &png, &ctx);
    settle(&mut harness);
    assert_eq!(image::open(&png).unwrap().to_rgba8().get_pixel(3, 3).0, [255, 0, 0, 255]);

    // A file that is neither DDS nor PNG is refused and nothing changes.
    let modded = std::fs::read(root.join("mods/update/update.rpf")).unwrap();
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "hello").unwrap();
    harness.state_mut().edit_texture(replace(text), &ctx);
    settle(&mut harness);
    assert_eq!(std::fs::read(root.join("mods/update/update.rpf")).unwrap(), modded);
    assert!(harness.state().status_error().is_some_and(|e| e.contains("expected a DDS or PNG file")), "{:?}", harness.state().status_error());
}

#[test]
fn add_rename_and_remove_textures() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("textures/probe.ytd", PROBE.to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("textures"));
    harness.state_mut().preview("probe.ytd", &ctx);
    settle(&mut harness);

    // Add a texture from a PNG: the dialog offers a name and a format, and
    // the new texture is selected.
    let picture = dir.path().join("green.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 255, 0, 255])).save(&picture).unwrap();
    let add = TextureEdit::Add { name: "green".into(), file: picture.clone(), format: None };
    harness.state_mut().ask_texture(add, &ctx);
    harness.run_steps(3);
    if let Ok(dir) = std::env::var("PALETO_SCREENS") {
        harness.render().unwrap().save(Path::new(&dir).join("add-texture.png")).unwrap();
    }
    harness.event(egui::Event::Text("green_probe".into()));
    harness.get_by_label("Format").click();
    harness.run_steps(2);
    harness.get_by_label("BC3 (DXT5)").click();
    harness.run_steps(2);
    harness.get_by_label("Add").click();
    settle(&mut harness);
    let (names, selected) = harness.state().preview_textures().unwrap();
    assert_eq!(names.len(), 3);
    assert_eq!(names[selected], "green_probe");
    assert!(harness.query_by_label_contains("BC3 (DXT5)").is_some());

    // Without a format, an opaque picture becomes BC1.
    let add = TextureEdit::Add { name: "green_bc1".into(), file: picture, format: None };
    harness.state_mut().edit_texture(add, &ctx);
    settle(&mut harness);
    assert!(harness.query_by_label_contains("BC1 (DXT1)").is_some());
    harness.state_mut().edit_texture(TextureEdit::Remove("green_bc1".into()), &ctx);
    settle(&mut harness);

    // Rename it through the Edit menu and the dialog.
    harness.get_by_label_contains("green_probe").click();
    harness.run_steps(2);
    harness.get_by_label("✏ Edit").click();
    harness.run_steps(2);
    harness.get_by_label("Rename…").click();
    harness.run_steps(3);
    assert!(matches!(harness.state().texture_dialog(), Some(TextureEdit::Rename { .. })));
    // The name is selected: typing replaces it. An existing name is refused.
    harness.event(egui::Event::Text("red_probe".into()));
    harness.run_steps(2);
    harness.get_by_label("A texture with this name already exists.");
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(1);
    harness.event(egui::Event::Text("lime_probe".into()));
    harness.run_steps(2);
    harness.get_by_label("Rename").click();
    settle(&mut harness);
    assert!(harness.state().texture_dialog().is_none());
    let (names, selected) = harness.state().preview_textures().unwrap();
    assert_eq!(names[selected], "lime_probe");
    assert!(!names.contains(&"green_probe".to_string()));

    // Remove it, after confirming.
    harness.get_by_label("✏ Edit").click();
    harness.run_steps(2);
    harness.get_by_label("🗑 Remove").click();
    harness.run_steps(3);
    harness.get_by_label("Remove").click();
    settle(&mut harness);
    let (mut names, _) = harness.state().preview_textures().unwrap();
    names.sort();
    assert_eq!(names, ["blue_probe", "red_probe"]);
    assert!(harness.state().status_error().is_none());
}

/// A file dropped on the window, as the windowing integration hands it over.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// The pixel at (0, 0) of a texture in the mods copy's dictionary.
fn first_pixel(root: &Path, texture: &str) -> [u8; 4] {
    let ytd = ArchivePath::new(root.join("mods/update/update.rpf")).join("textures").join("probe.ytd").read(None).unwrap();
    let dict = paleto_formats::TextureDictionary::parse(&ytd).unwrap();
    let tex = dict.textures.iter().find(|t| t.name == texture).unwrap();
    let (_, _, rgba) = tex.rgba(0).unwrap();
    rgba[..4].try_into().unwrap()
}

#[test]
fn replace_textures_from_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("textures/probe.ytd", PROBE.to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    // A mod's folder: pictures named after the textures (in any case), one new.
    let pack = dir.path().join("Better Probes/textures");
    std::fs::create_dir_all(&pack).unwrap();
    let green = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255]));
    green.save(pack.join("RED_PROBE.png")).unwrap();
    green.save(pack.join("brand_new.png")).unwrap();
    std::fs::write(pack.join("readme.txt"), "install with OpenIV").unwrap();

    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("textures"));
    harness.state_mut().preview("probe.ytd", &ctx);
    settle(&mut harness);

    // Dropping the folder on the open dictionary matches its pictures by name.
    harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(dir.path().join("Better Probes"))));
    harness.run_steps(3);
    let batch = harness.state().texture_batch().unwrap().clone();
    assert_eq!(batch.replace, [("red_probe".to_string(), pack.join("RED_PROBE.png"))]);
    assert_eq!(batch.unmatched, [pack.join("brand_new.png")]);
    harness.get_by_label("Add them as new textures");
    if let Ok(dir) = std::env::var("PALETO_SCREENS") {
        harness.render().unwrap().save(Path::new(&dir).join("texture-batch.png")).unwrap();
    }

    // Replace, adding the new one: one change to the mods copy.
    harness.state_mut().confirm_texture_batch(true, &ctx);
    settle(&mut harness);
    let (names, _) = harness.state().preview_textures().unwrap();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"brand_new".to_string()));
    assert_eq!(first_pixel(&root, "red_probe"), [0, 255, 0, 255]);
    // The untouched texture keeps its pixels.
    assert_eq!(first_pixel(&root, "blue_probe")[2], 255);
    assert!(harness.state().status_error().is_none());
}
