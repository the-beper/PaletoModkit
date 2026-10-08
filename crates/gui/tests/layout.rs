//! Every control stays inside the window as it shrinks: toolbars wrap or
//! show icons, side panels leave room for the file list, lists shorten names.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
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

/// Controls (buttons, check boxes, combo boxes, text fields) whose right edge
/// is past the window's.
fn overflowing(harness: &Harness<'_, App>, width: f32) -> Vec<String> {
    use egui::accesskit::Role;
    let controls = [Role::Button, Role::CheckBox, Role::ComboBox, Role::TextInput, Role::Link];
    harness
        .query_all(By::new())
        .filter(|n| controls.contains(&n.accesskit_node().role()))
        .filter(|n| n.rect().right() > width + 0.5)
        .map(|n| format!("{:?} {:?} at {:?}", n.accesskit_node().role(), n.accesskit_node().label(), n.rect()))
        .collect()
}

#[test]
fn controls_fit_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let archive = root.join("update/update.rpf");
    let mut editor = ArchiveEditor::new("update.rpf");
    editor.add_file("common/data/a_rather_long_file_name_for_a_narrow_list.xml", b"<a>\n<b/>\n</a>\n".to_vec()).unwrap();
    // A texture with mipmaps, so the preview also shows its level picker.
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(256, 128, image::Rgba([0, 128, 255, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let add = paleto_formats::Edit::Add { name: "a_texture_with_a_rather_long_name", file: &png, format: None };
    let edits = [add, paleto_formats::Edit::Remove("red_probe"), paleto_formats::Edit::Remove("blue_probe")];
    let ytd = paleto_formats::edit_textures(include_bytes!("data/probe.ytd"), &edits).unwrap();
    editor.add_file("common/data/vehicle_textures_with_a_long_name.ytd", ytd).unwrap();
    editor.save(&archive).unwrap();
    let data = ArchivePath::new(archive).join("common").join("data");

    for (width, height) in [(1280.0, 800.0), (800.0, 600.0), (640.0, 480.0)] {
        for file in ["a_rather_long_file_name_for_a_narrow_list.xml", "vehicle_textures_with_a_long_name.ytd"] {
            let mut harness = Harness::builder().with_size([width, height]).build_eframe(|cc| App::new(cc));
            let ctx = harness.ctx.clone();
            harness.state_mut().select_game_folder(&root, &ctx);
            settle(&mut harness);
            harness.state_mut().open(data.clone());
            harness.state_mut().preview(file, &ctx);
            settle(&mut harness);
            let outside = overflowing(&harness, width);
            assert!(outside.is_empty(), "{width}×{height}, {file}: {outside:#?}");
            // The file list keeps room: its rows are on screen, left of the preview.
            // (The name is also the preview's title: the row is the leftmost.)
            let row = harness.get_all_by_label("a_rather_long_file_name_for_a_narrow_list.xml").map(|n| n.rect()).min_by(|a, b| a.left().total_cmp(&b.left())).unwrap();
            assert!(row.width() > 40.0 && row.right() <= width, "{width}×{height}: {row:?}");
        }
    }
}

#[test]
fn mods_window_fits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let resource = |version: u32| {
        let mut data = b"RSC7".to_vec();
        for word in [version, (version >> 4) << 28, (version & 15) << 28] {
            data.extend(word.to_le_bytes());
        }
        data.extend([0u8; 32]);
        data
    };
    ArchiveEditor::new("x64e.rpf").save(&root.join("x64e.rpf")).unwrap();
    let mut copy = ArchiveEditor::new("x64e.rpf");
    for i in 0..30 {
        copy.add_file(&format!("levels/gta5/vehicles/a_vehicle_with_a_long_name_{i}.yft"), resource(162)).unwrap();
    }
    copy.save(&root.join("mods/x64e.rpf")).unwrap();

    for (width, height) in [(1280.0, 800.0), (800.0, 600.0), (640.0, 480.0)] {
        let mut harness = Harness::builder().with_size([width, height]).build_eframe(|cc| App::new(cc));
        let ctx = harness.ctx.clone();
        harness.state_mut().select_game_folder(&root, &ctx);
        settle(&mut harness);
        harness.state_mut().check_mods(&ctx);
        settle(&mut harness);
        harness.get_by_label("Mods in game");
        harness.get_by_label("30 files in the mods folder were made for Legacy: the Enhanced game may crash on them or ignore them.");
        let outside = overflowing(&harness, width);
        assert!(outside.is_empty(), "{width}×{height}: {outside:#?}");
        let bottom = harness.get_by_label("Refresh").rect().bottom();
        assert!(bottom <= height, "{width}×{height}: Refresh at {bottom}");
        if width == 800.0 && std::env::var_os("PALETO_SHOT").is_some() {
            let image = harness.render().expect("render");
            image.save(std::env::var("PALETO_SHOT").unwrap()).unwrap();
        }
    }
}

#[test]
fn location_bar_fits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("common/data/a_rather_long_folder_name/handling.meta", b"<x/>".to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    std::fs::copy(root.join("update/update.rpf"), root.join("mods/update/update.rpf")).unwrap();
    let deep = paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common").join("data").join("a_rather_long_folder_name");

    for (width, height) in [(1280.0, 800.0), (800.0, 600.0), (640.0, 480.0)] {
        let mut harness = Harness::builder().with_size([width, height]).build_eframe(|cc| App::new(cc));
        let ctx = harness.ctx.clone();
        harness.state_mut().select_game_folder(&root, &ctx);
        settle(&mut harness);
        harness.state_mut().open(deep.clone());
        settle(&mut harness);
        let button = harness.get_by_label("↩ Show original").rect();
        assert!(button.right() <= width, "{width}×{height}: {button:?}");
        let outside = overflowing(&harness, width);
        assert!(outside.is_empty(), "{width}×{height}: {outside:#?}");
    }
}
