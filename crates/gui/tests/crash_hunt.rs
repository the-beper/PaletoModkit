//! The "Find the crashing mod" window.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::App;

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
fn finds_the_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    for f in ["GTA5_Enhanced.exe", "OpenRPF.asi", "Broken.asi", "Fine.asi"] {
        std::fs::write(root.join(f), b"").unwrap();
    }

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_crash_hunt(&ctx);
    harness.run_steps(2);
    harness.get_by_label("Start").click();
    settle(&mut harness);
    // Two suspects: Broken.asi is turned off first; the game then works.
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "testing: 2 suspects, off: Broken.asi");
    assert!(!root.join("Broken.asi").exists() && root.join("OpenRPF.asi").exists());
    harness.get_by_label("It works").click();
    settle(&mut harness);
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "turned off: Broken.asi");
    assert!(root.join("Fine.asi").exists() && !root.join("Broken.asi").exists());

    harness.get_by_label("Turn everything back on").click();
    settle(&mut harness);
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "idle");
    assert!(root.join("Broken.asi").exists());
}

#[test]
fn looks_inside_the_archive() {
    use paleto_rpf::ArchiveEditor;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods/update")).unwrap();
    std::fs::create_dir_all(root.join("update")).unwrap();
    for f in ["GTA5_Enhanced.exe", "OpenRPF.asi", "Fine.asi"] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    let mut update = ArchiveEditor::new("update.rpf");
    update.add_file("common/data/handling.meta", b"stock".to_vec()).unwrap();
    update.add_file("common/data/timecycle.xml", b"stock".to_vec()).unwrap();
    update.save(&root.join("update/update.rpf")).unwrap();
    let mut update = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
    update.add_file("common/data/handling.meta", b"broken".to_vec()).unwrap();
    update.add_file("common/data/timecycle.xml", b"pretty".to_vec()).unwrap();
    update.save(&root.join("mods/update/update.rpf")).unwrap();
    let read = |f: &str| ArchiveEditor::open(&root.join("mods/update/update.rpf"), None).unwrap().read(&format!("common/data/{f}"), None).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_crash_hunt(&ctx);
    harness.run_steps(2);
    harness.get_by_label("Start").click();
    settle(&mut harness);
    // Fine.asi off: still crashes, so update.rpf is the culprit.
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "testing: 2 suspects, off: Fine.asi");
    harness.get_by_label("It crashed").click();
    settle(&mut harness);
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "turned off: mods/update/update.rpf");

    harness.get_by_label("Look inside it").click();
    settle(&mut harness);
    harness.get_by_label("Looking inside update/update.rpf: the game's own files are put back for the files turned off.");
    assert_eq!(read("handling.meta"), b"stock", "turned off: the game's own is back");
    assert_eq!(read("timecycle.xml"), b"pretty");
    harness.get_by_label("It works").click();
    settle(&mut harness);
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "turned off: update/update.rpf/common/data/handling.meta");

    harness.get_by_label("Turn everything back on").click();
    settle(&mut harness);
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "idle");
    assert_eq!(read("handling.meta"), b"broken");
}

/// A minimal minidump: one module, the game's exe, and a crash in it.
fn dump(address: u64) -> Vec<u8> {
    let mut d = vec![0u8; 0x1000];
    let put32 = |d: &mut Vec<u8>, at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, at: usize, v: u64| d[at..at + 8].copy_from_slice(&v.to_le_bytes());
    d[..4].copy_from_slice(b"MDMP");
    put32(&mut d, 8, 2);
    put32(&mut d, 12, 0x20);
    put32(&mut d, 0x20, 4);
    put32(&mut d, 0x28, 0x100);
    put32(&mut d, 0x2C, 6);
    put32(&mut d, 0x34, 0x200);
    put32(&mut d, 0x100, 1);
    put64(&mut d, 0x104, 0x1_4000_0000);
    put32(&mut d, 0x10C, 0x100_0000);
    put32(&mut d, 0x118, 0x800);
    let name: Vec<u8> = "GTA5_Enhanced.exe".encode_utf16().flat_map(u16::to_le_bytes).collect();
    put32(&mut d, 0x800, name.len() as u32);
    d[0x804..0x804 + name.len()].copy_from_slice(&name);
    put32(&mut d, 0x208, 0x8000_0003);
    put64(&mut d, 0x218, address);
    d
}

#[test]
fn shows_the_last_crash() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("GTAV Enhanced/CrashLogs")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    std::fs::write(root.join("GTAV Enhanced/CrashLogs/a.dmp"), dump(0x1_4012_3456)).unwrap();
    std::fs::write(root.join("GTAV Enhanced/CrashLogs/b.dmp"), dump(0x1_4012_3456)).unwrap();

    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_last_crash(&ctx);
    harness.run_steps(2);
    assert!(harness.state().crash_verdict().unwrap().contains("game's own code"));
    harness.get_by_label("GTA5_Enhanced.exe + 0x123456");
    harness.get_by_label("The game crashed at the same place once more before: a cause that is still there.");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("crash.png")).unwrap();
    }
    harness.get_by_label("Find the crashing mod…").click();
    harness.run_steps(2);
    assert!(harness.state().crash_verdict().is_none());
    assert_eq!(harness.state().crash_hunt_summary().unwrap(), "idle");
}
