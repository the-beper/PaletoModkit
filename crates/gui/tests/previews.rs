//! Previews of clip dictionaries (.ycd, a read-only summary), collision (.ybn) and
//! navmeshes (.ynv, both in the 3D view), and the Face menu of cube maps.

use std::time::Duration;

use eframe::egui;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use paleto_core::ArchivePath;
use paleto_modkit::App;
use paleto_rpf::ArchiveEditor;

const SYS: u64 = 0x5000_0000;

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

/// A game folder whose update.rpf holds `files` in data/.
fn game_with(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    for (name, data) in files {
        ed.add_file(&format!("data/{name}"), data.clone()).unwrap();
    }
    ed.save(&root.join("update/update.rpf")).unwrap();
    (dir, root)
}

fn open(root: &std::path::Path, file: &str) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("data"));
    settle(&mut harness);
    harness.state_mut().preview(file, &ctx);
    settle(&mut harness);
    harness
}

/// A resource of `system` (at most 0xFFFF bytes, its size given by `pages`),
/// stored in one uncompressed deflate block.
fn rsc(version: u32, pages: u32, system: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    for w in [0x3743_5352, version, ((version >> 4) << 28) | pages, (version & 0xF) << 28] {
        data.extend(u32::to_le_bytes(w));
    }
    let len = u16::try_from(system.len()).unwrap();
    data.push(0x01); // a final, stored deflate block
    data.extend(len.to_le_bytes());
    data.extend((!len).to_le_bytes());
    data.extend(system);
    data
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Two clips (one animation, one group) sharing one animation, as in the formats tests.
fn clip_dictionary() -> Vec<u8> {
    let mut system = vec![0u8; 0x400];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    let ptr = |at: u64| (SYS + at).to_le_bytes();
    put(0x18, &ptr(0x100));
    put(0x28, &ptr(0x140));
    put(0x30, &[2, 0, 2, 0]);
    put(0x100 + 0x18, &ptr(0x160));
    put(0x100 + 0x20, &[2, 0, 1, 0]);
    put(0x160, &ptr(0x1A0));
    put(0x1A0, &0x1234u32.to_le_bytes());
    put(0x1A8, &ptr(0x200));
    put(0x140, &ptr(0x1C0));
    put(0x148, &ptr(0x1E0));
    put(0x1C0, &0xAAAAu32.to_le_bytes());
    put(0x1C8, &ptr(0x260));
    put(0x1E0, &0xBBBBu32.to_le_bytes());
    put(0x1E8, &ptr(0x300));
    put(0x214, &31u16.to_le_bytes());
    put(0x216, &32u16.to_le_bytes());
    put(0x218, &1.0f32.to_le_bytes());
    put(0x248, &1u16.to_le_bytes());
    put(0x250, &ptr(0x2E0));
    put(0x258, &3u16.to_le_bytes());
    put(0x2E0, &[0, 0, 0, 0, 0, 0, 1, 1, 7, 0, 1, 1]);
    put(0x260 + 0x10, &1u32.to_le_bytes());
    put(0x260 + 0x18, &ptr(0x3C0));
    put(0x260 + 0x50, &ptr(0x200));
    put(0x260 + 0x5C, &0.5f32.to_le_bytes());
    put(0x260 + 0x60, &2.0f32.to_le_bytes());
    put(0x3C0, b"pack:/a.clip\0");
    put(0x300 + 0x10, &2u32.to_le_bytes());
    put(0x300 + 0x50, &ptr(0x380));
    put(0x300 + 0x58, &[1, 0, 1, 0]);
    put(0x380, &0.25f32.to_le_bytes());
    put(0x384, &1.0f32.to_le_bytes());
    put(0x388, &1.0f32.to_le_bytes());
    put(0x390, &ptr(0x200));
    rsc(46, 1 << 26, &system)
}

/// A composite of a box moved 10 along X and a one-triangle geometry: 13 triangles.
fn collision() -> Vec<u8> {
    let mut system = vec![0u8; 0x400];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    put(0x10, &[10]);
    put(0x70, &(SYS + 0x220).to_le_bytes());
    put(0x78, &(SYS + 0x240).to_le_bytes());
    put(0xA0, &2u16.to_le_bytes());
    put(0x220, &(SYS + 0xC0).to_le_bytes());
    put(0x228, &(SYS + 0x140).to_le_bytes());
    put(0x240, &floats(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 0.0, 0.0, 1.0]));
    put(0x280, &floats(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
    put(0xC0 + 0x10, &[3]);
    put(0xC0 + 0x20, &floats(&[1.0, 2.0, 3.0]));
    put(0xC0 + 0x30, &floats(&[-1.0, -2.0, -3.0]));
    put(0x140 + 0x10, &[4]);
    put(0x140 + 0x88, &(SYS + 0x2E0).to_le_bytes());
    put(0x140 + 0x90, &floats(&[1.0, 1.0, 1.0]));
    put(0x140 + 0xB0, &(SYS + 0x2C0).to_le_bytes());
    put(0x140 + 0xD0, &3u32.to_le_bytes());
    put(0x140 + 0xD4, &1u32.to_le_bytes());
    let shorts: Vec<u8> = [0i16, 0, 0, 4, 0, 0, 0, 4, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
    put(0x2C0, &shorts);
    put(0x2E4, &[0, 0, 1, 0, 2, 0]);
    rsc(43, (1 << 27) | 1, &system)
}

/// A quad and a triangle sharing an edge: 5 vertices, 3 triangles.
fn navmesh() -> Vec<u8> {
    let mut system = vec![0u8; 0x1000];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    put(0x70, &(SYS + 0x200).to_le_bytes());
    put(0x80, &(SYS + 0x280).to_le_bytes());
    put(0x118, &(SYS + 0x300).to_le_bytes());
    put(0x120, &(SYS + 0x380).to_le_bytes());
    put(0x380, &floats(&[100.0, 200.0, 10.0, 0.0, 250.0, 350.0, 30.0]));
    // Lists of one part each: vertices at 0x400, indices at 0x480, polygons at 0x500.
    for (at, total, data) in [(0x200, 5u32, 0x400u64), (0x280, 7, 0x480), (0x300, 2, 0x500)] {
        put(at + 8, &total.to_le_bytes());
        put(at + 0x10, &(SYS + at as u64 + 0x40).to_le_bytes());
        put(at + 0x20, &1u32.to_le_bytes());
        put(at + 0x40, &(SYS + data).to_le_bytes());
        put(at + 0x48, &total.to_le_bytes());
    }
    let max = u16::MAX;
    for (k, v) in [[0, 0, 0], [max, 0, 0], [max, max, max], [0, max, max], [max, max / 2, max]].iter().enumerate() {
        for (i, c) in v.iter().enumerate() {
            put(0x400 + k * 6 + i * 2, &c.to_le_bytes());
        }
    }
    for (k, i) in [0u16, 1, 2, 3, 1, 4, 2].into_iter().enumerate() {
        put(0x480 + k * 2, &i.to_le_bytes());
    }
    put(0x502, &(4u16 << 5).to_le_bytes());
    put(0x530 + 2, &(3u16 << 5).to_le_bytes());
    put(0x530 + 4, &4u16.to_le_bytes());
    rsc(2, 1 << 24, &system)
}

#[test]
fn clip_dictionary_is_a_read_only_summary() {
    let (_dir, root) = game_with(&[("moves.ycd", clip_dictionary())]);
    let mut harness = open(&root, "moves.ycd");
    let text = harness.state().preview_text().unwrap().to_string();
    assert!(text.starts_with("2 clips, 1 animation\n"), "{text}");
    assert!(text.contains("clip 0x0000AAAA (pack:/a.clip): animation clip"), "{text}");
    assert!(harness.state().preview_info().is_some_and(|i| i.contains("read-only")), "{:?}", harness.state().preview_info());

    // No Save button, and the text cannot be typed into.
    assert!(harness.query_by_label_contains("💾").is_none());
    {
        let editor = harness.get_by_role(Role::MultilineTextInput);
        assert!(editor.accesskit_node().is_read_only());
        editor.click();
    }
    harness.run_steps(2);
    harness.event(egui::Event::Text("typed".into()));
    harness.run_steps(2);
    assert_eq!(harness.state().preview_text(), Some(text.as_str()));

    // Even changed behind the editor's back, a summary is never saved.
    let ctx = harness.ctx.clone();
    harness.state_mut().edit_preview("something else");
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    assert!(harness.state().status_error().is_some_and(|e| e.contains("not saved")), "{:?}", harness.state().status_error());
    assert!(!root.join("mods/update/update.rpf").exists());
}

#[test]
fn collision_and_navmeshes_show_in_the_3d_view() {
    for (file, data, summary) in [("probe.ybn", collision(), "39 vertices · 13 triangles"), ("probe.ynv", navmesh(), "5 vertices · 3 triangles")] {
        let (_dir, root) = game_with(&[(file, data)]);
        let harness = open(&root, file);
        assert!(!harness.state().status_error().is_some_and(|e| !e.starts_with("keys:")), "{file}: {:?}", harness.state().status_error());
        harness.get_by_label_contains(summary);
        assert!(harness.state().preview_info().is_some_and(|i| i.contains("triangles")), "{file}: {:?}", harness.state().preview_info());
        // Collision exports as CodeWalker XML; navmeshes do not.
        assert_eq!(harness.query_by_label_contains("Export as XML").is_some(), file.ends_with(".ybn"), "{file}");
    }
}

/// A DDS cube map of 4x4 A8R8G8B8 faces, one level, face `i` filled with the grey `10 * (i + 1)`.
fn cube_dds() -> Vec<u8> {
    let mut dds = vec![0u8; 128];
    let mut put = |at: usize, v: u32| dds[at..at + 4].copy_from_slice(&v.to_le_bytes());
    for (at, v) in [(0, u32::from_le_bytes(*b"DDS ")), (4, 124), (8, 0x1007), (12, 4), (16, 4), (76, 32), (80, 0x41), (88, 32), (92, 0xFF_0000), (96, 0xFF00), (100, 0xFF), (104, 0xFF00_0000), (108, 0x1008), (112, 0x200 | 0xFC00)] {
        put(at, v);
    }
    for face in 0..6u8 {
        dds.extend(std::iter::repeat_n([10 * (face + 1); 4], 16).flatten());
    }
    dds
}

/// The face the Face menu shows, if there is one.
fn face(harness: &Harness<'_, App>) -> Option<String> {
    harness.query_all_by_role(Role::ComboBox).find_map(|n| n.accesskit_node().value().filter(|v| v.starts_with("Face")))
}

#[test]
fn cube_maps_have_a_face_menu() {
    // red_probe becomes a cube map; blue_probe stays flat.
    let ytd = paleto_formats::replace_textures(include_bytes!("data/probe.ytd"), &[("red_probe", &cube_dds())]).unwrap();
    let (_dir, root) = game_with(&[("sky.ytd", ytd)]);
    let mut harness = open(&root, "sky.ytd");
    assert_eq!(harness.state().preview_textures(), Some((vec!["red_probe".to_string(), "blue_probe".to_string()], 0)));
    assert_eq!(face(&harness).as_deref(), Some("Face +X"));

    harness.query(By::new().role(Role::ComboBox).value("Face +X")).unwrap().click();
    harness.run_steps(3);
    harness.get_by_label("Face −Y").click();
    harness.run_steps(3);
    assert_eq!(face(&harness).as_deref(), Some("Face −Y"));
    assert!(!harness.state().status_error().is_some_and(|e| !e.starts_with("keys:")), "{:?}", harness.state().status_error());

    // A flat texture has no Face menu; back on the cube map, it starts at the first face.
    harness.get_by_label_contains("blue_probe").click();
    harness.run_steps(3);
    assert_eq!(face(&harness), None);
    harness.get_by_label_contains("red_probe").click();
    harness.run_steps(3);
    assert_eq!(face(&harness).as_deref(), Some("Face +X"));
}
