//! The "Prefix tweaks" window.

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

/// A game in a fake Steam library, with a prefix and a Proton.
fn steam(dir: &std::path::Path) -> std::path::PathBuf {
    let root = dir.join("steamapps/common/Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let proton = dir.join("Proton X");
    std::fs::create_dir_all(proton.join("files/bin")).unwrap();
    std::fs::write(proton.join("files/bin/wine"), b"").unwrap();
    let compat = dir.join("steamapps/compatdata/3240220");
    std::fs::create_dir_all(compat.join("pfx/drive_c/windows")).unwrap();
    std::fs::write(
        compat.join("pfx/system.reg"),
        "[Software\\\\Microsoft\\\\Windows NT\\\\CurrentVersion] 1\n\"CurrentBuild\"=\"22000\"\n\n",
    )
    .unwrap();
    std::fs::write(
        compat.join("config_info"),
        format!("X-1\n{}/files/share/\n", proton.display()),
    )
    .unwrap();
    root
}

#[test]
fn shows_the_prefix_and_asks_before_installing() {
    let dir = tempfile::tempdir().unwrap();
    let root = steam(dir.path());
    let mut harness = Harness::builder()
        .with_size([1000.0, 800.0])
        .build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    // The button beside Install.
    harness.get_by_label("🛠 Prefix tweaks…").click();
    harness.run_steps(4);
    let summary = harness.state().prefix_tweaks_summary().unwrap();
    assert!(summary.contains("not installed"), "{summary}");
    harness.get_by_label("Proton X");
    harness.get_by_label(".NET Framework 4.8 is not installed (only Wine's stub)");
    // Installing asks first, and says what it will do.
    harness.get_by_label("Install .NET Framework 4.8…").click();
    harness.run_steps(2);
    harness.get_by_label(
        "It changes the game's Wine prefix and takes a few minutes. Close the game first.",
    );
    harness.get_by_label("Back the prefix up first (recommended)");
    if let Some(dir) = std::env::var_os("PALETO_SCREENS") {
        harness
            .render()
            .unwrap()
            .save(std::path::Path::new(&dir).join("prefix-tweaks.png"))
            .unwrap();
    }
    harness.get_by_label("Start");
    // Cancelling the question starts nothing.
    harness.get_all_by_label("Cancel").next().unwrap().click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Start").is_none());
}

#[test]
fn says_when_there_is_no_prefix() {
    // A game outside Steam: nothing to change.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut harness = Harness::builder()
        .with_size([1000.0, 800.0])
        .build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_prefix_tweaks(&ctx);
    harness.run_steps(2);
    harness.get_by_label("The game's Wine prefix cannot be changed from here.");
    assert!(
        harness
            .state()
            .prefix_tweaks_summary()
            .unwrap()
            .contains("not in a Steam library")
    );
}

/// Installs .NET for real, from the window: a copy of a prefix without .NET
/// (`PALETO_PREFIX_SOURCE`, a compatdata/<app id> folder), the network, about
/// five minutes. Run it with `XDG_DATA_HOME` pointing to a scratch folder, so
/// the downloads and the backup do not land among the real ones, and `TMPDIR`
/// on a filesystem that copies by reference:
///   cargo test -p paleto-modkit --test prefix -- --ignored
#[test]
#[ignore = "installs .NET for real: needs PALETO_PREFIX_SOURCE, the network and about five minutes"]
fn installs_dotnet_from_the_window() {
    let source = std::env::var("PALETO_PREFIX_SOURCE").expect("PALETO_PREFIX_SOURCE");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("steamapps/common/Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let compat = dir.path().join("steamapps/compatdata/3240220");
    std::fs::create_dir_all(compat.parent().unwrap()).unwrap();
    assert!(std::process::Command::new("cp").args(["-a", "--reflink=auto", &source]).arg(&compat).status().unwrap().success());

    let mut harness = Harness::builder().with_size([1000.0, 800.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.get_by_label("🛠 Prefix tweaks…").click();
    harness.run_steps(2);
    assert!(harness.state().prefix_tweaks_summary().unwrap().contains("not installed"));
    harness.get_by_label("Install .NET Framework 4.8…").click();
    harness.run_steps(2);
    harness.get_by_label("Start").click();
    harness.run_steps(3);
    // Running: a way to stop it, and the log.
    harness.get_by_label("Cancel");
    // The section's title and the running job's.
    assert_eq!(harness.get_all_by_label(".NET Framework 4.8").count(), 2);

    let end = std::time::Instant::now() + Duration::from_secs(25 * 60);
    loop {
        harness.step();
        if harness.state().prefix_tweaks_summary().unwrap().contains(".NET Framework 4.8 is installed") {
            break;
        }
        assert!(std::time::Instant::now() < end, "still running after 25 minutes");
        std::thread::sleep(Duration::from_millis(500));
    }
    harness.run_steps(3);
    harness.get_by_label(".NET Framework 4.8 is installed: ScriptHookVDotNet mods can run");
    // The backup made before the install is listed.
    assert!(harness.state().prefix_tweaks_summary().unwrap().contains("1 backups"));
    harness.get_by_label("before installing .NET");
    if let Some(shots) = std::env::var_os("PALETO_SCREENS") {
        harness.render().unwrap().save(std::path::Path::new(&shots).join("prefix-tweaks-done.png")).unwrap();
    }
}
