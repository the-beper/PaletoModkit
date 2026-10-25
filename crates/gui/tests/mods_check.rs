//! The "Mods in game" window: the banner, the inventory of the mods and the maintenance tools.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use egui::accesskit::Role;
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

/// Whether the window lists a mod of this name (the file list behind it shows names too).
fn shown(harness: &Harness<'_, App>, name: &str) -> bool {
    harness.query_all_by_role_and_label(Role::CheckBox, name).next().is_some()
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
    assert!(summary.starts_with("banner: No ASI loader or OpenIV.asi"), "{summary}");
    assert!(summary.contains("missing DLC packs: mp2026_01"), "{summary}");
    // No log yet: nothing is known about the archives.
    assert!(summary.lines().any(|l| l == "update/update.rpf: archive"), "{summary}");
    harness.get_by_label("Mods in game");

    // Set up and played once: the log tells which archives the game read.
    std::fs::write(root.join("OpenIV.asi"), b"").unwrap();
    std::fs::write(root.join("dinput8.dll"), b"MZ ASI LOADER %s\\*.asi").unwrap();
    std::fs::write(root.join("OpenIV.log"), "TRACE: [fiDeviceLocal::openBulk] 'mods/update/update.rpf' {M}\n").unwrap();
    harness.get_by_label("Refresh").click();
    settle(&mut harness);
    // The setup is complete: the DLC list is the first problem, with its fix.
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.starts_with("banner: The mods copy of update.rpf predates the last game update"), "{summary}");
    harness.get_by_label("Add to DLC list").click();
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.starts_with("banner: The game will load your mods"), "{summary}");
    harness.get_by_label("✔ The game will load your mods");
    assert!(!summary.contains("missing DLC packs"), "{summary}");
    assert!(summary.contains("update/update.rpf: archive, Read in the last session"), "{summary}");
    assert!(summary.contains("x64e.rpf: archive, Not read by the game"), "{summary}");

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
    harness.get_by_label("Maintenance").click();
    harness.run_steps(10);
    harness.get_by_label("Every model, texture and particle effect is in the Enhanced format");

    // A model made for Legacy is installed.
    let mut copy = ArchiveEditor::open(&root.join("mods/x64e.rpf"), None).unwrap();
    copy.add_file("props/new.ydr", resource(165)).unwrap();
    copy.save(&root.join("mods/x64e.rpf")).unwrap();
    harness.get_by_label("Refresh").click();
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("x64e.rpf: archive, Not Enhanced: resource version 165 (Enhanced uses 159)"), "{summary}");
    assert!(summary.contains("problem: 1 file in the mods folder was made for Legacy: the Enhanced game may crash on it or ignore it."), "{summary}");
    assert!(!summary.contains("old.ydr") && !summary.contains("fine.ydr"), "{summary}");
    harness.get_by_label("1 file in the mods folder was made for Legacy: the Enhanced game may crash on it or ignore it.");
    harness.get_by_label("Legacy files").click();
    harness.run_steps(10);
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
    assert!(summary.lines().any(|l| l == "piccolo: add-on pack, Not in the DLC list"), "{summary}");
    assert!(summary.lines().any(|l| l.starts_with("Trainer.asi: plugin")), "{summary}");
    // The window's sections open with an animation: click once it has stopped moving.
    harness.run_steps(15);
    harness.get_by_label("This pack of the mods folder is not in the DLC list, so the game never loads it: piccolo.");
    harness.get_by_label("Script Hook V v3889.0/1158.13 works with this game version (VER_EN_1_0_1158_13)");
    // The banner's fix and the pack's own button do the same.
    harness.get_all_by_label("Add to DLC list").last().unwrap().click();
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
    harness.get_by_label("Maintenance").click();
    harness.run_steps(10);
    harness.get_by_label("Look for them").click();
    settle(&mut harness);
    harness.get_by_label("1 file (0.0 GB) is identical to the game's own: the game reads its own without them.");
    harness.get_by_label("Delete them").click();
    settle(&mut harness);
    assert!(!root.join("mods/update/x64/dlcpacks/mpbeach").exists());
    assert!(root.join("update/x64/dlcpacks/mpbeach/dlc.rpf").exists());
}

/// A fake Enhanced game that the game will load: the loader, the plugin, a script, a pack and an archive.
fn loadable_game(dir: &std::path::Path) -> std::path::PathBuf {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/piccolo")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    std::fs::write(root.join("OpenRPF.asi"), b"").unwrap();
    std::fs::write(root.join("dinput8.dll"), b"MZ ASI LOADER %s\\*.asi").unwrap();
    std::fs::write(root.join("Trainer.asi"), b"").unwrap();
    std::fs::write(root.join("scripts/Mod.dll"), b"").unwrap();
    update_rpf(&root.join("update/update.rpf"), &["mpheist", "piccolo"]);
    update_rpf(&root.join("mods/update/update.rpf"), &["mpheist", "piccolo"]);
    ArchiveEditor::new("dlc.rpf").save(&root.join("mods/update/x64/dlcpacks/piccolo/dlc.rpf")).unwrap();
    ArchiveEditor::new("x64e.rpf").save(&root.join("mods/x64e.rpf")).unwrap();
    root
}

#[test]
fn inventory_filters_and_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let root = loadable_game(dir.path());

    let mut harness = Harness::builder().with_size([1000.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.starts_with("banner: The game will load your mods"), "{summary}");
    harness.get_by_label("✔ The game will load your mods");
    for (name, kind) in [("Trainer.asi", "plugin"), ("Mod.dll", "script"), ("piccolo", "add-on pack"), ("x64e.rpf", "archive")] {
        assert!(summary.lines().any(|l| l.starts_with(&format!("{name}: {kind}"))), "{summary}");
    }

    // The chips narrow the list to one kind.
    harness.get_by_label("Plugins").click();
    harness.run_steps(2);
    assert!(shown(&harness, "Trainer.asi") && !shown(&harness, "Mod.dll") && !shown(&harness, "piccolo"));
    harness.get_by_label("Scripts").click();
    harness.run_steps(2);
    harness.run_steps(5);
    assert!(!shown(&harness, "Trainer.asi") && shown(&harness, "Mod.dll"));
    harness.get_by_label("Add-on packs").click();
    harness.run_steps(2);
    assert!(shown(&harness, "piccolo") && !shown(&harness, "x64e.rpf"));
    harness.get_by_label("Archives").click();
    harness.run_steps(2);
    assert!(shown(&harness, "x64e.rpf") && !shown(&harness, "piccolo"));

    // Turning a plugin off moves it out of the game folder, and back.
    harness.get_by_label("Plugins").click();
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::CheckBox, "Trainer.asi").click();
    settle(&mut harness);
    let game = paleto_core::Game::open(&root).unwrap();
    assert!(!root.join("Trainer.asi").exists());
    assert!(paleto_core::crash_hunt::disabled_dir(&game).join("Trainer.asi").is_file());
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("Trainer.asi: plugin, Turned off"), "{summary}");
    // The chosen chip survived the refresh.
    assert!(shown(&harness, "Trainer.asi") && !shown(&harness, "Mod.dll"));
    harness.get_by_role_and_label(Role::CheckBox, "Trainer.asi").click();
    settle(&mut harness);
    assert!(root.join("Trainer.asi").is_file());
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(!summary.contains("Turned off"), "{summary}");
}

#[test]
fn empty_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    harness.get_by_label("No mods found in the game folder");
}

#[test]
fn toggle_is_refused_during_a_hunt() {
    let dir = tempfile::tempdir().unwrap();
    let root = loadable_game(dir.path());
    let mut harness = Harness::builder().with_size([1000.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    let game = paleto_core::Game::open(&root).unwrap();
    paleto_core::crash_hunt::start(&game).unwrap();
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    assert!(harness.get_by_role_and_label(Role::CheckBox, "Trainer.asi").accesskit_node().is_disabled());
}
