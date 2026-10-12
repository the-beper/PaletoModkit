//! Renders the main screens against a real game install and saves them as PNG
//! in $PALETO_SCREENS (skipped when unset).
//!
//! PALETO_SCREENS=/tmp/shots PALETO_PACKAGE=mod.oiv cargo test -p paleto-modkit --test screens -- --nocapture

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::{ArchivePath, game};
use paleto_modkit::App;
use paleto_modkit::app::Info;
use paleto_modkit::settings::{Density, Settings, TextSize};

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

fn save(harness: &mut Harness<'_, App>, dir: &Path, name: &str) {
    // No pointer, and nothing highlighted under it.
    harness.event(egui::Event::PointerGone);
    harness.run_steps(2);
    let image = harness.render().expect("render");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).unwrap();
    println!("saved {}", path.display());
}

fn mock_game(temp_dir: &Path) -> (paleto_core::Game, PathBuf) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use paleto_rpf::ArchiveEditor;

    let root = temp_dir.join("Grand Theft Auto V");
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("x64")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();

    let dlclist = r#"<?xml version="1.0" encoding="UTF-8"?>
<SMandatoryPacksData>
	<Paths>
		<Item>platform:\dlcPacks\mpbeach\</Item>
		<Item>platform:\dlcPacks\mpheist\</Item>
		<Item>platform:\dlcPacks\patchday28ng\</Item>
		<Item>platform:\dlcPacks\spcustomcar\</Item>
	</Paths>
</SMandatoryPacksData>
"#;

    let handling = r#"<?xml version="1.0" encoding="UTF-8"?>
<CHandlingDataMgr>
	<HandlingData>
		<Item type="CHandlingData">
			<handlingName>ADDER</handlingName>
			<fMass value="1800.000000"/>
			<fInitialDragCoeff value="8.000000"/>
			<fPercentSubmerged value="85.000000"/>
			<vecCentreOfMassOffset x="0.000000" y="0.000000" z="0.000000"/>
			<vecInertiaMultiplier x="1.000000" y="1.200000" z="1.400000"/>
			<fDriveBiasFront value="0.200000"/>
			<nInitialDriveGears value="6"/>
			<fInitialDriveForce value="0.360000"/>
			<fDriveInertia value="1.000000"/>
			<fClutchChangeRateScaleUpShift value="2.300000"/>
			<fClutchChangeRateScaleDownShift value="2.300000"/>
			<fInitialDriveMaxFlatVel value="160.000000"/>
			<fBrakeForce value="1.100000"/>
			<fBrakeBiasFront value="0.550000"/>
			<fHandBrakeForce value="0.800000"/>
			<fSteeringLock value="40.000000"/>
		</Item>
	</HandlingData>
</CHandlingDataMgr>
"#;

    let mut update_ed = ArchiveEditor::new("update.rpf");
    update_ed.add_file("common/data/dlclist.xml", dlclist.as_bytes().to_vec()).unwrap();
    update_ed.add_file("common/data/handling.meta", handling.as_bytes().to_vec()).unwrap();
    update_ed.save(&root.join("update/update.rpf")).unwrap();

    let mut mods_update_ed = ArchiveEditor::new("update.rpf");
    mods_update_ed.add_file("common/data/dlclist.xml", dlclist.as_bytes().to_vec()).unwrap();
    mods_update_ed.add_file("common/data/handling.meta", handling.as_bytes().to_vec()).unwrap();
    mods_update_ed.add_file("common/data/extratitleupdatedata.meta", b"<extratitleupdatedata/>".to_vec()).unwrap();
    mods_update_ed.save(&root.join("mods/update/update.rpf")).unwrap();

    let probe = include_bytes!("data/probe.ytd");
    let mut veh_ed = ArchiveEditor::new("vehicles.rpf");
    veh_ed.add_file("adder.ytd", probe.to_vec()).unwrap();
    veh_ed.add_file("adder.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("adder_hi.yft", vec![0u8; 256]).unwrap();
    veh_ed.add_file("banshee.ytd", probe.to_vec()).unwrap();
    veh_ed.add_file("banshee.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("cheetah.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("entityxf.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("infernus.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("turismor.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("vacca.yft", vec![0u8; 128]).unwrap();
    veh_ed.add_file("zentorno.yft", vec![0u8; 128]).unwrap();

    let temp_veh = temp_dir.join("vehicles.rpf");
    veh_ed.save(&temp_veh).unwrap();
    let veh_bytes = std::fs::read(&temp_veh).unwrap();

    let mut x64e_ed = ArchiveEditor::new("x64e.rpf");
    x64e_ed.archive("levels/gta5/vehicles.rpf", true, None).unwrap();
    x64e_ed.add_file("levels/gta5/vehicles.rpf", veh_bytes).unwrap();
    x64e_ed.save(&root.join("x64e.rpf")).unwrap();

    let package_path = temp_dir.join("sample-mod.oiv");
    {
        let file = std::fs::File::create(&package_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let icon_path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/icons/hicolor/128x128/apps/io.github.the_beper.PaletoModkit.png"));
        if let Ok(icon_bytes) = std::fs::read(icon_path) {
            zip.start_file("icon.png", SimpleFileOptions::default()).unwrap();
            zip.write_all(&icon_bytes).unwrap();
        }

        let assembly = r#"<?xml version="1.0" encoding="UTF-8"?>
<package version="2.2" id="{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}" target="Five">
  <metadata>
    <name>Super Realistic Handling &amp; DLC Pack</name>
    <version><major>2</major><minor>4</minor><tag>Release</tag></version>
    <author><displayName>Paleto Team</displayName></author>
    <description><![CDATA[Complete overhaul of vehicle physics, handling parameters, and new DLC packs. Automatically installs into the safe mods folder without altering original files.]]></description>
  </metadata>
  <colors>
    <headerBackground>$FF2E3440</headerBackground>
  </colors>
  <content>
    <archive path="update/update.rpf" createIfNotExist="False">
      <xml path="common/data/handling.meta">
        <replace xpath="/CHandlingDataMgr/HandlingData/Item[1]/fInitialDriveForce">
          <fInitialDriveForce value="0.450000"/>
        </replace>
      </xml>
      <xml path="common/data/dlclist.xml">
        <add xpath="/SMandatoryPacksData/Paths">
          <Item>platform:\dlcPacks\newdlc\</Item>
        </add>
      </xml>
    </archive>
  </content>
</package>
"#;
        zip.start_file("assembly.xml", SimpleFileOptions::default()).unwrap();
        zip.write_all(assembly.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    let game = paleto_core::Game::open(&root).unwrap();
    (game, package_path)
}

#[test]
fn screens() {
    let Some(dir) = std::env::var_os("PALETO_SCREENS").map(PathBuf::from) else { return };
    let temp_dir = tempfile::tempdir().unwrap();
    let (game, _pkg) = if let Some(g) = game::detect_installs().into_iter().next() {
        (g, PathBuf::new())
    } else {
        mock_game(temp_dir.path())
    };
    std::fs::create_dir_all(&dir).unwrap();

    let mut harness = Harness::builder().with_size([1280.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&game.root, &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "1-game-folder");

    let data = ArchivePath::new(game.root.join("update/update.rpf")).join("common").join("data");
    harness.state_mut().open(data.clone());
    settle(&mut harness);
    save(&mut harness, &dir, "2-archive");

    harness.state_mut().preview("dlclist.xml", &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "3-preview");

    harness.state_mut().open(ArchivePath::new(game.root.join("x64e.rpf")).join("levels").join("gta5").join("vehicles.rpf"));
    harness.event(egui::Event::Text("adder".into()));
    settle(&mut harness);
    save(&mut harness, &dir, "4-type-to-search");

    harness.state_mut().preview("adder.ytd", &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "4b-textures");

    let target_name = if harness.state().preview_textures().as_ref().is_some_and(|(t, _)| t.iter().any(|s| s.contains("venem_lights"))) {
        "venem_lights"
    } else {
        "blue_probe"
    };
    harness.get_by_label_contains(target_name).click();
    harness.run_steps(3);
    save(&mut harness, &dir, "4c-textures-alpha");

    let mut s = Settings { text_size: TextSize::Small, density: Density::Compact, ..Default::default() };
    harness.state_mut().open(data.clone());
    harness.state_mut().set_settings(s.clone(), &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "5-small-compact");

    s = Settings { text_size: TextSize::Large, density: Density::Comfortable, colored_icons: false, ..Default::default() };
    harness.state_mut().set_settings(s, &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "6-large-comfortable-mono");
}

#[test]
fn dialogs() {
    let Some(dir) = std::env::var_os("PALETO_SCREENS").map(PathBuf::from) else { return };
    let temp_dir = tempfile::tempdir().unwrap();
    let (game, default_package) = if let Some(g) = game::detect_installs().into_iter().next() {
        (g, PathBuf::new())
    } else {
        mock_game(temp_dir.path())
    };
    let package = std::env::var_os("PALETO_PACKAGE").map(PathBuf::from).unwrap_or(default_package);
    std::fs::create_dir_all(&dir).unwrap();

    let mut harness = Harness::builder().with_size([1280.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&game.root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(game.root.join("x64e.rpf")).join("levels").join("gta5").join("vehicles.rpf"));
    settle(&mut harness);
    harness.state_mut().ask_delete(&["adder.yft", "adder.ytd"]);
    harness.run_steps(3);
    save(&mut harness, &dir, "7-delete");
    harness.state_mut().cancel_dialogs();

    // Unsaved-changes prompt (discarded, nothing is written).
    harness.state_mut().open(ArchivePath::new(game.root.join("update/update.rpf")).join("common").join("data"));
    harness.state_mut().preview("handling.meta", &ctx);
    harness.run_steps(2);
    harness.state_mut().edit_preview("<CHandlingDataMgr>\n  <!-- edited -->\n</CHandlingDataMgr>\n");
    harness.run_steps(2);
    if let Some(target) = harness.get_all_by_label("x64").next().or_else(|| harness.get_all_by_label("mods").next()) {
        target.click();
    }
    harness.run_steps(3);
    save(&mut harness, &dir, "9-unsaved");
    if let Some(discard) = harness.get_all_by_label("Discard").next() {
        discard.click();
    }
    harness.run_steps(3);

    if let Some(expand) = harness.get_all_by_label("⏷ Expand all").next() {
        expand.click();
    }
    harness.run_steps(10);

    if let Some(help) = harness.get_all_by_label("Help").next() {
        help.click();
    }
    harness.run_steps(2);
    if let Some(about) = harness.get_all_by_label("About").next() {
        about.click();
    }
    harness.run_steps(3);
    save(&mut harness, &dir, "10-about");
    harness.key_press(egui::Key::Escape);

    if let Some(help) = harness.get_all_by_label("Help").next() {
        help.click();
    }
    harness.run_steps(2);

    if let Some(gs) = harness.get_all_by_label("Getting started").next() {
        gs.click();
    }
    harness.run_steps(3);
    save(&mut harness, &dir, "11-help");
    harness.key_press(egui::Key::Escape);

    if let Some(collapse) = harness.get_all_by_label("⏶ Collapse all").next() {
        collapse.click();
    }
    harness.run_steps(3);

    if package.is_file() {
        harness.state_mut().open_package(&package, &ctx);
        harness.run_steps(3);
        save(&mut harness, &dir, "8-install");
    }
}

/// The README and AppStream screenshots, from a clean copy of a real install
/// (Enhanced when there is one, else Legacy): only the original game files,
/// linked into a fresh folder (no mods folder, no tools), with caches of its
/// own. Saved in $PALETO_SCREENS under their final names:
///
/// PALETO_SCREENS=$PWD/screenshots cargo test --release -p paleto-modkit --test screens readme -- --nocapture
#[test]
fn readme() {
    let Some(dir) = std::env::var_os("PALETO_SCREENS").map(PathBuf::from) else { return };
    let installs = game::detect_installs();
    let Some(real) = installs.iter().find(|g| g.edition == paleto_core::game::Edition::Enhanced).or(installs.first()) else { return };
    let temp_dir = tempfile::tempdir().unwrap();
    let legacy = real.edition == paleto_core::game::Edition::Legacy;
    let root = temp_dir.path().join(if legacy { "Grand Theft Auto V" } else { "Grand Theft Auto V Enhanced" });
    std::fs::create_dir_all(&root).unwrap();
    for entry in std::fs::read_dir(&real.root).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The game files, and the ASI loader and plugin that load the mods folder.
        let tools = ["GTA5.exe", "dinput8.dll", "OpenIV.asi", "GTA5_Enhanced.exe", "xinput1_4.dll", "OpenRPF.asi"];
        let original = name == "update" || name == "x64" || tools.contains(&name.as_str()) || (name.ends_with(".rpf") && (name.starts_with("x64") || name == "common.rpf"));
        if original {
            std::os::unix::fs::symlink(entry.path(), root.join(&name)).unwrap();
        }
    }
    // Caches of its own, with the keys already prepared.
    let data = temp_dir.path().join("data");
    std::fs::create_dir_all(data.join("paleto")).unwrap();
    for file in ["keys.bin", "magic.dat"] {
        let _ = std::fs::copy(game::data_dir().join(file), data.join("paleto").join(file));
    }
    // SAFETY: set before the app starts its threads; run this test alone.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    let (_, package) = mock_game(&temp_dir.path().join("mock"));
    std::fs::create_dir_all(&dir).unwrap();

    let mut harness = Harness::builder().with_size([1280.0, 760.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "game-folder");

    let vehicles = ArchivePath::new(root.join("x64e.rpf")).join("levels").join("gta5").join("vehicles.rpf");
    harness.state_mut().open(vehicles.clone());
    settle(&mut harness);
    harness.event(egui::Event::Text("adder".into()));
    settle(&mut harness);
    save(&mut harness, &dir, "4-type-to-search");

    harness.state_mut().preview("adder.yft", &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "archive-browser");

    harness.state_mut().preview("adder.ytd", &ctx);
    settle(&mut harness);
    if let Some(lights) = harness.get_all_by_label_contains("venem_lights").next() {
        lights.click();
    }
    harness.run_steps(3);
    save(&mut harness, &dir, "texture-viewer");

    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("x64").join("data"));
    settle(&mut harness);
    harness.state_mut().preview("carmodcols.ymt", &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "xml-editor");

    harness.state_mut().open(ArchivePath::new(root.join("x64/audio/sfx/RESIDENT.rpf")));
    settle(&mut harness);
    harness.state_mut().preview("doors.awc", &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "audio-preview");

    harness.state_mut().close_preview(&ctx);
    harness.state_mut().open(vehicles);
    settle(&mut harness);
    harness.state_mut().open_package(&package, &ctx);
    harness.run_steps(3);
    save(&mut harness, &dir, "oiv-installer");
}

/// Every menu and window, to review how they read:
///
/// PALETO_SCREENS=/tmp/tour cargo test -p paleto-modkit --test screens tour -- --nocapture
#[test]
fn tour() {
    let Some(dir) = std::env::var_os("PALETO_SCREENS").map(PathBuf::from) else { return };
    let temp_dir = tempfile::tempdir().unwrap();
    let game = game::detect_installs().into_iter().next().unwrap_or_else(|| mock_game(temp_dir.path()).0);
    std::fs::create_dir_all(&dir).unwrap();

    let mut harness = Harness::builder().with_size([1280.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&game.root, &ctx);
    settle(&mut harness);
    save(&mut harness, &dir, "main");
    harness.state_mut().open(ArchivePath::new(game.root.join("common.rpf")));
    settle(&mut harness);
    save(&mut harness, &dir, "archive");
    for (name, size) in [("archive-small", TextSize::Small), ("archive-large", TextSize::Large)] {
        let s = Settings { text_size: size, ..Default::default() };
        harness.state_mut().set_settings(s, &ctx);
        settle(&mut harness);
        save(&mut harness, &dir, name);
    }
    harness.state_mut().set_settings(Settings::default(), &ctx);
    // A narrow window with a text open and its search, and without the side tree.
    let mut narrow = Harness::builder().with_size([760.0, 560.0]).build_eframe(|cc| App::new(cc));
    let narrow_ctx = narrow.ctx.clone();
    narrow.state_mut().select_game_folder(&game.root, &narrow_ctx);
    settle(&mut narrow);
    let data = ArchivePath::new(game.root.join("update/update.rpf")).join("common").join("data");
    narrow.state_mut().open(data);
    narrow.state_mut().preview("dlclist.xml", &narrow_ctx);
    settle(&mut narrow);
    narrow.state_mut().search_preview("pack");
    narrow.run_steps(4);
    save(&mut narrow, &dir, "narrow-search");
    let mut tiny = Harness::builder().with_size([560.0, 480.0]).build_eframe(|cc| App::new(cc));
    let tiny_ctx = tiny.ctx.clone();
    tiny.state_mut().select_game_folder(&game.root, &tiny_ctx);
    settle(&mut tiny);
    tiny.state_mut().open(ArchivePath::new(game.root.join("update/update.rpf")).join("common").join("data"));
    tiny.state_mut().preview("dlclist.xml", &tiny_ctx);
    settle(&mut tiny);
    save(&mut tiny, &dir, "tiny");
    drop(tiny);
    let mut wide = Harness::builder().with_size([1400.0, 700.0]).build_eframe(|cc| App::new(cc));
    let wide_ctx = wide.ctx.clone();
    wide.state_mut().select_game_folder(&game.root, &wide_ctx);
    settle(&mut wide);
    wide.state_mut().open(ArchivePath::new(game.root.join("update/update.rpf")).join("common").join("data"));
    wide.state_mut().preview("dlclist.xml", &wide_ctx);
    settle(&mut wide);
    save(&mut wide, &dir, "wide-preview");
    drop(wide);
    let no_tree = Settings { show_tree: false, ..Default::default() };
    narrow.state_mut().set_settings(no_tree, &narrow_ctx);
    settle(&mut narrow);
    save(&mut narrow, &dir, "narrow-no-tree");
    drop(narrow);
    // A fractional display scale, as on many laptops (rounding shows there).
    let mut scaled = Harness::builder().with_size([1280.0, 800.0]).with_pixels_per_point(1.2).build_eframe(|cc| App::new(cc));
    let scaled_ctx = scaled.ctx.clone();
    scaled.state_mut().select_game_folder(&game.root, &scaled_ctx);
    settle(&mut scaled);
    scaled.state_mut().open(ArchivePath::new(game.root.join("common.rpf")));
    settle(&mut scaled);
    save(&mut scaled, &dir, "archive-scaled");
    drop(scaled);
    harness.state_mut().go(paleto_modkit::Location::Folder(game.root.clone()));
    settle(&mut harness);
    for menu in ["File", "Game", "View", "Help"] {
        harness.get_by_label(menu).click();
        harness.run_steps(3);
        save(&mut harness, &dir, &format!("menu-{}", menu.to_lowercase()));
        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
    }
    harness.state_mut().ask_delete(&["mods"]);
    harness.run_steps(3);
    save(&mut harness, &dir, "delete-on-disk");
    harness.state_mut().cancel_dialogs();
    harness.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::N);
    harness.run_steps(3);
    save(&mut harness, &dir, "new-folder");
    harness.state_mut().cancel_dialogs();
    harness.run_steps(2);
    for (name, info) in [("getting-started", Info::GettingStarted), ("shortcuts", Info::Shortcuts), ("about", Info::About)] {
        harness.state_mut().show_info(Some(info), &ctx);
        harness.run_steps(3);
        save(&mut harness, &dir, name);
        harness.state_mut().show_info(None, &ctx);
        harness.run_steps(2);
    }

    type Open = fn(&mut App, &egui::Context);
    let windows: [(&str, Open); 5] = [
        ("mods-check", |a, c| a.check_mods(c)),
        ("snapshots", |a, c| a.open_snapshots(c)),
        ("last-crash", |a, c| a.open_last_crash(c)),
        ("crash-hunt", |a, c| a.open_crash_hunt(c)),
        ("export", |a, c| a.open_export(c)),
    ];
    for (name, open) in windows {
        open(harness.state_mut(), &ctx);
        settle(&mut harness);
        harness.run_steps(5);
        save(&mut harness, &dir, name);
        harness.key_press(egui::Key::Escape);
        harness.run_steps(3);
    }
}

/// The screenshots of docs/, from a clean copy of a real Enhanced install
/// (the game files linked into a Steam library in /tmp/paleto-docs, a small
/// mods folder made here, a game session log and the newest crash dumps)
/// under docs/images:
///
/// PALETO_SCREENS=$PWD/docs/images cargo test --release -p paleto-modkit --test screens docs -- --nocapture
#[test]
fn docs() {
    use egui_kittest::kittest::Queryable;
    let Some(dir) = std::env::var_os("PALETO_SCREENS").map(PathBuf::from) else { return };
    let Some(real) = game::detect_installs().into_iter().find(|g| g.edition == paleto_core::game::Edition::Enhanced) else { return };
    let base = std::env::temp_dir().join("paleto-docs");
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("steamapps/common/Grand Theft Auto V Enhanced");
    let user = base.join("steamapps/compatdata/3240220/pfx/drive_c/users/steamuser");
    std::fs::create_dir_all(&root).unwrap();
    // Caches of its own, with the keys already prepared.
    let data = base.join("data");
    std::fs::create_dir_all(data.join("paleto")).unwrap();
    for file in ["keys.bin", "magic.dat"] {
        let _ = std::fs::copy(game::data_dir().join(file), data.join("paleto").join(file));
    }
    // SAFETY: set before the app starts its threads; run this test alone.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    // The converter from packaging/build-cwconv.sh, when built.
    let converter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cwconv/paleto-cwconv");
    if converter.is_file() {
        unsafe { std::env::set_var("PALETO_CWCONV", &converter) };
    }
    let plugins = ["GameplayFixesV.asi", "InteriorsV.asi", "StreetVendorsV.asi", "GravityGun.asi", "InversePower.asi", "Rain-wash-v.asi"];
    for entry in std::fs::read_dir(&real.root).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The game files, the ASI loader, OpenRPF, Script Hook V and a few plugins.
        let keep = ["update", "x64", "GTA5_Enhanced.exe", "xinput1_4.dll", "OpenRPF.asi", "ScriptHookV.dll", "ScriptHookV.log"].contains(&name.as_str())
            || plugins.contains(&name.as_str())
            || (name.ends_with(".rpf") && (name.starts_with("x64") || name == "common.rpf"));
        if keep {
            std::os::unix::fs::symlink(entry.path(), root.join(&name)).unwrap();
        }
    }
    // The newest crash dumps, where the game writes them.
    let crashes = paleto_core::crash_report::crash_dirs(&real);
    let logs = user.join("AppData/Local/Rockstar Games/GTAV Enhanced/CrashLogs");
    std::fs::create_dir_all(&logs).unwrap();
    let mut dumps: Vec<_> = crashes.iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "dmp")).collect();
    dumps.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    // Oldest first, so the newest is also the last written (the report takes the newest file).
    let recent: Vec<_> = dumps.iter().rev().take(6).rev().collect();
    for dump in recent {
        std::fs::copy(dump, logs.join(dump.file_name().unwrap())).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    // The game's log of the newest crash, which the report reads with it.
    if let Some(log) = dumps.last().map(|d| d.with_file_name("crashcontext.log")).filter(|l| l.is_file()) {
        std::fs::copy(log, logs.join("crashcontext.log")).unwrap();
    }
    // A pack copied by hand (made for Legacy, not in the DLC list), and the
    // same one downloaded, when there is one.
    let legacy_pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/sppiccolo/dlc.rpf");
    let download = base.join("Downloads/piccolo");
    if legacy_pack.is_file() {
        for dest in [root.join("mods/update/x64/dlcpacks/sppiccolo"), download.clone()] {
            std::fs::create_dir_all(&dest).unwrap();
            std::fs::copy(&legacy_pack, dest.join("dlc.rpf")).unwrap();
        }
    }
    // The log of a game session that read the modded pack but not the unlisted one.
    let log = user.join("AppData/Roaming/Los Santos Multiplayer/OpenRPF.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        &log,
        "OpenRPF 0.3\n[info] init success\n\
         [info] redirect 'mods/update/x64/dlcpacks/patch2023_01_g9ec/dlc.rpf' (6)\n\
         [info] successfully loaded modified root RPF 'mods/update/x64/dlcpacks/patch2023_01_g9ec/dlc.rpf'\n",
    )
    .unwrap();
    std::fs::create_dir_all(&dir).unwrap();

    let mut harness = Harness::builder().with_size([1180.0, 860.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);

    // A modded archive: a file added to a small DLC pack.
    let files = base.join("files");
    std::fs::create_dir_all(files.join("common/data")).unwrap();
    std::fs::write(files.join("common/data/handling.meta"), "<CHandlingDataMgr />\n").unwrap();
    harness.state_mut().open(ArchivePath::new(root.join("update/x64/dlcpacks/patch2023_01_g9ec/dlc.rpf")));
    settle(&mut harness);
    harness.state_mut().add_files(vec![files.join("common")], &ctx);
    settle(&mut harness);
    let game = paleto_core::Game::open(&root).unwrap();
    for note in ["Before VisualV", "Working setup"] {
        paleto_core::snapshots::create(&game, note).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
    }
    harness.state_mut().go(paleto_modkit::Location::Folder(root.join("mods")));
    settle(&mut harness);
    harness.get_by_label("⏶ Collapse all").click();
    harness.event(egui::Event::PointerGone);
    harness.run_steps(3);

    let wait = |harness: &mut Harness<'_, App>| {
        for _ in 0..300 {
            harness.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        settle(harness);
    };
    harness.state_mut().ask_addon_packs(std::slice::from_ref(&download), &ctx);
    harness.run_steps(3);
    if harness.query_by_label_contains("Install add-on pack").is_some() {
        save(&mut harness, &dir, "addon-pack");
    }
    harness.state_mut().cancel_dialogs();
    harness.run_steps(2);

    harness.state_mut().check_mods(&ctx);
    wait(&mut harness);
    save(&mut harness, &dir, "mods-in-game");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);

    harness.state_mut().open_export(&ctx);
    wait(&mut harness);
    save(&mut harness, &dir, "export");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);

    harness.state_mut().open_snapshots(&ctx);
    harness.run_steps(3);
    save(&mut harness, &dir, "snapshots");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);

    // The game's Wine prefix, as Steam leaves it before .NET: made here, with a Proton
    // that has only the files the window looks for, and one backup.
    let steamapps = base.join("steamapps");
    let proton = steamapps.join("common/Proton - Experimental");
    std::fs::create_dir_all(proton.join("files/bin")).unwrap();
    for tool in ["wine", "wineserver"] {
        std::fs::write(proton.join("files/bin").join(tool), b"").unwrap();
    }
    let compat = steamapps.join("compatdata/3240220");
    std::fs::write(compat.join("config_info"), format!("1760000000\n{}/files/share/fonts/\n", proton.display())).unwrap();
    let net = compat.join("pfx/drive_c/windows/Microsoft.NET/Framework64/v4.0.30319");
    std::fs::create_dir_all(&net).unwrap();
    std::fs::write(net.join("mscorlib.dll"), vec![0u8; 700_000]).unwrap();
    std::fs::write(
        compat.join("pfx/system.reg"),
        "[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion] 1\n\"CurrentBuild\"=\"22000\"\n\n",
    )
    .unwrap();
    let prefix = paleto_core::prefix::find(&game).unwrap();
    paleto_core::prefix::backup(&prefix, "before the .NET mods").unwrap();
    harness.state_mut().open_prefix_tweaks(&ctx);
    wait(&mut harness);
    save(&mut harness, &dir, "prefix-tweaks");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);

    harness.state_mut().open_last_crash(&ctx);
    harness.run_steps(3);
    save(&mut harness, &dir, "last-crash");
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);

    harness.state_mut().open_crash_hunt(&ctx);
    harness.run_steps(3);
    harness.get_by_label("Start").click();
    wait(&mut harness);
    harness.get_by_label("It crashed").click();
    wait(&mut harness);
    if let Some(list) = harness.query_by_label_contains("Turned off for this test") {
        list.click();
    }
    harness.event(egui::Event::PointerGone);
    harness.run_steps(3);
    save(&mut harness, &dir, "crash-hunt");
    harness.get_by_label("Stop and turn everything back on").click();
    wait(&mut harness);
    drop(harness);
    let _ = std::fs::remove_dir_all(&base);
}
