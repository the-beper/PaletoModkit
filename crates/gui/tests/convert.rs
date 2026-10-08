//! Converting the mods folder's Legacy files from the "Mods in game" window.
//! A test binary of its own: it sets PALETO_CWCONV.

use std::os::unix::fs::PermissionsExt;
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
fn converts_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    // A stand-in for paleto-cwconv: a version 165 model becomes 159.
    let tool = dir.path().join("paleto-cwconv");
    std::fs::write(
        &tool,
        r#"#!/bin/sh
while [ $# -gt 0 ]; do
    { printf 'RSC7\237\000\000\000\000\000\000\220\000\000\000\360'; tail -c +17 "$1"; } > "$2"
    echo "converted $1"
    shift 2
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: this test binary runs this single test. No cached keys, as in
    // CI: the fake game's keys cannot be found, and mods are checked anyway.
    unsafe {
        std::env::set_var("PALETO_CWCONV", &tool);
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
    }

    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut pack = ArchiveEditor::new("dlc.rpf");
    pack.archive("x64/vehicles.rpf", true, None).unwrap().add_file("car.ydr", resource(165)).unwrap();
    pack.save(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    // The background check at start says so in the status bar.
    harness.get_by_label("⚠ 1 file in the mods folder was made for Legacy");
    harness.state_mut().check_mods(&ctx);
    settle(&mut harness);
    harness.get_by_label("1 file in the mods folder was made for Legacy: the Enhanced game may crash on it or ignore it.");
    harness.get_by_label("Convert it to Enhanced").click();
    settle(&mut harness);

    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("all files are in the Enhanced format"), "{summary}");
    let mut saved = ArchiveEditor::open(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), None).unwrap();
    assert_eq!(saved.archive("x64/vehicles.rpf", false, None).unwrap().read("car.ydr", None).unwrap(), resource(159));
    // The Legacy file is kept.
    let kept: Vec<_> = std::fs::read_dir(root.join("mods_legacy")).unwrap().flatten().collect();
    assert_eq!(kept.len(), 1);
    let original = kept[0].path().join("update/x64/dlcpacks/car/dlc.rpf/x64/vehicles.rpf/car.ydr");
    assert_eq!(std::fs::read(original).unwrap(), resource(165));

    // The conversion can be undone from the same window.
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("last conversion:"), "{summary}");
    harness.get_by_label("Undo the last conversion").click();
    settle(&mut harness);
    assert_eq!(harness.state().status_error(), None);
    let mut saved = ArchiveEditor::open(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), None).unwrap();
    assert_eq!(saved.archive("x64/vehicles.rpf", false, None).unwrap().read("car.ydr", None).unwrap(), resource(165));
    assert_eq!(std::fs::read_dir(root.join("mods_legacy")).unwrap().count(), 0);
    let summary = harness.state().mods_check_summary().unwrap();
    assert!(summary.contains("not Enhanced:") && !summary.contains("last conversion"), "{summary}");

    // A package with a Legacy model inside its dlc.rpf: the installer says so
    // and offers to convert it.
    use std::io::Write;
    let mut pack = ArchiveEditor::new("dlc.rpf");
    pack.archive("x64/vehicles.rpf", true, None).unwrap().add_file("bike.yft", resource(162)).unwrap();
    pack.save(&dir.path().join("dlc.rpf")).unwrap();
    let oiv = dir.path().join("bike.oiv");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&oiv).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("assembly.xml", opts).unwrap();
    zip.write_all(
        br#"<package version="2.2" target="Five"><metadata><name>Bike</name></metadata>
        <content><add source="dlc.rpf">update\x64\dlcpacks\bike\dlc.rpf</add></content></package>"#,
    )
    .unwrap();
    zip.start_file("content/dlc.rpf", opts).unwrap();
    zip.write_all(&std::fs::read(dir.path().join("dlc.rpf")).unwrap()).unwrap();
    zip.finish().unwrap();
    harness.state_mut().check_mods(&ctx);
    harness.state_mut().open_package(&oiv, &ctx);
    harness.run_steps(3);
    harness.get_by_label_contains("1 file in this package was made for Legacy");
    harness.get_by_label("Convert it to Enhanced while installing (with CodeWalker's converter)");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("install-legacy.png")).unwrap();
    }

    harness.get_by_label("Cancel").click();
    harness.run_steps(2);

    // A Legacy model dropped on an archive folder: the window offers to convert it.
    let loose = dir.path().join("van.ydr");
    std::fs::write(&loose, resource(165)).unwrap();
    let archive = root.join("mods/update/x64/dlcpacks/car/dlc.rpf");
    harness.state_mut().open(paleto_core::ArchivePath::new(archive.clone()).join("x64"));
    settle(&mut harness);
    harness.state_mut().add_files(vec![loose.clone()], &ctx);
    harness.run_steps(3);
    harness.get_by_label("Files made for Legacy");
    harness.get_by_label("1 file is in the Legacy format: the Enhanced game may crash on it or ignore it.");
    harness.get_by_label("Convert it to Enhanced (with CodeWalker's converter)");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("add-legacy.png")).unwrap();
    }
    harness.get_by_label("Add").click();
    settle(&mut harness);
    assert_eq!(harness.state().status_error(), None);
    let pack = ArchiveEditor::open(&archive, None).unwrap();
    assert_eq!(pack.read("x64/van.ydr", None).unwrap(), resource(159));
    assert_eq!(std::fs::read(&loose).unwrap(), resource(165), "the file on disk is not changed");
}
