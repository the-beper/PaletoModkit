//! Favorites and recent locations: kept between sessions by their game folder,
//! reopened with a click, and re-pointed when the install moves.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_core::{ArchivePath, vfs};
use paleto_modkit::{App, Location};
use paleto_rpf::ArchiveEditor;

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..300 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(2);
}

/// `x.rpf` built of `files`, as bytes (the content of a nested archive).
fn archive_bytes(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.rpf");
    let mut ed = ArchiveEditor::new("x.rpf");
    for (name, data) in files {
        ed.add_file(name, data.clone()).unwrap();
    }
    ed.save(&path).unwrap();
    std::fs::read(&path).unwrap()
}

/// A game whose update.rpf holds `levels/gta5/vehicles.rpf` (one file inside),
/// and a plain folder whose name is not Unicode.
fn game_with(root: &Path, marker: u8, odd_name: Option<&[u8]>) {
    use std::os::unix::ffi::OsStringExt;
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let inner = archive_bytes(&[("adder.yft", vec![marker])]);
    let outer = archive_bytes(&[("levels/gta5/vehicles.rpf", inner), ("data/table.txt", vec![marker])]);
    std::fs::write(root.join("update/update.rpf"), outer).unwrap();
    if let Some(odd_name) = odd_name {
        let odd = root.join(OsString::from_vec(odd_name.to_vec()));
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("read me.txt"), b"hello").unwrap();
    }
}

/// In-memory stand-in for eframe's settings file.
#[derive(Default)]
struct Store(std::collections::HashMap<String, String>);

impl eframe::Storage for Store {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

#[test]
fn favorites_keep_between_sessions_and_reopen_where_they_point() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    // No game, keys or settings of the user's: a scratch home.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("PALETO_GAME");
    }
    const REL: &str = "update/update.rpf";
    let root1 = dir.path().join("Grand Theft Auto V Enhanced");
    let odd_name = vec![b'o', b'l', b'd', 0xF6];
    game_with(&root1, b'f', Some(&odd_name));
    let root2 = dir.path().join("Second Copy");
    game_with(&root2, b's', None);
    let game1 = paleto_core::Game::open(&root1).unwrap();
    let nested = |root: &Path| ArchivePath {
        disk: root.join(REL),
        inner: vec!["levels".to_string(), "gta5".to_string(), "vehicles.rpf".to_string()],
    };

    let mut harness = Harness::builder().with_size([1200.0, 800.0]).allow_missing_glyphs().build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();

    harness.state_mut().select_game_folder(&root1, &ctx);
    settle(&mut harness);
    // The inside of a nested archive appears in the recents.
    harness.state_mut().open(nested(&root1));
    settle(&mut harness);
    // A plain folder of an odd name is a recent too.
    let odd_dir = root1.join(OsString::from_vec(odd_name.clone()));
    harness.state_mut().go(Location::Folder(odd_dir.clone()));
    settle(&mut harness);
    let odd_rel = vfs::in_game(&odd_dir, &game1).unwrap();
    let odd_title = odd_rel.to_string_lossy().into_owned();
    assert_eq!(
        harness.state().recent_places(),
        [odd_title.clone(), format!("{REL}/levels/gta5/vehicles.rpf")],
        "the recents, newest first, by their path in the game"
    );

    // Keep the nested place: the window takes the place shown.
    harness.state_mut().open(nested(&root1));
    settle(&mut harness);
    harness.state_mut().open_favorites(&ctx);
    harness.run_steps(2);
    harness.get_by_label("Add the shown place…").click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().favorite_places(), [format!("{REL}/levels/gta5/vehicles.rpf")]);

    // Kept through a session: saved and read back where they were.
    let mut store = Store::default();
    eframe::App::save(harness.state_mut(), &mut store);
    let expected = format!("{}\tarchive\t{REL}\tlevels/gta5/vehicles.rpf", root1.display());
    assert_eq!(store.0.get("favorites"), Some(&expected), "the storage form: the game folder, the kind, the rel and the inner path");
    // A session where the favorite was forgotten reads it back from the storage.
    harness.get_all_by_label("Forget").next().unwrap().click_accesskit();
    harness.run_steps(2);
    assert!(harness.state().favorite_places().is_empty());
    harness.state_mut().restore(&store, &ctx);
    assert_eq!(harness.state().favorite_places(), [format!("{REL}/levels/gta5/vehicles.rpf")]);

    // Reopen it by name: the place that way is the one it was made of.
    harness.get_by_label(&format!("{REL} (levels/gta5/vehicles.rpf)")).click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Archive(nested(&root1))));

    // ...the folder of the odd name, the same: kept lossless, not by its
    // Unicode stand-in. The first row named so is the favorite (the recents
    // hold one of the same name).
    harness.get_all_by_label("Forget").next().unwrap().click_accesskit();
    harness.run_steps(2);
    harness.state_mut().go(Location::Folder(odd_dir.clone()));
    settle(&mut harness);
    harness.get_by_label("Add the shown place…").click_accesskit();
    settle(&mut harness);
    store.0.clear();
    eframe::App::save(harness.state_mut(), &mut store);
    assert_eq!(
        store.0.get("favorites"),
        Some(&format!("{}\tfolder\told%F6\t", root1.display())),
        "the name of the disk keeps its bytes: `old` with the byte 0xF6 stored as %F6"
    );

    // Reopen it: the same path back, byte by byte. The first row named so is
    // the favorite's (the recents hold one of the same name).
    harness.get_all_by_label(odd_title.as_str()).next().unwrap().click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Folder(odd_dir.clone())));

    // The item is gone: the list says why, and the entry stays (favorites are
    // never pruned for being missing), and a click opens nothing.
    std::fs::remove_dir_all(&odd_dir).unwrap();
    harness.state_mut().open_favorites(&ctx);
    harness.run_steps(2);
    harness.get_by_label("the file is gone (its mods copy too)");
    let before = harness.state().location().cloned();
    harness.get_all_by_label(odd_title.as_str()).next().unwrap().click_accesskit();
    harness.run_steps(2);
    assert_eq!(harness.state().location(), before.as_ref(), "nothing opened");
    assert_eq!(harness.state().favorite_places().len(), 1, "the favorite stays");

    // A moved install: the favorite re-points to where the item of its path is now.
    harness.state_mut().select_game_folder(&root2, &ctx);
    settle(&mut harness);
    // Its own install still there: the favorite pins it, whatever the game now selected.
    harness.state_mut().open_favorites(&ctx);
    harness.run_steps(2);
    harness.get_by_label(&format!("{REL} (levels/gta5/vehicles.rpf)")).click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Archive(nested(&root1))));
    std::fs::rename(&root1, dir.path().join("Grand Theft Auto V (moved)")).unwrap();
    harness.state_mut().open_favorites(&ctx);
    harness.run_steps(2);
    harness.get_by_label(&format!("{REL} (levels/gta5/vehicles.rpf)")).click_accesskit();
    settle(&mut harness);
    assert_eq!(harness.state().location(), Some(&Location::Archive(nested(&root2))));
}
