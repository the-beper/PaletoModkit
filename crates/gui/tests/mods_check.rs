//! The "Mods in game" window: setup checks, the last session's log and the DLC list.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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

fn update_rpf(path: &std::path::Path, packs: &[&str]) {
    let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
    let xml = format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n");
    let mut editor = ArchiveEditor::new("update.rpf");
    editor.add_file("common/data/dlclist.xml", xml.into_bytes()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    editor.save(path).unwrap();
}

#[test]
fn mods_in_game() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    // The game was updated (mp2026_01) after its update.rpf was copied to the mods folder.
    update_rpf(&root.join("update/update.rpf"), &["mpheist", "mp2026_01"]);
    update_rpf(&root.join("mods/update/update.rpf"), &["mpheist"]);
    std::fs::write(root.join("mods/x64e.rpf"), b"").unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    harness.run_steps(2);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.starts_with("no ASI loader or OpenIV.asi"), "{summary}");
    assert!(summary.contains("missing DLC packs: mp2026_01"), "{summary}");
    // No log yet: nothing is known about the archives.
    assert!(summary.contains("update/update.rpf: None"), "{summary}");
    harness.get_by_label("Mods in game");

    // Set up and played once: the log tells which archives the game read.
    std::fs::write(root.join("OpenIV.asi"), b"").unwrap();
    std::fs::write(root.join("dinput8.dll"), b"MZ ASI LOADER %s\\*.asi").unwrap();
    std::fs::write(root.join("OpenIV.log"), "TRACE: [fiDeviceLocal::openBulk] 'mods/update/update.rpf' {M}\n").unwrap();
    harness.get_by_label("Add them to the mods copy's DLC list").click();
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.starts_with("OpenIV.asi is ready"), "{summary}");
    assert!(!summary.contains("missing DLC packs"), "{summary}");
    assert!(summary.contains("update/update.rpf: Some(true)") && summary.contains("x64e.rpf: Some(false)"), "{summary}");

    // The original update.rpf is untouched.
    let original = paleto_core::ArchivePath::new(root.join("update/update.rpf")).join("common").join("data").join("dlclist.xml");
    assert!(String::from_utf8(original.read(None).unwrap()).unwrap().contains("mp2026_01"));
    let modded = paleto_core::ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data").join("dlclist.xml");
    let modded = String::from_utf8(modded.read(None).unwrap()).unwrap();
    assert!(modded.contains("\t\t<Item>dlcpacks:/mpheist/</Item>\n\t\t<Item>dlcpacks:/mp2026_01/</Item>\n\t</Paths>"), "{modded}");
}

/// A resource file of this version (the payload is never read).
fn resource(version: u32) -> Vec<u8> {
    let mut data = b"RSC7".to_vec();
    for word in [version, (version >> 4) << 28, (version & 15) << 28] {
        data.extend(word.to_le_bytes());
    }
    data.extend([0u8; 32]);
    data
}

#[test]
fn legacy_formats_in_mods() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut original = ArchiveEditor::new("x64e.rpf");
    original.add_file("props/old.ydr", resource(165)).unwrap();
    original.save(&root.join("x64e.rpf")).unwrap();
    let mut copy = ArchiveEditor::new("x64e.rpf");
    copy.add_file("props/old.ydr", resource(165)).unwrap();
    copy.add_file("props/fine.ydr", resource(159)).unwrap();
    copy.save(&root.join("mods/x64e.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    // Rockstar's own Legacy leftover is not reported.
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("all files are in the Enhanced format"), "{summary}");
    harness.get_by_label("Every model, texture and particle effect is in the Enhanced format");

    // A model made for Legacy is installed.
    let mut copy = ArchiveEditor::open(&root.join("mods/x64e.rpf"), None).unwrap();
    copy.add_file("props/new.ydr", resource(165)).unwrap();
    copy.save(&root.join("mods/x64e.rpf")).unwrap();
    harness.get_by_label("Refresh").click();
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("not Enhanced: x64e.rpf/props/new.ydr: resource version 165 (Enhanced uses 159)"), "{summary}");
    assert!(!summary.contains("old.ydr") && !summary.contains("fine.ydr: resource"), "{summary}");
    harness.get_by_label("1 file in the mods folder was made for Legacy: the Enhanced game may crash on it or ignore it.");
    harness.get_by_label("x64e.rpf/props/new.ydr");
}

#[test]
fn unlisted_addon_pack() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/piccolo")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
    update_rpf(&root.join("mods/update/update.rpf"), &["mpheist"]);
    ArchiveEditor::new("dlc.rpf").save(&root.join("mods/update/x64/dlcpacks/piccolo/dlc.rpf")).unwrap();
    std::fs::write(root.join("ScriptHookV.dll"), b"").unwrap();
    std::fs::write(root.join("ScriptHookV.log"), "// GTA V SCRIPT HOOK (build Jul 15 2026, v3889.0/1158.13)\n[19:49:14] INIT: Success, game version is VER_EN_1_0_1158_13\n").unwrap();
    std::fs::write(root.join("Trainer.asi"), b"").unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("unlisted DLC packs: piccolo"), "{summary}");
    harness.get_by_label("This pack of the mods folder is not in the DLC list, so the game never loads it: piccolo.");
    harness.get_by_label("Script Hook V v3889.0/1158.13 works with this game version (VER_EN_1_0_1158_13)");
    harness.get_by_label("ASI plugins (1)");
    harness.get_by_label("Add them to the mods copy's DLC list").click();
    settle(&mut harness);

    let summary = harness.state().mods_check_summary().unwrap();
    assert!(!summary.contains("unlisted DLC packs"), "{summary}");
    let modded = paleto_core::ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data").join("dlclist.xml");
    let modded = String::from_utf8(modded.read(None).unwrap()).unwrap();
    assert!(modded.contains("\t\t<Item>dlcpacks:/mpheist/</Item>\n\t\t<Item>dlcpacks:/piccolo/</Item>\n\t</Paths>"), "{modded}");
}

#[test]
fn dropped_addon_pack() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
    let download = dir.path().join("Super Car 1.2");
    std::fs::create_dir_all(download.join("dlcpacks/supercar")).unwrap();
    ArchiveEditor::new("dlc.rpf").save(&download.join("dlcpacks/supercar/dlc.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(download.clone())));
    harness.run_steps(3);
    harness.get_by_label("Install add-on pack");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("addon-pack.png")).unwrap();
    }
    harness.get_by_label("Install").click();
    settle(&mut harness);

    assert!(root.join("mods/update/x64/dlcpacks/supercar/dlc.rpf").is_file());
    let modded = paleto_core::ArchivePath::new(root.join("mods/update/update.rpf")).join("common").join("data").join("dlclist.xml");
    let modded = String::from_utf8(modded.read(None).unwrap()).unwrap();
    assert!(modded.contains("<Item>dlcpacks:/mpheist/</Item>\n\t\t<Item>dlcpacks:/supercar/</Item>"), "{modded}");
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn deletes_unchanged_copies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/mpbeach")).unwrap();
    std::fs::create_dir_all(root.join("update/x64/dlcpacks/mpbeach")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    std::fs::write(root.join("update/x64/dlcpacks/mpbeach/dlc.rpf"), b"beach").unwrap();
    std::fs::write(root.join("mods/update/x64/dlcpacks/mpbeach/dlc.rpf"), b"beach").unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    harness.get_by_label("Look for them").click();
    settle(&mut harness);
    harness.get_by_label("1 file (0.0 GB) is identical to the game's own: the game reads its own without them.");
    harness.get_by_label("Delete them").click();
    settle(&mut harness);
    assert!(!root.join("mods/update/x64/dlcpacks/mpbeach").exists());
    assert!(root.join("update/x64/dlcpacks/mpbeach/dlc.rpf").exists());
}
