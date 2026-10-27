//! The model viewer: exporting a model as CodeWalker XML.

use std::time::Duration;

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

#[test]
fn export_model_as_xml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("models/prop_probe.ydr", empty_drawable()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("models"));
    settle(&mut harness);
    harness.state_mut().preview("prop_probe.ydr", &ctx);
    settle(&mut harness);
    harness.get_by_label_contains("Export as XML");

    let out = dir.path().join("out");
    harness.state_mut().export_model(&out, &ctx);
    settle(&mut harness);
    let xml = std::fs::read_to_string(out.join("prop_probe.ydr.xml")).unwrap();
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Drawable>\n"), "{xml}");
    assert!(xml.contains("<Name>prop_probe</Name>"), "{xml}");
}
