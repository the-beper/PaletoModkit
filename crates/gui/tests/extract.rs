//! Extracting from an archive into a folder inside a game folder: through the confined writer, as every other change to a game.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
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

/// A game folder whose update.rpf holds `data/x.txt`, with an original `x64/a.rpf` and an empty mods folder.
fn game(dir: &Path) -> PathBuf {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::create_dir_all(root.join("x64")).unwrap();
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    std::fs::write(root.join("x64/a.rpf"), b"original").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    ed.add_file("data/x.txt", b"extracted".to_vec()).unwrap();
    ed.save(&root.join("update/update.rpf")).unwrap();
    root
}

/// Every file below `dir` with its bytes, except this tool's own folder.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let rel = e.path().strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if rel == ".paleto" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
            out.insert(rel, if e.path().is_file() { std::fs::read(e.path()).unwrap() } else { Vec::new() });
        }
    }
    out
}

#[test]
fn extracting_into_the_original_game_data_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = game(dir.path());
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("data"));
    settle(&mut harness);
    let before = files(&root);

    for dest in [root.join("x64"), root.join("x64/new"), root.join("update")] {
        harness.state_mut().extract_to(vec!["x.txt".into()], &dest, &ctx);
        settle(&mut harness);
        assert!(harness.state().status_error().is_some_and(|e| e.contains("original") || e.contains("game's own data")), "{dest:?}: {:?}", harness.state().status_error());
        assert_eq!(files(&root), before, "{dest:?}");
    }

    // The mods folder takes it, and so does a folder outside every game.
    harness.state_mut().extract_to(vec!["x.txt".into()], &root.join("mods/x64"), &ctx);
    settle(&mut harness);
    assert_eq!(std::fs::read(root.join("mods/x64/x.txt")).unwrap(), b"extracted");
    assert_eq!(std::fs::read(root.join("x64/a.rpf")).unwrap(), b"original");
    harness.state_mut().extract_to(vec!["x.txt".into()], &dir.path().join("plain"), &ctx);
    settle(&mut harness);
    assert_eq!(std::fs::read(dir.path().join("plain/x.txt")).unwrap(), b"extracted");
}
