use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;

use super::*;
use crate::fixtures::*;
use crate::game::Edition;

fn rel(p: &str) -> RelPath {
    RelPath::parse(p).unwrap()
}

fn open(dir: &Path) -> GameRoots {
    game_folder(dir, Edition::Enhanced).roots().unwrap()
}

fn write(roots: &GameRoots, path: &str, data: &[u8]) -> Result<(), Error> {
    let mut dest = roots.dest(&rel(path))?;
    roots.replace(&mut dest, |f| f.write_all(data))
}

fn err(r: Result<impl std::fmt::Debug, Error>) -> String {
    r.unwrap_err().to_string()
}

#[test]
fn names_match_ignoring_case_and_the_exact_name_wins() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    std::fs::rename(dir.path().join("game/scripts"), dir.path().join("game/Scripts")).unwrap();
    let dest = roots.dest(&rel("SCRIPTS\\ORIG.asi")).unwrap();
    assert_eq!((dest.logical.clone(), dest.name.as_str()), (vec!["Scripts".to_string()], "orig.asi"));
    assert!(matches!(dest.existing, Some(Existing::File(_))));
    std::fs::create_dir(dir.path().join("game/scripts")).unwrap();
    assert_eq!(roots.dest(&rel("scripts/a.asi")).unwrap().logical, ["scripts"], "the exact name wins");
    // Several that differ only in case: the first in order, not a random one.
    std::fs::create_dir(dir.path().join("game/SCRIPTS")).unwrap();
    assert_eq!(roots.dest(&rel("ScRiPtS/a.asi")).unwrap().logical, ["SCRIPTS"]);
}

#[test]
fn a_new_path_lists_the_folders_to_make() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let dest = roots.dest(&rel("plugins/deep/er/b.cfg")).unwrap();
    assert_eq!((dest.logical.len(), dest.missing.clone(), dest.name.as_str(), dest.existing.is_none()), (0, vec!["plugins".to_string(), "deep".into(), "er".into()], "b.cfg", true));
    assert!(!dir.path().join("game/plugins").exists(), "resolving writes nothing");
    let mut dest = dest;
    roots.ensure_parent(&mut dest).unwrap();
    assert!(dir.path().join("game/plugins/deep/er").is_dir());
    assert_eq!((dest.logical, dest.missing), (vec!["plugins".to_string(), "deep".into(), "er".into()], vec![]));
}

#[test]
fn internal_links_are_followed_and_the_path_handed_out_is_link_free() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    std::fs::rename(game.join("scripts"), game.join("scripts_real")).unwrap();
    symlink("scripts_real", game.join("scripts")).unwrap();
    symlink("scripts", game.join("again")).unwrap();
    symlink(game.join("scripts_real"), game.join("absolute")).unwrap();
    let canon = std::fs::canonicalize(&game).unwrap();
    symlink(canon.join("scripts_real"), game.join("canonical")).unwrap();
    for via in ["scripts", "again", "absolute", "canonical"] {
        let dest = roots.dest(&rel(&format!("{via}/orig.asi"))).unwrap();
        assert_eq!(dest.logical, ["scripts_real"], "{via}");
        assert_eq!(dest.path(), roots.game.shown.join("scripts_real/orig.asi"), "{via}");
    }
}

#[test]
fn links_that_leave_the_root_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    for (name, target) in [
        ("up", "..".to_string()),
        ("up2", "scripts/../..".to_string()),
        ("out", outside.display().to_string()),
        ("outdots", format!("{}/../outside", game.display())),
        ("deep", "../outside".to_string()),
        ("gone", "/nonexistent/place".to_string()),
    ] {
        symlink(&target, game.join(name)).unwrap();
        let e = err(roots.dest(&rel(&format!("{name}/x.asi"))));
        assert!(e.contains("leads outside"), "{name} -> {target}: {e}");
    }
    // `..` that stays inside is fine.
    symlink("scripts/../x64e", game.join("inside")).unwrap();
    let dest = roots.dest(&rel("inside/x.asi")).unwrap();
    assert_eq!((dest.logical.len(), dest.missing), (0, vec!["x64e".to_string()]), "a dangling link inside the root names a folder to make");
}

#[test]
fn link_loops_and_links_through_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    symlink("loop", game.join("loop")).unwrap();
    symlink("b", game.join("a")).unwrap();
    symlink("a", game.join("b")).unwrap();
    for name in ["loop", "a", "b"] {
        let e = err(roots.dest(&rel(&format!("{name}/x.asi"))));
        assert!(e.contains("link") && e.starts_with("unsafe path"), "{name}: {e}");
    }
    let e = err(roots.dest(&rel("scripts/orig.asi/x")));
    assert!(e.contains("is a file, not a folder"), "{e}");
}

#[test]
fn loose_files_never_land_on_game_data_or_the_tools_folders() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    std::fs::create_dir_all(game.join("mods_snapshots")).unwrap();
    std::fs::create_dir_all(game.join(".paleto")).unwrap();
    for (name, target) in [("a", "x64"), ("b", "update"), ("c", "mods"), ("d", "mods_snapshots"), ("e", ".paleto")] {
        symlink(target, game.join(name)).unwrap();
    }
    for name in ["a", "b", "c", "d", "e"] {
        let e = err(roots.dest(&rel(&format!("{name}/evil.asi"))));
        assert!(e.starts_with("unsafe path"), "{name}: {e}");
    }
    // Names that are the tool's even without a link.
    for path in [".paleto/journal/x", "mods_snapshots/s/x", "mods_disabled/x", "mods_legacy/1/x", ".mods.restoring/x", "mods"] {
        let e = err(roots.dest(&rel(path)));
        assert!(e.starts_with("unsafe path"), "{path}: {e}");
    }
    // A program is refused when written, not when found (callers skip it quietly).
    let mut exe = roots.dest(&rel("GTA5_Enhanced.exe")).unwrap();
    let e = err(roots.replace(&mut exe, |f| f.write_all(b"x")));
    assert!(e.contains("programs are never replaced"), "{e}");
    assert!(roots.remove(&exe).is_err());
    assert_eq!(std::fs::read(game.join("GTA5_Enhanced.exe")).unwrap(), b"exe");
}

#[test]
fn mods_folder_checks() {
    for (what, target) in [("mods -> .", "."), ("mods -> ..", ".."), ("mods -> x64", "x64"), ("mods -> update", "update"), ("mods -> x64/sub", "x64/sub"), ("mods -> the tool's folder", "mods_snapshots")] {
        let dir = tempfile::tempdir().unwrap();
        let game = game_folder(dir.path(), Edition::Enhanced);
        std::fs::create_dir_all(game.root.join("x64/sub")).unwrap();
        std::fs::create_dir_all(game.root.join("mods_snapshots")).unwrap();
        symlink(target, game.root.join("mods")).unwrap();
        let roots = game.roots().unwrap();
        assert!(roots.mods().is_none(), "{what}");
        let e = err(roots.dest(&rel("x64/new.rpf")));
        assert!(e.starts_with("unsafe path") && e.contains("the mods folder"), "{what}: {e}");
        // The game folder's loose files do not depend on the mods folder.
        assert!(roots.dest(&rel("scripts/a.asi")).is_ok(), "{what}");
    }
    // A mods folder on the same drive as the originals' own place, outside the game.
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let drive2 = dir.path().join("drive2");
    std::fs::create_dir_all(&drive2).unwrap();
    std::fs::rename(game.root.join("x64"), drive2.join("x64")).unwrap();
    symlink(drive2.join("x64"), game.root.join("x64")).unwrap();
    for target in [drive2.join("x64"), drive2.join("x64/deeper"), drive2.clone()] {
        let _ = std::fs::remove_file(game.root.join("mods"));
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, game.root.join("mods")).unwrap();
        let e = err(game.roots().unwrap().dest(&rel("update/a.rpf")));
        assert!(e.contains("overlaps the game's own data"), "{}: {e}", target.display());
    }
    // A plain file named mods.
    std::fs::remove_file(game.root.join("mods")).unwrap();
    std::fs::write(game.root.join("mods"), b"").unwrap();
    assert!(err(game.roots().unwrap().dest(&rel("update/a.rpf"))).contains("is not a folder"));
}

#[test]
fn mods_folder_that_is_a_link_to_a_profile_or_another_drive_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let drive2 = dir.path().join("drive2/mods");
    std::fs::create_dir_all(&drive2).unwrap();
    std::fs::create_dir_all(game.root.join("mods_profileA")).unwrap();
    for target in [drive2.clone(), game.root.join("mods_profileA")] {
        let _ = std::fs::remove_file(game.root.join("mods"));
        symlink(&target, game.root.join("mods")).unwrap();
        let roots = game.roots().unwrap();
        write(&roots, "update/x64/dlcpacks/car/dlc.rpf", b"pack").unwrap();
        assert_eq!(std::fs::read(target.join("update/x64/dlcpacks/car/dlc.rpf")).unwrap(), b"pack");
        // Links inside it may use the folder's own real path, or the one it was reached by.
        symlink(target.join("update"), target.join("viaabs")).unwrap();
        symlink(game.root.join("mods/update"), target.join("viashown")).unwrap();
        for via in ["viaabs", "viashown"] {
            assert_eq!(roots.dest(&rel(&format!("mods/{via}/x64/dlcpacks/car/dlc.rpf"))).unwrap().logical, ["update", "x64", "dlcpacks", "car"], "{via}");
        }
    }
    // The links to the originals, though, never are.
    let roots = game.roots().unwrap();
    symlink("../../x64", game.root.join("mods/x64")).unwrap();
    assert!(err(roots.dest(&rel("x64/new.rpf"))).contains("leads outside"));
}

#[test]
fn a_missing_mods_folder_is_made_by_the_first_write_and_a_racing_maker_is_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    let mut dest = roots.dest(&rel("update/x64/dlcpacks/a/dlc.rpf")).unwrap();
    assert_eq!(dest.missing, ["mods", "update", "x64", "dlcpacks", "a"]);
    // Somebody makes `mods` and `update` between the look and the write.
    std::fs::create_dir_all(game.join("mods/update")).unwrap();
    roots.replace(&mut dest, |f| f.write_all(b"pack")).unwrap();
    assert_eq!(std::fs::read(game.join("mods/update/x64/dlcpacks/a/dlc.rpf")).unwrap(), b"pack");
    // ... or makes one a link.
    let mut dest = roots.dest(&rel("update/other/b.rpf")).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, game.join("mods/update/other")).unwrap();
    let e = err(roots.replace(&mut dest, |f| f.write_all(b"x")));
    assert!(e.contains("link"), "{e}");
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn replace_writes_through_an_unpredictable_name_and_touches_no_other_file() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let scripts = dir.path().join("game/scripts");
    std::fs::write(scripts.join(".a.asi.paleto-tmp"), b"not ours").unwrap();
    std::fs::write(scripts.join(".a.asi.paleto-undo"), b"not ours").unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    symlink(&outside, scripts.join(".b.asi.paleto-tmp")).unwrap();
    for name in ["a.asi", "b.asi"] {
        write(&roots, &format!("scripts/{name}"), name.as_bytes()).unwrap();
        write(&roots, &format!("scripts/{name}"), b"again").unwrap();
    }
    assert_eq!(std::fs::read(scripts.join("a.asi")).unwrap(), b"again");
    assert_eq!(std::fs::read(scripts.join(".a.asi.paleto-tmp")).unwrap(), b"not ours");
    assert_eq!(std::fs::read(scripts.join(".a.asi.paleto-undo")).unwrap(), b"not ours");
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    assert!(leftovers(&scripts).iter().all(|l| l.contains(".paleto-tmp") || l.contains(".paleto-undo")), "{:?}", leftovers(&scripts));
    assert_eq!(leftovers(&scripts).len(), 3);
}

#[test]
fn replace_replaces_a_link_and_refuses_one_that_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let scripts = dir.path().join("game/scripts");
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    symlink("orig.asi", scripts.join("inside.asi")).unwrap();
    symlink("nothing.asi", scripts.join("dangling.asi")).unwrap();
    symlink("/nonexistent/x", scripts.join("dangling_out.asi")).unwrap();
    symlink(&outside, scripts.join("out.asi")).unwrap();
    for name in ["inside.asi", "dangling.asi", "dangling_out.asi"] {
        write(&roots, &format!("scripts/{name}"), b"new").unwrap();
        assert!(std::fs::symlink_metadata(scripts.join(name)).unwrap().is_file(), "{name}");
    }
    assert_eq!(std::fs::read(scripts.join("orig.asi")).unwrap(), b"orig", "the target of a link is not written through");
    assert!(err(write(&roots, "scripts/out.asi", b"new")).contains("leads outside"));
    assert!(std::fs::symlink_metadata(scripts.join("out.asi")).unwrap().is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

#[test]
fn a_failed_write_leaves_the_file_and_the_folder_as_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let scripts = dir.path().join("game/scripts");
    std::fs::write(scripts.join("a.asi"), b"old").unwrap();
    let before = snapshot(&dir.path().join("game"));
    // A failing write callback, then every system call failing in turn.
    let mut dest = roots.dest(&rel("scripts/a.asi")).unwrap();
    assert!(roots.replace(&mut dest, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_unchanged(&before, &snapshot(&dir.path().join("game")));
    let mut k = 1;
    loop {
        faults::arm(k, faults::Mode::Fail(std::io::ErrorKind::Other));
        let result = roots.dest(&rel("scripts/a.asi")).and_then(|mut d| roots.replace(&mut d, |f| f.write_all(b"new")));
        let fired = faults::disarm();
        if !fired || result.is_ok() {
            // Done, or only the best-effort flush of the folder failed after the rename.
            result.unwrap();
            break;
        }
        // The resolution or the write failed: nothing may have changed.
        assert_unchanged(&before, &snapshot(&dir.path().join("game")));
        k += 1;
        assert!(k < 200);
    }
    assert!(k > 8, "the loop reached the system calls: {k}");
    assert_eq!(std::fs::read(scripts.join("a.asi")).unwrap(), b"new");
    assert!(leftovers(&scripts).is_empty());
}

#[test]
fn rename_without_replace_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dir::open_path(dir.path()).unwrap();
    for (what, caps) in [("renameat2", Caps::ALL), ("no renameat2", Caps { noreplace: false, ..Caps::ALL }), ("no renameat2 and no hard links", Caps { noreplace: false, hard_links: false, openat2: true })] {
        let _caps = faults::caps(caps);
        std::fs::write(dir.path().join("from"), b"from").unwrap();
        std::fs::write(dir.path().join("to"), b"to").unwrap();
        let e = d.rename("from", "to", false).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{what}");
        assert_eq!((std::fs::read(dir.path().join("from")).unwrap(), std::fs::read(dir.path().join("to")).unwrap()), (b"from".to_vec(), b"to".to_vec()), "{what}");
        symlink("/nonexistent", dir.path().join("dangling")).unwrap();
        assert_eq!(d.rename("from", "dangling", false).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists, "{what}: a link is a name too");
        std::fs::remove_file(dir.path().join("dangling")).unwrap();
        d.rename("from", "free", false).unwrap();
        assert_eq!(std::fs::read(dir.path().join("free")).unwrap(), b"from", "{what}");
        d.rename("free", "to", true).unwrap();
        assert_eq!(std::fs::read(dir.path().join("to")).unwrap(), b"from", "{what}");
        std::fs::remove_file(dir.path().join("to")).unwrap();
    }
}

#[test]
fn directory_operations_refuse_links_and_odd_names() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dir::open_path(dir.path()).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("f"), b"f").unwrap();
    symlink(&outside, dir.path().join("dirlink")).unwrap();
    symlink(outside.join("f"), dir.path().join("filelink")).unwrap();
    assert!(d.open_dir("dirlink").is_err() && d.open_read("filelink").is_err());
    assert_eq!(d.open_read_following("filelink").map(|mut f| std::io::read_to_string(&mut f).unwrap()).unwrap(), "f");
    assert!(d.open_dir_following("dirlink").is_ok());
    assert!(d.create_excl("filelink").is_err() && d.create_excl("dirlink").is_err(), "O_EXCL does not follow");
    assert!(d.mkdir("dirlink").is_err());
    assert!(d.open_read("outside").is_err(), "a folder is not a file to read");
    for bad in ["", ".", "..", "a/b", "/etc"] {
        assert_eq!(d.open_read(bad).unwrap_err().kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
        assert!(d.unlink(bad).is_err() && d.stat(bad).is_err());
    }
    // A link is unlinked itself.
    d.unlink("filelink").unwrap();
    assert!(outside.join("f").exists());
    assert_eq!(d.kind("filelink").unwrap(), None);
    assert_eq!(d.kind("dirlink").unwrap(), Some(Kind::Link));
    assert_eq!(d.kind_following("dirlink").unwrap(), Some(Kind::Dir));
    assert_eq!(d.stat("dirlink").unwrap(), Some(Existing::Link(outside.clone().into_os_string())));
    assert_eq!(d.stat("outside").unwrap(), Some(Existing::Dir));
    assert!(d.free_space().unwrap() > 0);
    d.sync().unwrap();
    d.mkdir("sub").unwrap();
    d.rmdir("sub").unwrap();
    let mut names = d.names().unwrap();
    names.sort();
    assert_eq!(names, ["dirlink", "outside"]);
}

#[test]
fn unique_names_are_unique_and_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dir::open_path(dir.path()).unwrap();
    let names: std::collections::HashSet<String> = (0..200).map(|_| d.create_unique().unwrap().1).collect();
    assert_eq!(names.len(), 200);
    assert!(names.iter().all(|n| n.starts_with(".paleto-") && n.ends_with(".new") && n.len() == 28));
    assert_eq!(d.names().unwrap().len(), 200);
}

#[test]
fn hard_links_can_be_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let d = Dir::open_path(dir.path()).unwrap();
    std::fs::write(dir.path().join("a"), b"a").unwrap();
    d.link("a", "b").unwrap();
    assert_eq!(d.link("a", "b").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    let _caps = faults::caps(Caps { hard_links: false, ..Caps::ALL });
    assert_eq!(d.link("a", "c").unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    assert!(!dir.path().join("c").exists());
}

#[test]
fn verify_notices_a_folder_moved_out_or_swapped_for_a_link() {
    for forced in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = open(dir.path());
        if forced {
            roots.game.force_fallback();
        }
        let game = dir.path().join("game");
        let dest = roots.dest(&rel("scripts/orig.asi")).unwrap();
        roots.verify(&dest).unwrap();
        // Moved inside the game folder: the held handle no longer is `scripts`.
        std::fs::rename(game.join("scripts"), game.join("elsewhere")).unwrap();
        assert!(err(roots.verify(&dest).map(|()| 0)).contains("changed while"), "forced={forced}");
        std::fs::create_dir(game.join("scripts")).unwrap();
        assert!(roots.verify(&dest).is_err(), "forced={forced}: a different folder with the same name");
        std::fs::remove_dir(game.join("scripts")).unwrap();
        symlink("elsewhere", game.join("scripts")).unwrap();
        assert!(roots.verify(&dest).is_err(), "forced={forced}: swapped for a link to the same folder");
        // A write through the stale destination is refused, and lands nowhere new.
        let mut stale = dest;
        assert!(roots.replace(&mut stale, |f| f.write_all(b"x")).is_err());
        assert_eq!(std::fs::read(game.join("elsewhere/orig.asi")).unwrap(), b"orig");
    }
}

#[test]
fn openat2_that_is_missing_falls_back_to_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let guard = faults::caps(Caps { openat2: false, ..Caps::ALL });
    write(&roots, "scripts/a.asi", b"a").unwrap();
    assert_eq!(std::fs::read(dir.path().join("game/scripts/a.asi")).unwrap(), b"a");
    drop(guard);
    // An openat2 that fails for another reason than being absent refuses the write.
    faults::arm_at(faults::Point::Openat2, 1, faults::Mode::Fail(std::io::ErrorKind::PermissionDenied));
    let result = write(&roots, "scripts/b.asi", b"b");
    assert!(faults::disarm() && result.is_err());
    assert!(!dir.path().join("game/scripts/b.asi").exists());
    write(&roots, "scripts/c.asi", b"c").unwrap();
}

#[test]
fn reads_follow_links_inside_the_game_or_mods_folder_only() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let game = game.root;
    std::fs::create_dir_all(game.join("mods/x64")).unwrap();
    let outside = dir.path().join("outside.rpf");
    std::fs::write(&outside, b"outside").unwrap();
    let roots = GameRoots::open(&game).unwrap();
    let original = std::fs::read(game.join("x64/a.rpf")).unwrap();
    for (name, target, expect) in [
        ("rel.rpf", "../../x64/a.rpf".to_string(), Some(true)),
        ("abs.rpf", game.join("x64/a.rpf").display().to_string(), Some(true)),
        ("dangling.rpf", "nothing.rpf".to_string(), None),
        ("dangling_out.rpf", "/nonexistent/z.rpf".to_string(), None),
        ("loop.rpf", "loop.rpf".to_string(), None),
        ("out.rpf", outside.display().to_string(), Some(false)),
        ("up.rpf", "../../../outside.rpf".to_string(), Some(false)),
    ] {
        symlink(&target, game.join("mods/x64").join(name)).unwrap();
        let dest = roots.mods_dest(&rel(&format!("x64/{name}"))).unwrap();
        let read = roots.read_dest(&dest);
        match expect {
            Some(true) => {
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut read.unwrap().unwrap(), &mut data).unwrap();
                assert_eq!(data, original, "{name}");
            }
            Some(false) => assert!(read.unwrap_err().to_string().contains("leads outside"), "{name}"),
            None => assert!(read.unwrap().is_none(), "{name}"),
        }
    }
}

#[test]
fn originals_are_read_through_any_link() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let drive2 = dir.path().join("drive2");
    std::fs::create_dir_all(&drive2).unwrap();
    std::fs::rename(game.root.join("x64"), drive2.join("x64_real")).unwrap();
    symlink(drive2.join("x64_real"), game.root.join("x64")).unwrap();
    symlink("../../drive2/x64_real/a.rpf", game.root.join("update/link.rpf")).unwrap();
    let roots = game.roots().unwrap();
    for path in ["X64/A.rpf", "update/link.rpf"] {
        let file = roots.original(&rel(path)).unwrap().unwrap_or_else(|| panic!("{path}"));
        assert!(file.metadata().unwrap().len() > 0, "{path}");
    }
    assert!(roots.original(&rel("x64/missing.rpf")).unwrap().is_none());
    assert!(roots.original(&rel("scripts/orig.asi/x")).unwrap().is_none());
    assert!(roots.original(&rel("scripts")).unwrap().is_none(), "a folder is not a file");
    // The mods copy is made of it, and is a file of its own.
    let copy = roots.mods_copy(&rel("x64/a.rpf")).unwrap().unwrap();
    assert!(std::fs::symlink_metadata(&copy).unwrap().is_file());
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(drive2.join("x64_real/a.rpf")).unwrap());
    assert_eq!(roots.mods_copy(&rel("x64/none.rpf")).unwrap(), None);
}

#[test]
fn remove_unlinks_the_name_only_if_the_file_is_the_one_found() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let scripts = dir.path().join("game/scripts");
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::write(scripts.join("a.asi"), b"old").unwrap();
    let dest = roots.dest(&rel("scripts/a.asi")).unwrap();
    std::fs::write(scripts.join("a.asi"), b"changed meanwhile, longer").unwrap();
    assert!(err(roots.remove(&dest).map(|()| 0)).contains("changed while"));
    assert!(scripts.join("a.asi").exists());
    let dest = roots.dest(&rel("scripts/a.asi")).unwrap();
    roots.remove(&dest).unwrap();
    assert!(!scripts.join("a.asi").exists());
    symlink(&outside, scripts.join("l.asi")).unwrap();
    roots.remove(&roots.dest(&rel("scripts/l.asi")).unwrap()).unwrap();
    assert!(!scripts.join("l.asi").exists() && outside.exists(), "the link goes, not its target");
    roots.remove(&roots.dest(&rel("scripts/none.asi")).unwrap()).unwrap();
    assert!(roots.remove(&roots.dest(&rel("scripts")).unwrap()).is_err());
}

#[test]
fn prune_empty_stops_at_the_first_folder_with_something_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    std::fs::create_dir_all(game.join("plugins/a/b")).unwrap();
    std::fs::write(game.join("plugins/keep.txt"), b"k").unwrap();
    let dest = roots.dest(&rel("plugins/a/b/x.cfg")).unwrap();
    roots.prune_empty(&dest).unwrap();
    assert!(!game.join("plugins/a").exists() && game.join("plugins/keep.txt").exists());
}

#[test]
fn fingerprints_tell_files_apart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    let big = vec![7u8; 300 << 10];
    std::fs::write(&path, &big).unwrap();
    let fp = |p: &Path| Fingerprint::of(&std::fs::File::open(p).unwrap()).unwrap();
    let a = fp(&path);
    assert_eq!(a, fp(&path));
    let mut changed = big.clone();
    changed[big.len() - 1] = 8;
    std::fs::write(&path, &changed).unwrap();
    assert_ne!(a.head_tail, fp(&path).head_tail, "the tail counts");
    std::fs::hard_link(&path, dir.path().join("g")).unwrap();
    assert!(fp(&path).same_file(&fp(&dir.path().join("g"))));
    std::fs::write(dir.path().join("h"), b"").unwrap();
    assert!(!fp(&path).same_file(&fp(&dir.path().join("h"))));
    assert_eq!(fp(&dir.path().join("h")).size, 0);
}

#[test]
fn an_original_that_is_a_pipe_or_leads_to_one_is_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let fifo = std::ffi::CString::new(game.root.join("x64/pipe.rpf").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    symlink("pipe.rpf", game.root.join("x64/linkpipe.rpf")).unwrap();
    symlink("/dev/null", game.root.join("x64/null.rpf")).unwrap();
    let roots = game.roots().unwrap();
    for name in ["pipe.rpf", "linkpipe.rpf", "null.rpf"] {
        assert!(roots.original(&rel(&format!("x64/{name}"))).unwrap().is_none(), "{name}");
        assert_eq!(roots.mods_copy(&rel(&format!("x64/{name}"))).unwrap(), None, "{name}");
    }
    assert!(roots.original(&rel("x64/a.rpf")).unwrap().is_some());
}

#[test]
fn the_games_own_dll_and_txt_files_are_kept_but_mod_loaders_and_new_files_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    for name in ["steam_api64.dll", "Bink2w64.DLL", "version.txt", "commandline.txt", "dinput8.dll", "ScriptHookV.dll", "ScriptHookVDotNet3.dll", "ScriptHookVDotNet.ini", "OpenIV.asi", "readme.md"] {
        std::fs::write(game.join(name), b"orig").unwrap();
    }
    std::fs::write(game.join("scripts/x.dll"), b"orig").unwrap();
    symlink("steam_api64.dll", game.join("link.dll")).unwrap();
    for name in ["steam_api64.dll", "STEAM_API64.DLL", "Bink2w64.DLL", "version.txt", "commandline.txt", "link.dll"] {
        let mut dest = roots.dest(&rel(name)).unwrap();
        let e = err(roots.replace(&mut dest, |f| f.write_all(b"new")));
        assert!(e.contains("one of the game's own files in the game folder"), "{name}: {e}");
        let e = err(roots.remove(&dest).map(|()| 0));
        assert!(e.contains("one of the game's own files"), "{name}: {e}");
        let e = err(roots.check_write(&dest));
        assert!(e.contains("one of the game's own files"), "{name}: {e}");
    }
    for name in ["steam_api64.dll", "Bink2w64.DLL", "version.txt", "commandline.txt"] {
        assert_eq!(std::fs::read(game.join(name)).unwrap(), b"orig", "{name}");
    }
    assert!(std::fs::symlink_metadata(game.join("link.dll")).unwrap().is_symlink());
    // Mod loaders, plugins and other kinds of file may be replaced or deleted.
    for name in ["dinput8.dll", "Dinput8.DLL", "ScriptHookV.dll", "ScriptHookVDotNet3.dll", "ScriptHookVDotNet.ini", "OpenIV.asi", "readme.md", "scripts/x.dll"] {
        write(&roots, name, b"new").unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    roots.remove(&roots.dest(&rel("OpenIV.asi")).unwrap()).unwrap();
    // A new file is no file of the game's.
    write(&roots, "foo.dll", b"new").unwrap();
    write(&roots, "notes.txt", b"new").unwrap();
    assert_eq!(std::fs::read(game.join("foo.dll")).unwrap(), b"new");
    // Not a program's reach either way: the mods folder has its own rules.
    write(&roots, "update/x.dll", b"new").unwrap();
}

#[test]
fn a_hard_linked_destination_is_replaced_not_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let roots = open(dir.path());
    let game = dir.path().join("game");
    std::fs::hard_link(game.join("scripts/orig.asi"), game.join("scripts/twin.asi")).unwrap();
    write(&roots, "scripts/twin.asi", b"new").unwrap();
    assert_eq!(std::fs::read(game.join("scripts/orig.asi")).unwrap(), b"orig");
    assert_eq!(std::fs::read(game.join("scripts/twin.asi")).unwrap(), b"new");
}

#[test]
fn a_game_folder_reached_through_a_link_is_confined_by_its_real_place() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real_lib");
    std::fs::create_dir_all(&real).unwrap();
    game_folder(&real, Edition::Enhanced);
    symlink("real_lib", dir.path().join("lib")).unwrap();
    let roots = GameRoots::open(&dir.path().join("lib/game")).unwrap();
    symlink(real.join("game/scripts"), real.join("game/real_abs")).unwrap();
    symlink(dir.path().join("lib/game/scripts"), real.join("game/shown_abs")).unwrap();
    symlink(dir.path().join("lib/game/../game/scripts"), real.join("game/dots_abs")).unwrap();
    for via in ["real_abs", "shown_abs"] {
        write(&roots, &format!("{via}/a.asi"), via.as_bytes()).unwrap_or_else(|e| panic!("{via}: {e}"));
    }
    assert_eq!(std::fs::read(real.join("game/scripts/a.asi")).unwrap(), b"shown_abs");
    assert!(err(write(&roots, "dots_abs/a.asi", b"x")).contains("leads outside"), "a `..` after the root's path is not trusted");
    symlink(dir.path().join("lib"), real.join("game/up")).unwrap();
    assert!(write(&roots, "up/game/scripts/b.asi", b"x").is_err(), "a link to the folder that holds the game leaves it");
}
