//! The "Script Hook V" window, its update and the notices shown at start. Nothing here uses the network.

use std::io::Write;
use std::time::{Duration, SystemTime};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::scripthook::Release;
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

/// A game executable whose version resource says 1.0.1158.16.
fn exe() -> Vec<u8> {
    let mut data = b"MZ\0\0".to_vec();
    data.extend(0xFEEF04BDu32.to_le_bytes());
    data.extend([0u8; 4]);
    // File version: minor, major, revision, build.
    for field in [0u16, 1, 16, 1158] {
        data.extend(field.to_le_bytes());
    }
    data
}

fn dll(version: &str) -> Vec<u8> {
    let mut data = b"MZ\0\0junk".to_vec();
    data.extend(format!("v{version}\0").as_bytes());
    data
}

fn zip_with(dll: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("bin/ScriptHookV.dll", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(dll).unwrap();
    zip.finish().unwrap().into_inner()
}

fn release() -> Release {
    Release { version: "3889.0/1158.13".into(), url: "https://example.invalid/ScriptHookV.zip".into(), supports: "1.0.3889.0 / 1.0.1158.13".into() }
}

/// An Enhanced game with this Script Hook V installed (none when `shv` is None).
fn game(dir: &std::path::Path, shv: Option<&str>) -> std::path::PathBuf {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), exe()).unwrap();
    if let Some(version) = shv {
        std::fs::write(root.join("ScriptHookV.dll"), dll(version)).unwrap();
    }
    root
}

fn start(root: &std::path::Path) -> Harness<'static, App> {
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(&mut harness);
    harness
}

fn summary(harness: &Harness<'_, App>) -> String {
    harness.state().script_hook_summary().unwrap()
}

#[test]
fn compatibility_verdicts() {
    let dir = tempfile::tempdir().unwrap();
    // Built for the game's build (1158): not seen working yet.
    let root = game(dir.path(), Some("3888.0/1158.13"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    harness.get_by_label("Script Hook V");
    let text = summary(&harness);
    assert!(text.contains("installed: v3888.0/1158.13") && text.contains("game: 1.0.1158.16"), "{text}");
    assert!(text.contains("built for this game version, not run yet: start the game once to confirm"), "{text}");
    // Offline: no look for the latest release.
    assert!(text.contains("latest: not checked") && !text.contains("update:"), "{text}");
    harness.get_by_label("Installed");
    harness.get_by_label("Game version");

    // Its log says it started.
    std::fs::write(root.join("ScriptHookV.log"), "// GTA V SCRIPT HOOK (build Jul 15 2026, v3888.0/1158.13)\n[19:49:14] INIT: Success, game version is VER_EN_1_0_1158_16\n").unwrap();
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    assert!(summary(&harness).contains("works with this game version"), "{}", summary(&harness));

    // Built for an older build.
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), Some("3888.0/1157.9"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    let text = summary(&harness);
    assert!(text.contains("supports 3888.0/1157.9 but the game is 1.0.1158.16"), "{text}");
    assert!(text.contains("until a newer Script Hook V is released"), "{text}");
}

#[test]
fn updates_from_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), Some("3888.0/1157.9"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().set_script_hook_zip(zip_with(&dll("3889.0/1158.13")));
    harness.state_mut().offer_script_hook(release());
    harness.run_steps(3);
    // The notice comes first; the window is opened after it.
    harness.get_by_label("Not now").click();
    harness.run_steps(2);
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    let text = summary(&harness);
    assert!(text.contains("latest: v3889.0/1158.13") && text.contains("update: v3889.0/1158.13"), "{text}");
    assert!(text.contains("update Script Hook V to v3889.0/1158.13"), "{text}");

    harness.get_by_label("Update to v3889.0/1158.13").click();
    settle(&mut harness);
    let text = summary(&harness);
    assert!(text.contains("installed: v3889.0/1158.13") && !text.contains("update:"), "{text}");
    assert!(text.contains("built for this game version"), "{text}");
    // Replaced in place, with no backup beside it.
    assert!(dll_version(&root).contains("3889.0/1158.13"));
    let files: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(files.iter().filter(|f| f.to_ascii_lowercase().contains("scripthookv")).count(), 1, "{files:?}");
}

fn dll_version(root: &std::path::Path) -> String {
    String::from_utf8_lossy(&std::fs::read(root.join("ScriptHookV.dll")).unwrap()).into_owned()
}

#[test]
fn already_the_latest() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), Some("3889.0/1157.9"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().offer_script_hook(Release { version: "3889.0/1157.9".into(), ..release() });
    // Not newer than the installed one: no notice.
    harness.run_steps(3);
    assert_eq!(harness.state().notice_text(), None);
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    let text = summary(&harness);
    assert!(text.contains("already the latest release: wait for the author's update"), "{text}");
    assert!(!text.contains("update:"), "{text}");
}

#[test]
fn notices_come_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), Some("3888.0/1157.9"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().offer_script_hook(release());
    harness.state_mut().offer_crash(SystemTime::now() - Duration::from_secs(300), "The crash happened in Trainer.asi.");
    // The checks of the start find the Script Hook V that does not suit the game.
    harness.state_mut().startup_checks(&ctx);
    settle(&mut harness);
    let text = harness.state().notice_text().unwrap();
    assert!(text.contains("Script Hook V v3889.0/1158.13 is available (installed: v3888.0/1157.9)."), "{text}");
    harness.get_by_label("Not now").click();
    harness.run_steps(3);
    let text = harness.state().notice_text().unwrap();
    assert!(text.starts_with("Script Hook V is not compatible with this game version"), "{text}");
    assert!(text.contains("supports 3888.0/1157.9 but the game is 1.0.1158.16"), "{text}");
    harness.get_by_label("Close").click();
    harness.run_steps(3);
    let text = harness.state().notice_text().unwrap();
    assert!(text.starts_with("The game crashed 5 min ago") && text.contains("The crash happened in Trainer.asi."), "{text}");
    harness.get_by_label("Dismiss").click();
    harness.run_steps(3);
    assert_eq!(harness.state().notice_text(), None);
}

#[test]
fn update_from_the_notice() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), Some("3888.0/1157.9"));
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().set_script_hook_zip(zip_with(&dll("3889.0/1158.13")));
    harness.state_mut().offer_script_hook(release());
    harness.state_mut().startup_checks(&ctx);
    settle(&mut harness);
    harness.get_by_label("Update now").click();
    settle(&mut harness);
    // Updated: it now suits the game, so the next notice, about compatibility, never comes up.
    assert!(dll_version(&root).contains("3889.0/1158.13"));
    assert_eq!(harness.state().notice_text(), None);
}

#[test]
fn no_news_without_script_hook() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path(), None);
    let mut harness = start(&root);
    let ctx = harness.ctx.clone();
    harness.state_mut().offer_script_hook(release());
    harness.state_mut().startup_checks(&ctx);
    settle(&mut harness);
    assert_eq!(harness.state().notice_text(), None);
    // The window still offers to install it.
    harness.state_mut().open_script_hook(&ctx);
    harness.run_steps(2);
    assert!(summary(&harness).contains("installed: not installed"), "{}", summary(&harness));
    harness.get_by_label("Install v3889.0/1158.13");
}
