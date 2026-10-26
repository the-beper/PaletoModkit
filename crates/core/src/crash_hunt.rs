//! Finding the mod that makes the game crash, by halves: half of the
//! suspects are turned off, the user starts the game and says whether it
//! still crashes, and the suspects shrink to the half that holds the culprit.
//!
//! A part is turned off by moving it to the game folder's `mods_disabled/`
//! (same relative path), and back. The hunt's state is kept there too, so it
//! survives closing the app between game runs. It assumes one culprit.
//!
//! Once the culprit is an archive of the mods folder holding many changes
//! (update.rpf often holds several mods), a second hunt looks inside it: a
//! changed file is turned off by putting the game's own back in the archive,
//! the changed one kept in `mods_disabled/inside/`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use paleto_rpf::{ArchiveEditor, Keys};

use crate::export::{Kind, locate, read_in};
use crate::game::{Game, resolve_ci};
use crate::loader::{asi_plugins, setup};
use crate::Error;

/// Where the files turned off inside an archive are kept.
const INSIDE: &str = "inside";
/// Marks a file the mods copy had deleted, put back for a test.
const DELETED: &str = ".paleto-deleted";

/// A part of the setup that can be turned off: an `.asi` plugin
/// ("Trainer.asi"), a Script Hook V .NET script ("scripts/Mod.dll"), an add-on pack ("mods/update/x64/dlcpacks/car") or an
/// archive of the mods folder ("mods/x64e.rpf"); paths relative to the game folder.
pub type Part = String;

/// The mods copy of update.rpf holds the DLC list: with it off, the game
/// reads the original list, which names no add-on pack, so they are off too.
const UPDATE: &str = "mods/update/update.rpf";

const STATE: &str = "crash-hunt.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunt {
    /// The parts that may hold the culprit.
    pub suspects: Vec<Part>,
    /// The parts turned off for the current test.
    pub disabled: Vec<Part>,
    /// How many answers so far.
    pub steps: u32,
    /// When looking inside an archive: its path in the mods folder
    /// ("update/update.rpf"), and how each suspect file changed.
    pub inside: Option<String>,
    pub kinds: HashMap<Part, Kind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Start the game again, with `disabled` turned off.
    Test(Hunt),
    /// The culprit, left turned off; everything else is back on.
    Found(Part),
}

pub fn disabled_dir(game: &Game) -> PathBuf {
    game.root.join("mods_disabled")
}

/// The parts that can be turned off, by path.
pub fn parts(game: &Game) -> Vec<Part> {
    let loader = setup(game).plugin;
    let mods = game.mods_dir();
    let mut out: Vec<Part> = asi_plugins(game).into_iter().map(|p| p.name).filter(|n| !n.eq_ignore_ascii_case(loader)).collect();
    // SHVDN scripts: compiled or source, directly in the scripts folder.
    let scripts = resolve_ci(&game.root, "scripts");
    let folder = scripts.file_name().map_or_else(|| "scripts".to_string(), |n| n.to_string_lossy().into_owned());
    for entry in std::fs::read_dir(&scripts).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let script = Path::new(&name).extension().is_some_and(|e| ["dll", "cs", "vb"].iter().any(|x| e.eq_ignore_ascii_case(x)));
        if script && entry.path().is_file() {
            out.push(format!("{folder}/{name}"));
        }
    }
    let packs = resolve_ci(&game.mods_dir(), "update/x64/dlcpacks");
    for entry in std::fs::read_dir(&packs).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            out.push(format!("mods/update/x64/dlcpacks/{}", entry.file_name().to_string_lossy()));
        }
    }
    let files = if mods.is_dir() { crate::vfs::local_files(&mods).unwrap_or_default() } else { Vec::new() };
    for (path, _) in files {
        let rel = path.strip_prefix(&game.root).unwrap().to_string_lossy().replace('\\', "/");
        if rel.to_ascii_lowercase().ends_with(".rpf") && !rel.to_ascii_lowercase().starts_with("mods/update/x64/dlcpacks/") {
            out.push(rel);
        }
    }
    out.sort();
    out
}

/// The hunt in progress, if any.
pub fn load(game: &Game) -> Option<Hunt> {
    let text = std::fs::read_to_string(disabled_dir(game).join(STATE)).ok()?;
    let mut hunt = Hunt { suspects: Vec::new(), disabled: Vec::new(), steps: 0, inside: None, kinds: HashMap::new() };
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("steps", n)) => hunt.steps = n.parse().unwrap_or(0),
            Some(("suspect", p)) => hunt.suspects.push(p.to_string()),
            Some(("disabled", p)) => hunt.disabled.push(p.to_string()),
            Some(("inside", p)) => hunt.inside = Some(p.to_string()),
            Some(("kind", rest)) => {
                if let Some((kind, p)) = rest.split_once(' ') {
                    let kind = match kind {
                        "added" => Kind::Added,
                        "deleted" => Kind::Deleted,
                        _ => Kind::Modified,
                    };
                    hunt.kinds.insert(p.to_string(), kind);
                }
            }
            _ => {}
        }
    }
    Some(hunt)
}

fn save(game: &Game, hunt: &Hunt) -> Result<(), Error> {
    let mut text = format!("steps {}\n", hunt.steps);
    if let Some(top) = &hunt.inside {
        text += &format!("inside {top}\n");
    }
    for (p, kind) in &hunt.kinds {
        text += &format!("kind {} {p}\n", format!("{kind:?}").to_ascii_lowercase());
    }
    for p in &hunt.suspects {
        text += &format!("suspect {p}\n");
    }
    for p in &hunt.disabled {
        text += &format!("disabled {p}\n");
    }
    std::fs::create_dir_all(disabled_dir(game))?;
    std::fs::write(disabled_dir(game).join(STATE), text)?;
    Ok(())
}

/// The parts that change the game: copies in the mods folder identical to
/// the game's own files cannot crash it and are left out. This reads the
/// copies of game files whole (tens of GB take tens of seconds).
pub fn suspects(game: &Game) -> Vec<Part> {
    parts(game).into_iter().filter(|p| !p.starts_with("mods/") || !same_as_original(game, p)).collect()
}

/// Whether a part of the mods folder (a file, or a pack's folder) holds only
/// exact copies of the game's files.
fn same_as_original(game: &Game, part: &str) -> bool {
    let modded = game.root.join(part);
    let original = game.original(part.trim_start_matches("mods/"));
    if modded.is_file() {
        return crate::game::same_file(&modded, &original);
    }
    let files = crate::vfs::local_files(&modded).unwrap_or_default();
    !files.is_empty() && files.iter().all(|(p, _)| crate::game::same_file(p, &original.join(p.strip_prefix(&modded).unwrap())))
}

/// Starts a hunt among the parts that change the game: turns off the first half.
pub fn start(game: &Game) -> Result<Outcome, Error> {
    if load(game).is_some() {
        return Err(Error::Package("a hunt is already going on: answer it or stop it".into()));
    }
    let suspects = suspects(game);
    if suspects.is_empty() {
        return Err(Error::Package("there are no plugins, add-on packs or modified archives to turn off".into()));
    }
    next(game, None, Hunt { suspects, disabled: Vec::new(), steps: 0, inside: None, kinds: HashMap::new() })
}

/// Starts a hunt among the changed files of an archive of the mods folder
/// ("mods/update/update.rpf", as a part), once the archive is the culprit.
/// The changes are read with the cache kept in `cache` (see
/// `export::changed_files_cached_at`).
pub fn start_inside(game: &Game, keys: Option<&Keys>, part: &str, cache: &Path) -> Result<Outcome, Error> {
    if load(game).is_some() {
        return Err(Error::Package("a hunt is already going on: answer it or stop it".into()));
    }
    let top = part.strip_prefix("mods/").ok_or_else(|| Error::Package(format!("{part} is not in the mods folder")))?.to_string();
    // The culprit found by the first hunt is turned off: back on, to look inside.
    enable(game, part)?;
    let changes = crate::export::changed_files_cached_at(game, keys, cache)?;
    let prefix = format!("{}/", top.to_ascii_lowercase());
    let inner: Vec<&crate::export::Changed> =
        changes.iter().filter(|c| c.path.to_ascii_lowercase().starts_with(&prefix) && c.kind != Kind::Pack).collect();
    if inner.len() < 2 {
        return Err(Error::Package(format!("{top} holds {} changed files: nothing to look for inside", inner.len())));
    }
    let suspects = inner.iter().map(|c| c.path.clone()).collect();
    let kinds = inner.iter().map(|c| (c.path.clone(), c.kind)).collect();
    next(game, keys, Hunt { suspects, disabled: Vec::new(), steps: 0, inside: Some(top), kinds })
}

/// Takes the answer to the current test: whether the game still crashed.
pub fn answer(game: &Game, keys: Option<&Keys>, crashed: bool) -> Result<Outcome, Error> {
    let mut hunt = load(game).ok_or_else(|| Error::Package("no hunt is going on".into()))?;
    let off = &hunt.disabled;
    // Still crashing: the culprit is among the parts that were on. Not
    // crashing: it is among those turned off.
    hunt.suspects.retain(|p| off.contains(p) != crashed);
    hunt.steps += 1;
    if hunt.suspects.is_empty() {
        stop(game, keys)?;
        return Err(Error::Package(
            "the answers do not fit a single culprit among the parts (several mods together, or something else crashes the game): everything is back on".into(),
        ));
    }
    next(game, keys, hunt)
}

fn is_pack(part: &str) -> bool {
    part.starts_with("mods/update/x64/dlcpacks/")
}

/// Turns everything back on and, with one suspect left, reports it (turned
/// off); otherwise turns off half of the suspects for the next test.
fn next(game: &Game, keys: Option<&Keys>, mut hunt: Hunt) -> Result<Outcome, Error> {
    let back_on = std::mem::take(&mut hunt.disabled);
    if let Some(top) = &hunt.inside {
        switch_inside(game, keys, top, &back_on, &hunt.kinds, false)?;
    } else {
        for p in back_on {
            enable(game, &p)?;
        }
    }
    if let [culprit] = hunt.suspects.as_slice() {
        let culprit = culprit.clone();
        match &hunt.inside {
            Some(top) => switch_inside(game, keys, top, std::slice::from_ref(&culprit), &hunt.kinds, true)?,
            None => disable(game, &culprit)?,
        }
        let _ = std::fs::remove_file(disabled_dir(game).join(STATE));
        return Ok(Outcome::Found(culprit));
    }
    // update.rpf goes last, so it is never in the half turned off: with it
    // off, the add-on packs would be off too, unknown to the test. It is
    // found by elimination, as the last suspect left on.
    hunt.suspects.sort_by_key(|p| (p == UPDATE, is_pack(p), p.clone()));
    let half: Vec<Part> = hunt.suspects[..hunt.suspects.len() / 2].to_vec();
    match &hunt.inside {
        Some(top) => switch_inside(game, keys, top, &half, &hunt.kinds, true)?,
        None => {
            for p in &half {
                disable(game, p)?;
            }
        }
    }
    hunt.disabled = half;
    save(game, &hunt)?;
    Ok(Outcome::Test(hunt))
}

/// Ends the hunt: turns every part it turned off back on (the culprit too).
pub fn stop(game: &Game, keys: Option<&Keys>) -> Result<(), Error> {
    let dir = disabled_dir(game);
    if !dir.is_dir() {
        return Ok(());
    }
    restore_inside(game, keys)?;
    for (path, _) in crate::vfs::local_files(&dir).unwrap_or_default() {
        let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
        if rel == STATE || rel.starts_with(&format!("{INSIDE}/")) {
            continue;
        }
        // Packs are folders: put back the folder holding the file.
        let part = if is_pack(&rel) { rel.splitn(6, '/').take(5).collect::<Vec<_>>().join("/") } else { rel };
        if dir.join(&part).exists() {
            enable(game, &part)?;
        }
    }
    let _ = std::fs::remove_file(dir.join(STATE));
    Ok(())
}

/// The parts turned off now (by a hunt, or the culprit it left off).
pub fn turned_off(game: &Game) -> Vec<Part> {
    let dir = disabled_dir(game);
    let mut out: Vec<Part> = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for (path, _) in crate::vfs::local_files(&dir).unwrap_or_default() {
        let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
        if rel == STATE {
            continue;
        }
        if let Some(inner) = rel.strip_prefix(&format!("{INSIDE}/")) {
            out.push(inner.trim_end_matches(DELETED).to_string());
            continue;
        }
        let part = if is_pack(&rel) { rel.splitn(6, '/').take(5).collect::<Vec<_>>().join("/") } else { rel };
        if !out.contains(&part) {
            out.push(part);
        }
    }
    out
}

/// Turns files inside an archive of the mods folder off (`off`: the game's
/// own put back, the changed one kept aside) or back on. One save of the archive.
fn switch_inside(game: &Game, keys: Option<&Keys>, top: &str, items: &[Part], kinds: &HashMap<Part, Kind>, off: bool) -> Result<(), Error> {
    if items.is_empty() {
        return Ok(());
    }
    let disk = game.mods_dir().join(top);
    let original = game.original(top);
    let kept = disabled_dir(game).join(INSIDE);
    let mut editor = ArchiveEditor::open(&disk, keys)?;
    for item in items {
        let (_, nested, file) = locate(item).ok_or_else(|| Error::Package(format!("{item} is not inside an archive")))?;
        let kind = kinds.get(item).copied().unwrap_or(Kind::Modified);
        let aside = kept.join(item);
        let marker = kept.join(format!("{item}{DELETED}"));
        let ed = crate::convert::inner(&mut editor, &nested, keys)?;
        if off {
            std::fs::create_dir_all(aside.parent().unwrap())?;
            // What is kept aside is written once the archive took the change
            // (scripts, for one, can be refused), so a failure leaves no trace.
            match kind {
                Kind::Deleted => {
                    ed.add_file(&file, read_in(&original, keys, &nested, &file)?)?;
                    std::fs::write(&marker, b"")?;
                }
                Kind::Added => {
                    std::fs::write(&aside, ed.read(&file, keys)?)?;
                    ed.delete(&file)?;
                }
                _ => {
                    let changed = ed.read(&file, keys)?;
                    ed.add_file(&file, read_in(&original, keys, &nested, &file)?)?;
                    std::fs::write(&aside, changed)?;
                }
            }
        } else if marker.is_file() {
            ed.delete(&file)?;
            std::fs::remove_file(&marker)?;
        } else if aside.is_file() {
            ed.add_file(&file, std::fs::read(&aside)?)?;
            std::fs::remove_file(&aside)?;
        }
    }
    editor.save(&disk)?;
    Ok(())
}

/// Puts back every file a hunt inside an archive turned off.
fn restore_inside(game: &Game, keys: Option<&Keys>) -> Result<(), Error> {
    let kept = disabled_dir(game).join(INSIDE);
    if !kept.is_dir() {
        return Ok(());
    }
    let mut by_archive: BTreeMap<String, Vec<Part>> = BTreeMap::new();
    for (path, _) in crate::vfs::local_files(&kept)? {
        let item = path.strip_prefix(&kept).unwrap().to_string_lossy().replace('\\', "/");
        let item = item.trim_end_matches(DELETED).to_string();
        if let Some((top, _, _)) = locate(&item) {
            by_archive.entry(top).or_default().push(item);
        }
    }
    for (top, items) in by_archive {
        switch_inside(game, keys, &top, &items, &HashMap::new(), false)?;
    }
    let _ = std::fs::remove_dir_all(&kept);
    Ok(())
}

/// Turns a part (see [`parts`]) off or back on, outside a hunt: it is moved to
/// `mods_disabled/` and back. Refused while a hunt is going on, which owns that folder.
pub fn set_enabled(game: &Game, part: &str, enabled: bool) -> Result<(), Error> {
    if load(game).is_some() {
        return Err(Error::Package("a hunt is going on: answer it or stop it first".into()));
    }
    let known = if enabled { turned_off(game) } else { parts(game) };
    if !known.iter().any(|p| p == part) {
        return Err(Error::Package(format!("{part} is not a mod that can be turned {}", if enabled { "on" } else { "off" })));
    }
    if enabled { enable(game, part) } else { disable(game, part) }
}

fn disable(game: &Game, part: &str) -> Result<(), Error> {
    move_part(&game.root.join(part), &disabled_dir(game).join(part))
}

fn enable(game: &Game, part: &str) -> Result<(), Error> {
    move_part(&disabled_dir(game).join(part), &game.root.join(part))
}

fn move_part(from: &Path, to: &Path) -> Result<(), Error> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        return Err(Error::Package(format!("{} is in the way", to.display())));
    }
    std::fs::create_dir_all(to.parent().unwrap())?;
    std::fs::rename(from, to)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_game(dir: &Path) -> Game {
        let root = dir.join("game");
        for f in ["GTA5_Enhanced.exe", "OpenRPF.asi", "Trainer.asi", "Fixes.asi"] {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(f), b"").unwrap();
        }
        for f in ["mods/x64e.rpf", "mods/update/update.rpf", "mods/update/x64/dlcpacks/car/dlc.rpf", "mods/update/x64/dlcpacks/bus/dlc.rpf"] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), b"").unwrap();
        }
        Game::open(&root).unwrap()
    }

    /// Plays the hunt against a culprit: the game crashes while it is on.
    fn hunt_for(game: &Game, culprit: &str) -> Part {
        let mut outcome = start(game).unwrap();
        for _ in 0..10 {
            match outcome {
                Outcome::Found(p) => return p,
                Outcome::Test(hunt) => {
                    assert!(!hunt.disabled.iter().any(|p| p == UPDATE), "update.rpf off with the packs counted on: {hunt:?}");
                    // update.rpf off would turn the packs off too.
                    let off = &hunt.disabled;
                    for p in &hunt.disabled {
                        assert!(!game.root.join(p).exists() && disabled_dir(game).join(p).exists(), "{p} not moved");
                    }
                    outcome = answer(game, None, !off.iter().any(|p| p == culprit)).unwrap();
                }
            }
        }
        panic!("no culprit found");
    }

    fn parts_all() -> Vec<Part> {
        ["Fixes.asi", "Trainer.asi", "mods/update/update.rpf", "mods/update/x64/dlcpacks/bus", "mods/update/x64/dlcpacks/car", "mods/x64e.rpf"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn finds_each_culprit() {
        let dir = tempfile::tempdir().unwrap();
        let game = setup_game(dir.path());
        assert_eq!(parts(&game), parts_all());
        for culprit in parts_all() {
            assert_eq!(hunt_for(&game, &culprit), culprit);
            // The culprit is left off, everything else is back.
            assert_eq!(turned_off(&game), std::slice::from_ref(&culprit));
            assert!(load(&game).is_none());
            stop(&game, None).unwrap();
            assert!(turned_off(&game).is_empty());
            for p in parts_all() {
                assert!(game.root.join(&p).exists(), "{p} not back");
            }
        }
    }

    #[test]
    fn leaves_out_exact_copies() {
        let dir = tempfile::tempdir().unwrap();
        let game = setup_game(dir.path());
        // A game pack copied as it is, and one changed in place (same size).
        for (pack, data) in [("mpbeach", b"same".as_slice()), ("mpheist", b"edit")] {
            std::fs::create_dir_all(game.root.join(format!("update/x64/dlcpacks/{pack}"))).unwrap();
            std::fs::write(game.root.join(format!("update/x64/dlcpacks/{pack}/dlc.rpf")), b"same").unwrap();
            std::fs::create_dir_all(game.root.join(format!("mods/update/x64/dlcpacks/{pack}"))).unwrap();
            std::fs::write(game.root.join(format!("mods/update/x64/dlcpacks/{pack}/dlc.rpf")), data).unwrap();
        }
        assert!(parts(&game).iter().any(|p| p.ends_with("mpbeach")));
        let s = suspects(&game);
        assert!(!s.iter().any(|p| p.ends_with("mpbeach")) && s.iter().any(|p| p.ends_with("mpheist")), "{s:?}");
    }

    #[test]
    fn stops_midway() {
        let dir = tempfile::tempdir().unwrap();
        let game = setup_game(dir.path());
        assert!(turned_off(&game).is_empty());
        stop(&game, None).unwrap();
        let Outcome::Test(hunt) = start(&game).unwrap() else { panic!() };
        assert!(!hunt.disabled.is_empty());
        assert!(start(&game).is_err(), "one hunt at a time");
        assert_eq!(load(&game), Some(hunt));
        stop(&game, None).unwrap();
        assert!(load(&game).is_none() && turned_off(&game).is_empty());
        for p in parts_all() {
            assert!(game.root.join(&p).exists(), "{p} not back");
        }
    }

    /// Every file of an archive (nested ones too) with its content.
    fn contents(path: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(a: &paleto_rpf::Archive, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
            for (p, i) in a.walk(0) {
                let e = &a.entries[i];
                if e.is_archive() {
                    walk(&a.open_child(i, None).unwrap(), &format!("{prefix}{p}/"), out);
                } else if !e.is_dir() {
                    out.insert(format!("{prefix}{p}"), a.read(i, None).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&paleto_rpf::Archive::open(path, None).unwrap(), "", &mut out);
        out
    }

    #[test]
    fn looks_inside_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("update")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let mut update = ArchiveEditor::new("update.rpf");
        for f in ["a.meta", "b.meta", "c.meta", "gone.meta"] {
            update.add_file(&format!("common/data/{f}"), b"stock".to_vec()).unwrap();
        }
        update.archive("x64/levels.rpf", true, None).unwrap().add_file("map.ymap", b"stock".to_vec()).unwrap();
        update.save(&root.join("update/update.rpf")).unwrap();
        let mut update = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
        update.add_file("common/data/a.meta", b"mod a".to_vec()).unwrap();
        update.add_file("common/data/b.meta", b"mod b".to_vec()).unwrap();
        update.add_file("common/data/new.meta", b"new".to_vec()).unwrap();
        update.delete("common/data/gone.meta").unwrap();
        update.archive("x64/levels.rpf", false, None).unwrap().add_file("map.ymap", b"mod map".to_vec()).unwrap();
        std::fs::create_dir_all(root.join("mods/update")).unwrap();
        update.save(&root.join("mods/update/update.rpf")).unwrap();
        let game = Game::open(&root).unwrap();
        let modded = contents(&root.join("mods/update/update.rpf"));
        let cache = dir.path().join("changes.tsv");
        let changes = crate::export::changed_files(&game, None).unwrap();
        let suspects: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
        assert_eq!(suspects.len(), 5, "{suspects:?}");

        for culprit in &suspects {
            let mut outcome = start_inside(&game, None, "mods/update/update.rpf", &cache).unwrap();
            let found = loop {
                match outcome {
                    Outcome::Found(p) => break p,
                    Outcome::Test(hunt) => {
                        // The files turned off hold the game's own content (or are gone, if added).
                        let now = contents(&root.join("mods/update/update.rpf"));
                        for p in &hunt.disabled {
                            let inner = p.strip_prefix("update/update.rpf/").unwrap();
                            assert_ne!(now.get(inner), modded.get(inner), "{p} not turned off");
                        }
                        outcome = answer(&game, None, !hunt.disabled.contains(culprit)).unwrap();
                    }
                }
            };
            assert_eq!(&found, culprit);
            assert_eq!(turned_off(&game), std::slice::from_ref(culprit));
            stop(&game, None).unwrap();
            assert_eq!(contents(&root.join("mods/update/update.rpf")), modded, "not restored after {culprit}");
            assert!(turned_off(&game).is_empty());
        }
        // Looking inside the archive the first hunt left off puts it back first.
        disable(&game, "mods/update/update.rpf").unwrap();
        let Outcome::Test(_) = start_inside(&game, None, "mods/update/update.rpf", &cache).unwrap() else { panic!() };
        stop(&game, None).unwrap();
        assert_eq!(contents(&root.join("mods/update/update.rpf")), modded);
    }

    #[test]
    fn script_mods_are_parts() {
        let dir = tempfile::tempdir().unwrap();
        let game = setup_game(dir.path());
        std::fs::create_dir_all(game.root.join("scripts/sub")).unwrap();
        for f in ["Mod.dll", "Other.CS", "third.vb", "notes.txt", "sub/Deep.dll"] {
            std::fs::write(game.root.join("scripts").join(f), b"").unwrap();
        }
        let scripts = ["scripts/Mod.dll", "scripts/Other.CS", "scripts/third.vb"];
        let all = parts(&game);
        assert!(scripts.iter().all(|s| all.iter().any(|p| p == s)) && !all.iter().any(|p| p.contains("notes") || p.contains("Deep")), "{all:?}");
        assert!(scripts.iter().all(|s| suspects(&game).iter().any(|p| p == s)));
        for culprit in scripts {
            assert_eq!(hunt_for(&game, culprit), culprit);
            assert_eq!(turned_off(&game), [culprit]);
            stop(&game, None).unwrap();
            assert!(game.root.join(culprit).exists() && turned_off(&game).is_empty());
        }
    }

    #[test]
    fn turns_a_part_off_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let game = setup_game(dir.path());
        std::fs::create_dir_all(game.root.join("scripts")).unwrap();
        std::fs::write(game.root.join("scripts/Mod.dll"), b"").unwrap();
        for part in ["Trainer.asi", "scripts/Mod.dll", "mods/update/x64/dlcpacks/car", "mods/x64e.rpf"] {
            set_enabled(&game, part, false).unwrap();
            assert!(!game.root.join(part).exists() && turned_off(&game) == [part]);
            set_enabled(&game, part, true).unwrap();
            assert!(game.root.join(part).exists() && turned_off(&game).is_empty());
        }
        let Outcome::Test(_) = start(&game).unwrap() else { panic!() };
        assert!(set_enabled(&game, "Trainer.asi", false).is_err() && set_enabled(&game, "Trainer.asi", true).is_err());
        stop(&game, None).unwrap();
    }
}
