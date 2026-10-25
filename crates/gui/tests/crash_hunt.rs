//! The "Crash detection" window: causes, history and the search by elimination.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::App;

fn summary(harness: &Harness<'_, App>) -> String {
    harness.state().crash_detection_summary().unwrap()
}

fn settle(harness: &mut Harness<'_, App>) {
    // A click is handled on the frame after it: let it start its task before waiting for idle.
    harness.run_steps(2);
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
    harness.state_mut().open_crash_detection(&ctx);
    settle(&mut harness);
    harness.get_by_label("Start").click();
    settle(&mut harness);
    // Two suspects: Broken.asi is turned off first; the game then works.
    assert!(summary(&harness).contains("hunt: testing: 2 suspects, off: Broken.asi"), "{}", summary(&harness));
    assert!(!root.join("Broken.asi").exists() && root.join("OpenRPF.asi").exists());
    harness.get_by_label("It works").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: turned off: Broken.asi"), "{}", summary(&harness));
    assert!(root.join("Fine.asi").exists() && !root.join("Broken.asi").exists());

    harness.get_by_label("Turn everything back on").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: idle"), "{}", summary(&harness));
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
    harness.state_mut().open_crash_detection(&ctx);
    settle(&mut harness);
    harness.get_by_label("Start").click();
    settle(&mut harness);
    // Fine.asi off: still crashes, so update.rpf is the culprit.
    assert!(summary(&harness).contains("hunt: testing: 2 suspects, off: Fine.asi"), "{}", summary(&harness));
    harness.get_by_label("It crashed").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: turned off: mods/update/update.rpf"), "{}", summary(&harness));

    harness.get_by_label("Look inside it").click();
    settle(&mut harness);
    harness.get_by_label("Looking inside update/update.rpf: the game's own files are put back for the files turned off.");
    assert_eq!(read("handling.meta"), b"stock", "turned off: the game's own is back");
    assert_eq!(read("timecycle.xml"), b"pretty");
    harness.get_by_label("It works").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: turned off: update/update.rpf/common/data/handling.meta"), "{}", summary(&harness));

    harness.get_by_label("Turn everything back on").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: idle"), "{}", summary(&harness));
    assert_eq!(read("handling.meta"), b"broken");
}

/// A minimal minidump: the game's exe and Trainer.asi as modules, a crash at `address` and `stack` on the crashing thread's stack.
fn dump(address: u64, stack: &[u64]) -> Vec<u8> {
    let mut d = vec![0u8; 0x400];
    d[..4].copy_from_slice(b"MDMP");
    let put32 = |d: &mut Vec<u8>, at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, at: usize, v: u64| d[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut d, 8, 3);
    put32(&mut d, 12, 0x20);
    for (i, (kind, rva)) in [(4u32, 0x100u32), (6, 0x200), (3, 0x300)].into_iter().enumerate() {
        put32(&mut d, 0x20 + i * 12, kind);
        put32(&mut d, 0x20 + i * 12 + 8, rva);
    }
    d.resize(0x1000, 0);
    put32(&mut d, 0x100, 2);
    for (i, (base, size, name_at)) in [(0x1_4000_0000u64, 0x100_0000u32, 0x800usize), (0x7000_0000, 0x10_0000, 0x880)].into_iter().enumerate() {
        let m = 0x104 + i * 108;
        put64(&mut d, m, base);
        put32(&mut d, m + 8, size);
        put32(&mut d, m + 20, name_at as u32);
    }
    for (at, name) in [(0x800usize, "C:\\Game\\GTA5_Enhanced.exe"), (0x880, "C:\\Game\\Trainer.asi")] {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        put32(&mut d, at, units.len() as u32);
        d[at + 4..at + 4 + units.len()].copy_from_slice(&units);
    }
    put32(&mut d, 0x200, 7);
    put32(&mut d, 0x208, 0xC000_0005);
    put64(&mut d, 0x208 + 16, address);
    put32(&mut d, 0x208 + 24, 2);
    put64(&mut d, 0x208 + 40, 0x10);
    put32(&mut d, 0x208 + 156, 0x900);
    put64(&mut d, 0x900 + 0x98, 0x5000);
    put32(&mut d, 0x300, 1);
    put32(&mut d, 0x304, 7);
    put64(&mut d, 0x304 + 24, 0x5000);
    put32(&mut d, 0x304 + 32, (stack.len() * 8) as u32);
    put32(&mut d, 0x304 + 36, 0xA00);
    for (i, v) in stack.iter().enumerate() {
        put64(&mut d, 0xA00 + i * 8, *v);
    }
    d
}

/// A fake Enhanced game with these dumps (name, address, stack, seconds ago) and plugins.
fn game_with_crashes(dir: &std::path::Path, plugins: &[&str], dumps: &[(&str, u64, &[u64], u64)]) -> std::path::PathBuf {
    let root = dir.join("Grand Theft Auto V Enhanced");
    let crashes = root.join("GTAV Enhanced/CrashLogs");
    std::fs::create_dir_all(&crashes).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    for p in plugins {
        std::fs::write(root.join(p), b"").unwrap();
    }
    for (name, address, stack, ago) in dumps {
        let path = crashes.join(name);
        std::fs::write(&path, dump(*address, stack)).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(*ago)).unwrap();
    }
    root
}

fn open(root: &std::path::Path) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1000.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_crash_detection(&ctx);
    settle(&mut harness);
    harness
}

#[test]
fn shows_the_last_crash() {
    let dir = tempfile::tempdir().unwrap();
    let root = game_with_crashes(dir.path(), &[], &[("a.dmp", 0x1_4012_3456, &[], 200), ("b.dmp", 0x1_4012_3456, &[], 100)]);
    let mut harness = open(&root);
    assert!(summary(&harness).contains("game's own code"), "{}", summary(&harness));
    harness.get_by_label("The game crashed at the same place once more before: a cause that is still there.");
    harness.get_by_label("GTA5_Enhanced.exe + 0x123456");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&dir).join("crash.png")).unwrap();
    }
    // The own-code reading has no single culprit: elimination is suggested, and is the next section.
    harness.get_by_label("No single mod stands out: turning mods off by halves finds the one that crashes the game.");
    assert!(summary(&harness).contains("hunt: idle"));
}

#[test]
fn says_when_the_game_never_crashed() {
    let dir = tempfile::tempdir().unwrap();
    let root = game_with_crashes(dir.path(), &[], &[]);
    let harness = open(&root);
    harness.get_by_label("No crash dump yet: the game has not crashed");
    assert!(summary(&harness).contains("no crash") && summary(&harness).contains("history: 0"));
}

#[test]
fn names_the_plugin_and_turns_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let root = game_with_crashes(dir.path(), &["Trainer.asi", "Other.asi"], &[("a.dmp", 0x1_4000_1000, &[0x7000_0500], 10)]);
    let mut harness = open(&root);
    assert!(summary(&harness).contains("cause: Warning: On the crashing thread's stack: Trainer.asi."), "{}", summary(&harness));
    harness.get_by_label("Turn off Trainer.asi").click();
    settle(&mut harness);
    assert!(!root.join("Trainer.asi").exists() && root.join("Other.asi").exists());
    assert!(root.join("mods_disabled/Trainer.asi").exists());
    assert!(summary(&harness).contains("hunt: turned off: Trainer.asi"), "{}", summary(&harness));
    harness.get_by_label("Turn everything back on").click();
    settle(&mut harness);
    assert!(root.join("Trainer.asi").exists());
}

#[test]
fn lists_the_history_and_shows_an_older_crash() {
    let dir = tempfile::tempdir().unwrap();
    let root = game_with_crashes(
        dir.path(),
        &["Trainer.asi"],
        &[("a.dmp", 0x1_4000_1000, &[], 3000), ("b.dmp", 0x7000_0010, &[], 2000), ("c.dmp", 0x1_4000_1000, &[], 1000)],
    );
    let mut harness = open(&root);
    assert!(summary(&harness).contains("history: 3") && summary(&harness).contains("shown: c.dmp"), "{}", summary(&harness));
    harness.get_by_label("History (3 crashes)").click();
    harness.run_steps(2);
    assert_eq!(harness.get_all_by_label_contains("same place ×2").count(), 2);
    harness.get_by_label_contains("Trainer.asi + 0x10").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("shown: b.dmp") && summary(&harness).contains("The crash happened inside Trainer.asi."), "{}", summary(&harness));
}

#[test]
fn hunt_runs_beside_the_causes() {
    let dir = tempfile::tempdir().unwrap();
    let root = game_with_crashes(dir.path(), &["OpenRPF.asi", "Trainer.asi", "Fine.asi"], &[("a.dmp", 0x1_4000_1000, &[0x7000_0500], 10)]);
    let mut harness = open(&root);
    harness.get_by_label("Start").click();
    settle(&mut harness);
    let text = summary(&harness);
    assert!(text.contains("hunt: testing: 2 suspects") && text.contains("Trainer.asi"), "{text}");
    harness.get_by_label("Stop and turn everything back on").click();
    settle(&mut harness);
    assert!(summary(&harness).contains("hunt: idle") && root.join("Trainer.asi").exists());
}
