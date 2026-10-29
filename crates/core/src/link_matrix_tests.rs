//! OIV-02: the link matrix. Every kind of package command (and the add-on
//! installer) runs against a game folder whose `mods` folder, a folder inside
//! it, or a loose folder (`scripts`) is a link of some kind. A link that stays
//! inside the game or mods folder is followed; any other must refuse the command
//! before anything is written, which a snapshot of the game folder and of a
//! folder outside it, taken before and after, proves. Every case runs again with
//! openat2 switched off (an old kernel or a seccomp filter).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::fixtures::*;
use crate::fsroot::{Caps, faults};
use crate::game::{Edition, Game};
use crate::{Error, Package};

/// What goes wrong or right: a command (package commands as `assembly.xml` text, or the add-on installer).
enum Cmd {
    Oiv(String),
    Addon,
}

/// A package command in a zone (`x64`, `update` or `scripts`). `plain` ones work on
/// a game folder without links; the others (`<pso>` on a file that is no metadata
/// file) can only be checked for being refused.
fn commands(zone: &str) -> Vec<(&'static str, Cmd, bool)> {
    let oiv = |name, xml: String, plain| (name, Cmd::Oiv(xml), plain);
    if zone == "scripts" {
        return vec![
            oiv("add", r#"<add source="x">scripts\new.asi</add>"#.into(), true),
            oiv("delete", r#"<delete>scripts\t.txt</delete>"#.into(), true),
            oiv("text", r#"<text path="scripts\t.txt"><add>b</add></text>"#.into(), true),
            oiv("xml", r#"<xml path="scripts\f.xml"><add xpath="/a"><b/></add></xml>"#.into(), true),
            oiv("pso", r#"<pso path="scripts\t.pso"><remove xpath="/x"/></pso>"#.into(), false),
        ];
    }
    vec![
        oiv("add", format!(r#"<add source="x">{zone}\new.rpf</add>"#), true),
        oiv("add in a new folder", format!(r#"<add source="x">{zone}\sub\deeper\new.txt</add>"#), true),
        oiv("delete", format!(r#"<delete>{zone}\a.rpf</delete>"#), true),
        oiv("text", format!(r#"<text path="{zone}\list.txt"><add>b</add></text>"#), true),
        oiv("xml", format!(r#"<xml path="{zone}\f.xml"><add xpath="/a"><b/></add></xml>"#), true),
        oiv("pso", format!(r#"<pso path="{zone}\t.pso"><remove xpath="/x"/></pso>"#), false),
        oiv("archive", format!(r#"<archive path="{zone}\a.rpf"><add source="x">b.txt</add></archive>"#), true),
        oiv("archive, created", format!(r#"<archive path="{zone}\b.rpf" createIfNotExist="True"><add source="x">b.txt</add></archive>"#), true),
        oiv("defragmentation", format!(r#"<defragmentation archive="{zone}\a.rpf" />"#), true),
    ]
}

struct Scenario {
    name: &'static str,
    zone: &'static str,
    /// Also run the add-on installer.
    addon: bool,
    /// Whether the links lead out of the folder they must stay in.
    refused: bool,
    /// Plants the links; returns the game to install into.
    setup: fn(&Game, &Path) -> Game,
}

/// The files every command finds, in the game folder and (as bait) outside it.
fn world() -> (tempfile::TempDir, Game) {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let fill = |base: &Path, with_rpf: bool| {
        std::fs::create_dir_all(base).unwrap();
        std::fs::write(base.join("list.txt"), b"a\n").unwrap();
        std::fs::write(base.join("t.txt"), b"a\n").unwrap();
        std::fs::write(base.join("f.xml"), b"<a/>").unwrap();
        std::fs::write(base.join("t.pso"), b"not a metadata file").unwrap();
        if with_rpf {
            rpf(&base.join("a.rpf"), "a.rpf", &[("a.txt", b"a")]);
        }
    };
    fill(&game.root.join("x64"), true);
    fill(&game.root.join("update"), true);
    fill(&game.root.join("scripts"), false);
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    for name in ["x64_out", "update_out", "scripts_out"] {
        fill(&outside.join(name), true);
    }
    dlc_pack(&dir.path().join("dlc.rpf"), "piccolo");
    (dir, game)
}

fn run(game: &Game, dir: &Path, cmd: &Cmd) -> Result<Vec<String>, Error> {
    match cmd {
        Cmd::Oiv(xml) => {
            let oiv = oiv(dir, "x.oiv", xml, &[("content/x", b"new")], false);
            Package::open(&oiv)?.install_with(game, None)
        }
        Cmd::Addon => crate::addon::install_pack(game, None, &dir.join("dlc.rpf"), "piccolo", None),
    }
}

fn mods(game: &Game) -> PathBuf {
    std::fs::create_dir_all(game.root.join("mods")).unwrap();
    game.root.join("mods")
}

fn same(game: &Game, _: &Path) -> Game {
    game.clone()
}

const SCENARIOS: &[Scenario] = &[
    // Links inside the mods folder to the originals.
    Scenario { name: "mods/x64 -> ../x64", zone: "x64", addon: false, refused: true, setup: |g, _| { symlink("../x64", mods(g).join("x64")).unwrap(); g.clone() } },
    Scenario { name: "mods/x64 -> absolute path of the originals", zone: "x64", addon: false, refused: true, setup: |g, _| { symlink(g.root.join("x64"), mods(g).join("x64")).unwrap(); g.clone() } },
    Scenario { name: "mods/update -> ../update", zone: "update", addon: true, refused: true, setup: |g, _| { symlink("../update", mods(g).join("update")).unwrap(); g.clone() } },
    Scenario { name: "mods/x64 -> x64 (a loop)", zone: "x64", addon: false, refused: true, setup: |g, _| { symlink("x64", mods(g).join("x64")).unwrap(); g.clone() } },
    Scenario { name: "mods/x64 -> y, y -> x64", zone: "x64", addon: false, refused: true, setup: |g, _| { symlink("y", mods(g).join("x64")).unwrap(); symlink("x64", mods(g).join("y")).unwrap(); g.clone() } },
    Scenario { name: "mods/x64 -> a folder outside", zone: "x64", addon: false, refused: true, setup: |g, d| { symlink(d.join("outside/x64_out"), mods(g).join("x64")).unwrap(); g.clone() } },
    Scenario { name: "mods/update -> a folder outside", zone: "update", addon: true, refused: true, setup: |g, d| { symlink(d.join("outside/update_out"), mods(g).join("update")).unwrap(); g.clone() } },
    Scenario { name: "mods/x64 -> /nonexistent (dangling, outside)", zone: "x64", addon: false, refused: true, setup: |g, _| { symlink("/nonexistent/x64", mods(g).join("x64")).unwrap(); g.clone() } },
    // The mods folder itself leads into the originals.
    Scenario { name: "mods -> .", zone: "x64", addon: true, refused: true, setup: |g, _| { symlink(".", g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "mods -> x64", zone: "x64", addon: true, refused: true, setup: |g, _| { symlink("x64", g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "mods -> update", zone: "update", addon: true, refused: true, setup: |g, _| { symlink("update", g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "mods -> ..", zone: "x64", addon: true, refused: true, setup: |g, _| { symlink("..", g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "mods -> the game folder, absolute", zone: "update", addon: true, refused: true, setup: |g, _| { symlink(&g.root, g.root.join("mods")).unwrap(); g.clone() } },
    // Loose files in the game folder.
    Scenario { name: "scripts -> a folder outside", zone: "scripts", addon: false, refused: true, setup: |g, d| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink(d.join("outside/scripts_out"), g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> ../outside/scripts_out", zone: "scripts", addon: false, refused: true, setup: |g, _| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("../outside/scripts_out", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> x64", zone: "scripts", addon: false, refused: true, setup: |g, _| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("x64", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> update", zone: "scripts", addon: false, refused: true, setup: |g, _| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("update", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> mods", zone: "scripts", addon: false, refused: true, setup: |g, _| { mods(g); std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("mods", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> scripts (a loop)", zone: "scripts", addon: false, refused: true, setup: |g, _| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("scripts", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> /nonexistent (dangling, outside)", zone: "scripts", addon: false, refused: true, setup: |g, _| { std::fs::remove_dir_all(g.root.join("scripts")).unwrap(); symlink("/nonexistent/scripts", g.root.join("scripts")).unwrap(); g.clone() } },
    // Links that stay inside, or are the user's own arrangement of the folders.
    Scenario { name: "no links", zone: "x64", addon: true, refused: false, setup: same },
    Scenario { name: "no links (update)", zone: "update", addon: false, refused: false, setup: same },
    Scenario { name: "no links (scripts)", zone: "scripts", addon: false, refused: false, setup: same },
    Scenario { name: "mods -> <outside>/drive2/mods", zone: "x64", addon: true, refused: false, setup: |g, d| { std::fs::create_dir_all(d.join("drive2/mods")).unwrap(); symlink(d.join("drive2/mods"), g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "mods -> mods_profileA", zone: "update", addon: true, refused: false, setup: |g, _| { std::fs::create_dir_all(g.root.join("mods_profileA")).unwrap(); symlink("mods_profileA", g.root.join("mods")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> scripts_real", zone: "scripts", addon: false, refused: false, setup: |g, _| { std::fs::rename(g.root.join("scripts"), g.root.join("scripts_real")).unwrap(); symlink("scripts_real", g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "scripts -> scripts_real, absolute", zone: "scripts", addon: false, refused: false, setup: |g, _| { std::fs::rename(g.root.join("scripts"), g.root.join("scripts_real")).unwrap(); symlink(g.root.join("scripts_real"), g.root.join("scripts")).unwrap(); g.clone() } },
    Scenario { name: "a dangling link where a new file goes", zone: "x64", addon: false, refused: false, setup: |g, _| { std::fs::create_dir_all(g.root.join("mods/x64")).unwrap(); symlink("/nonexistent/z", g.root.join("mods/x64/new.rpf")).unwrap(); g.clone() } },
    Scenario { name: "a hard link in the mods folder to an original", zone: "x64", addon: false, refused: false, setup: |g, _| { std::fs::create_dir_all(g.root.join("mods/x64")).unwrap(); std::fs::hard_link(g.root.join("x64/a.rpf"), g.root.join("mods/x64/a.rpf")).unwrap(); g.clone() } },
    Scenario { name: "game folder reached through a Steam-style link (x64)", zone: "x64", addon: true, refused: false, setup: steam },
    Scenario { name: "game folder reached through a Steam-style link (scripts)", zone: "scripts", addon: false, refused: false, setup: steam },
];

/// The game moved to `real_lib/game` and reached as `lib/game`.
fn steam(g: &Game, d: &Path) -> Game {
    std::fs::create_dir_all(d.join("real_lib")).unwrap();
    std::fs::rename(&g.root, d.join("real_lib/game")).unwrap();
    symlink("real_lib", d.join("lib")).unwrap();
    Game::open(&d.join("lib/game")).unwrap()
}

fn check(s: &Scenario, name: &str, cmd: &Cmd, plain: bool) {
    let (dir, game) = world();
    let game = (s.setup)(&game, dir.path());
    let outside = dir.path().join("outside");
    let (before, before_out, before_data) = (snapshot(&game.root), snapshot(&outside), (snapshot(&game.root.join("x64")), snapshot(&game.root.join("update"))));
    let result = run(&game, dir.path(), cmd);
    let context = format!("{} / {name}", s.name);
    assert_unchanged(&before_out, &snapshot(&outside));
    if s.refused {
        let e = result.expect_err(&format!("{context}: was not refused")).to_string();
        assert!(e.contains("leads outside") || e.contains("unsafe path") || e.contains("link"), "{context}: refused for another reason: {e}");
        assert_unchanged(&before, &snapshot(&game.root));
        assert!(leftovers(&game.root).is_empty() && leftovers(&outside).is_empty(), "{context}: temporary files left");
    } else {
        if plain {
            result.unwrap_or_else(|e| panic!("{context}: {e}"));
        }
        assert_eq!(before_data, (snapshot(&game.root.join("x64")), snapshot(&game.root.join("update"))), "{context}: an original changed");
        assert_eq!(std::fs::read(game.root.join("GTA5_Enhanced.exe")).unwrap(), b"exe");
        assert!(leftovers(&game.root).is_empty(), "{context}: {:?}", leftovers(&game.root));
    }
}

fn matrix(refused: bool, fallback: bool) {
    let _caps = fallback.then(|| faults::caps(Caps { openat2: false, ..Caps::ALL }));
    let mut runs = 0;
    for s in SCENARIOS.iter().filter(|s| s.refused == refused) {
        for (name, cmd, plain) in commands(s.zone) {
            // A control that is not refused must not depend on the file `<pso>` cannot read.
            if !refused && !plain {
                continue;
            }
            check(s, name, &cmd, plain);
            runs += 1;
        }
        if s.addon {
            check(s, "add-on installer", &Cmd::Addon, true);
            runs += 1;
        }
    }
    assert!(runs > if refused { 60 } else { 30 }, "{runs}");
}

#[test]
fn links_that_lead_out_are_refused_by_every_command() {
    matrix(true, false);
}

#[test]
fn links_that_lead_out_are_refused_without_openat2() {
    matrix(true, true);
}

#[test]
fn links_that_stay_in_are_followed_by_every_command() {
    matrix(false, false);
}

#[test]
fn links_that_stay_in_are_followed_without_openat2() {
    matrix(false, true);
}

#[test]
fn followed_links_land_in_the_folder_they_lead_to() {
    let (dir, game) = world();
    let drive2 = dir.path().join("drive2/mods");
    std::fs::create_dir_all(&drive2).unwrap();
    symlink(&drive2, game.root.join("mods")).unwrap();
    let xml = r#"<add source="x">x64\new.rpf</add><archive path="x64\a.rpf"><add source="x">b.txt</add></archive><text path="update\list.txt"><add>b</add></text>"#;
    let before = snapshot(&game.root.join("x64"));
    run(&game, dir.path(), &Cmd::Oiv(xml.into())).unwrap();
    assert_unchanged(&before, &snapshot(&game.root.join("x64")));
    assert_eq!(std::fs::read(drive2.join("x64/new.rpf")).unwrap(), b"new");
    assert!(drive2.join("x64/a.rpf").is_file());
    assert_eq!(std::fs::read(drive2.join("update/list.txt")).unwrap(), b"a\nb\n");
    assert!(!game.root.join("x64/new.rpf").exists() && std::fs::symlink_metadata(game.root.join("mods")).unwrap().is_symlink());
}

fn install(game: &Game, dir: &Path, xml: &str) -> Result<Vec<String>, Error> {
    run(game, dir, &Cmd::Oiv(xml.into()))
}

#[test]
fn an_archive_destination_passes_what_any_write_passes() {
    let (dir, game) = world();
    let before = snapshot(&game.root);
    for path in ["new.exe", "scripts\\tool.EXE"] {
        let e = install(&game, dir.path(), &format!(r#"<archive path="{path}" createIfNotExist="True"><add source="x">a.txt</add></archive>"#)).unwrap_err().to_string();
        assert!(e.contains("programs are never replaced"), "{path}: {e}");
    }
    // An existing program is no archive to edit either.
    let e = install(&game, dir.path(), r#"<archive path="GTA5_Enhanced.exe"><add source="x">a.txt</add></archive>"#).unwrap_err().to_string();
    assert!(e.contains("programs are never replaced"), "{e}");
    assert_unchanged(&before, &snapshot(&game.root));
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn a_package_leaves_the_games_own_dll_and_txt_files_alone_and_installs_the_rest() {
    let (dir, game) = world();
    for name in ["steam_api64.dll", "version.txt", "commandline.txt", "dinput8.dll"] {
        std::fs::write(game.root.join(name), b"orig").unwrap();
    }
    let xml = r#"<add source="x">steam_api64.dll</add>
        <delete>version.txt</delete>
        <text path="commandline.txt"><add>-windowed</add></text>
        <add source="x">dinput8.dll</add>
        <add source="x">OpenIV.asi</add>
        <add source="x">foo.dll</add>
        <add source="x">scripts\new.dll</add>"#;
    let log = install(&game, dir.path(), xml).unwrap();
    let refusals: Vec<_> = log.iter().filter(|l| l.starts_with("warning: refusing")).collect();
    assert_eq!(refusals.len(), 3, "{log:?}");
    assert!(refusals.iter().all(|l| l.contains("one of the game's own files")), "{refusals:?}");
    for name in ["steam_api64.dll", "version.txt", "commandline.txt"] {
        assert_eq!(std::fs::read(game.root.join(name)).unwrap(), b"orig", "{name}");
    }
    for name in ["dinput8.dll", "OpenIV.asi", "foo.dll", "scripts/new.dll"] {
        assert_eq!(std::fs::read(game.root.join(name)).unwrap(), b"new", "{name}");
    }
    assert!(leftovers(&game.root).is_empty());
}

#[test]
fn the_mods_folder_is_no_place_for_loose_files_under_any_name() {
    let (dir, game) = world();
    std::fs::create_dir_all(game.root.join("mods_profileA")).unwrap();
    symlink("mods_profileA", game.root.join("mods")).unwrap();
    let before = snapshot(&game.root);
    for xml in [r#"<add source="x">mods_profileA\x.asi</add>"#, r#"<add source="x">MODS_PROFILEA\sub\x.asi</add>"#, r#"<text path="mods_profileA\t.txt" createIfNotExist="True"><add>b</add></text>"#] {
        let e = install(&game, dir.path(), xml).unwrap_err().to_string();
        assert!(e.contains("that is the mods folder"), "{xml}: {e}");
    }
    assert_unchanged(&before, &snapshot(&game.root));
    // `scripts` made another name for the mods folder.
    std::fs::remove_dir_all(game.root.join("scripts")).unwrap();
    symlink("mods_profileA", game.root.join("scripts")).unwrap();
    let before = snapshot(&game.root);
    let e = install(&game, dir.path(), r#"<add source="x">scripts\x.asi</add>"#).unwrap_err().to_string();
    assert!(e.contains("that is the mods folder"), "{e}");
    assert_unchanged(&before, &snapshot(&game.root));
    // Data still goes there, by the mods route.
    install(&game, dir.path(), r#"<add source="x">update\x.bin</add>"#).unwrap();
    assert_eq!(std::fs::read(game.root.join("mods_profileA/update/x.bin")).unwrap(), b"new");
}

#[test]
fn an_archive_named_under_mods_is_edited_in_place_of_the_mods_copy() {
    let (dir, game) = world();
    let original = snapshot(&game.root.join("update"));
    install(&game, dir.path(), r#"<archive path="mods\update\update.rpf"><add source="x">b.txt</add></archive>"#).unwrap();
    assert_unchanged(&original, &snapshot(&game.root.join("update")));
    assert!(!game.root.join("mods/mods").exists(), "a second mods folder was made inside mods");
    let edited = paleto_rpf::ArchiveEditor::open(&game.root.join("mods/update/update.rpf"), None).unwrap();
    assert_eq!(edited.read("b.txt", None).unwrap(), b"new");
    assert!(edited.read("common/data/dlclist.xml", None).is_ok(), "the copy was made from the original");
}
