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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, ArchiveEditor, Keys, StoredFile};

use crate::export::{Kind, locate, read_in};
use crate::fsroot::{Dir, GameRoots, Kind as Entry, Owned, Resolved};
use crate::game::{Game, resolve_ci};
use crate::loader::{asi_plugins, setup};
use crate::relpath::RelPath;
use crate::txn::{self, Staged, Txn, TxnFailure};
use crate::vfs::{prune_empty, scratch_file, transact};
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

/// The folder `mods_disabled` of the game, made if need be, never entered through a link.
fn disabled_folder(roots: &GameRoots, names: &[&str], create: bool) -> Result<Option<Dir>, Error> {
    roots.owned_folder("mods_disabled", names, create)
}

/// A file of `mods_disabled` (the names below it), for a transaction to write or delete.
fn kept_dest(roots: &GameRoots, names: &[&str]) -> Result<Resolved, Error> {
    roots.owned_dest(Owned::Disabled, &RelPath::parse(&names.join("/"))?)
}

fn save(txn: &mut Txn<'_>, roots: &GameRoots, hunt: &Hunt) -> Result<(), Error> {
    let mut text = format!("steps {}\n", hunt.steps);
    if let Some(top) = &hunt.inside {
        text += &format!("inside {top}\n");
    }
    // In order, so that the same hunt always writes the same file.
    let kinds: BTreeMap<&Part, &Kind> = hunt.kinds.iter().collect();
    for (p, kind) in kinds {
        text += &format!("kind {} {p}\n", format!("{kind:?}").to_ascii_lowercase());
    }
    for p in &hunt.suspects {
        text += &format!("suspect {p}\n");
    }
    for p in &hunt.disabled {
        text += &format!("disabled {p}\n");
    }
    txn.stage(&mut kept_dest(roots, &[STATE])?, |f| f.write_all(text.as_bytes())).map(drop)
}

/// Forgets the hunt (the file that says one is going on).
fn remove_state(txn: &mut Txn<'_>, roots: &GameRoots) -> Result<(), Error> {
    txn.delete(&kept_dest(roots, &[STATE])?)
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
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "starting a crash hunt")?;
    if load(game).is_some() {
        return Err(Error::Package("a hunt is already going on: answer it or stop it".into()));
    }
    let suspects = suspects(game);
    if suspects.is_empty() {
        return Err(Error::Package("there are no plugins, add-on packs or modified archives to turn off".into()));
    }
    next(game, &roots, None, Hunt { suspects, disabled: Vec::new(), steps: 0, inside: None, kinds: HashMap::new() })
}

/// Starts a hunt among the changed files of an archive of the mods folder
/// ("mods/update/update.rpf", as a part), once the archive is the culprit.
/// The changes are read with the cache kept in `cache` (see
/// `export::changed_files_cached_at`).
pub fn start_inside(game: &Game, keys: Option<&Keys>, part: &str, cache: &Path) -> Result<Outcome, Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "starting a crash hunt inside an archive")?;
    if load(game).is_some() {
        return Err(Error::Package("a hunt is already going on: answer it or stop it".into()));
    }
    let top = part.strip_prefix("mods/").ok_or_else(|| Error::Package(format!("{part} is not in the mods folder")))?.to_string();
    // The culprit found by the first hunt is turned off: back on, to look inside.
    enable(&roots, part)?;
    let changes = crate::export::changed_files_cached_at(game, keys, cache, &|| false)?;
    let prefix = format!("{}/", top.to_ascii_lowercase());
    let inner: Vec<&crate::export::Changed> =
        changes.iter().filter(|c| c.path.to_ascii_lowercase().starts_with(&prefix) && c.kind != Kind::Pack).collect();
    if inner.len() < 2 {
        return Err(Error::Package(format!("{top} holds {} changed files: nothing to look for inside", inner.len())));
    }
    let suspects = inner.iter().map(|c| c.path.clone()).collect();
    let kinds = inner.iter().map(|c| (c.path.clone(), c.kind)).collect();
    next(game, &roots, keys, Hunt { suspects, disabled: Vec::new(), steps: 0, inside: Some(top), kinds })
}

/// Takes the answer to the current test: whether the game still crashed.
pub fn answer(game: &Game, keys: Option<&Keys>, crashed: bool) -> Result<Outcome, Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "answering a crash hunt")?;
    let mut hunt = load(game).ok_or_else(|| Error::Package("no hunt is going on".into()))?;
    // A step that failed after it moved parts (see `next`) left the state as it was: do not go on from a state that
    // is not what is on disk.
    if hunt.inside.is_none() {
        let (mut on_disk, mut said) = (turned_off(game), hunt.disabled.clone());
        on_disk.sort();
        said.sort();
        if on_disk != said {
            return Err(Error::Package("the last step was interrupted after it moved some parts, so the hunt no longer knows which are off: stop it (everything goes back on) and start again".into()));
        }
    }
    let off = &hunt.disabled;
    // Still crashing: the culprit is among the parts that were on. Not
    // crashing: it is among those turned off.
    hunt.suspects.retain(|p| off.contains(p) != crashed);
    hunt.steps += 1;
    if hunt.suspects.is_empty() {
        stop_in(game, &roots, keys)?;
        return Err(Error::Package(
            "the answers do not fit a single culprit among the parts (several mods together, or something else crashes the game): everything is back on".into(),
        ));
    }
    next(game, &roots, keys, hunt)
}

fn is_pack(part: &str) -> bool {
    part.starts_with("mods/update/x64/dlcpacks/")
}

/// Turns everything back on and, with one suspect left, reports it (turned
/// off); otherwise turns off half of the suspects for the next test.
///
/// What is inside an archive of the mods folder (the archive, the files kept aside, the state of the hunt)
/// changes in one transaction. A part moved to or from `mods_disabled/` is one `renameat2` that never replaces
/// anything (atomic and lossless, but not part of the transaction: there is no move operation in it): if the
/// transaction fails after parts were moved, the error says so and they stay moved, which `stop` puts back.
fn next(game: &Game, roots: &GameRoots, keys: Option<&Keys>, hunt: Hunt) -> Result<Outcome, Error> {
    let mut moved = Vec::new();
    transact(roots, "turning mods on and off for a crash hunt", keys, |txn| next_in(txn, game, roots, keys, hunt, &mut moved)).map_err(|e| after_moves(e, &moved))
}

fn next_in(txn: &mut Txn<'_>, game: &Game, roots: &GameRoots, keys: Option<&Keys>, mut hunt: Hunt, moved: &mut Vec<String>) -> Result<(Outcome, Vec<(Staged, String)>), Error> {
    let back_on = std::mem::take(&mut hunt.disabled);
    if let [culprit] = hunt.suspects.as_slice() {
        let culprit = culprit.clone();
        let mut staged = Vec::new();
        match &hunt.inside {
            Some(top) => staged = switch_inside(txn, game, roots, keys, top, &back_on, std::slice::from_ref(&culprit), &hunt.kinds)?,
            None => {
                turn_on(roots, back_on, moved)?;
                disable(roots, &culprit)?;
                moved.push(format!("{culprit} was turned off"));
            }
        }
        remove_state(txn, roots)?;
        return Ok((Outcome::Found(culprit), staged));
    }
    // update.rpf goes last, so it is never in the half turned off: with it
    // off, the add-on packs would be off too, unknown to the test. It is
    // found by elimination, as the last suspect left on.
    hunt.suspects.sort_by_key(|p| (p == UPDATE, is_pack(p), p.clone()));
    let half: Vec<Part> = hunt.suspects[..hunt.suspects.len() / 2].to_vec();
    let mut staged = Vec::new();
    match &hunt.inside {
        Some(top) => staged = switch_inside(txn, game, roots, keys, top, &back_on, &half, &hunt.kinds)?,
        None => {
            turn_on(roots, back_on, moved)?;
            for p in &half {
                disable(roots, p)?;
                moved.push(format!("{p} was turned off"));
            }
        }
    }
    hunt.disabled = half;
    save(txn, roots, &hunt)?;
    Ok((Outcome::Test(hunt), staged))
}

/// Moves parts back from `mods_disabled/`, noting each in `moved`.
fn turn_on(roots: &GameRoots, parts: Vec<Part>, moved: &mut Vec<String>) -> Result<(), Error> {
    for p in parts {
        enable(roots, &p)?;
        moved.push(format!("{p} was turned on"));
    }
    Ok(())
}

/// `e`, said honestly after parts were moved (see [`next`]): the changes of the transaction were undone or not, and
/// what was moved is still where it was moved to.
fn after_moves(e: Error, moved: &[String]) -> Error {
    if moved.is_empty() {
        return e;
    }
    let left = moved.iter().cloned();
    match e {
        Error::Txn(mut f) => {
            // A clean undo left no journal for the next run to finish.
            f.journaled &= !f.undone;
            f.undone = false;
            f.failures.extend(left);
            Error::Txn(f)
        }
        cause => Error::Txn(Box::new(TxnFailure { cause, log: Vec::new(), undone: false, failures: left.collect(), journaled: false, verb: "changed" })),
    }
}

/// Ends the hunt: turns every part it turned off back on (the culprit too).
pub fn stop(game: &Game, keys: Option<&Keys>) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "stopping a crash hunt")?;
    stop_in(game, &roots, keys)
}

fn stop_in(game: &Game, roots: &GameRoots, keys: Option<&Keys>) -> Result<(), Error> {
    let dir = disabled_dir(game);
    if !dir.is_dir() {
        return Ok(());
    }
    let mut moved = Vec::new();
    transact(roots, "stopping a crash hunt", keys, |txn| {
        let staged = restore_inside(txn, game, roots, keys)?;
        for (path, _) in crate::vfs::local_files(&dir).unwrap_or_default() {
            let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
            if rel == STATE || rel.starts_with(&format!("{INSIDE}/")) {
                continue;
            }
            // Packs are folders: put back the folder holding the file.
            let part = if is_pack(&rel) { rel.splitn(6, '/').take(5).collect::<Vec<_>>().join("/") } else { rel };
            if dir.join(&part).exists() {
                enable(roots, &part)?;
                moved.push(format!("{part} was turned on"));
            }
        }
        remove_state(txn, roots)?;
        Ok(((), staged))
    })
    .map_err(|e| after_moves(e, &moved))?;
    // Only the folders left empty go: whatever else is in there is not this hunt's to delete.
    if let Ok(Some(dir)) = roots.owned_folder("mods_disabled", &[], false) {
        let _ = prune_empty(&dir, INSIDE, 0);
    }
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

/// Turns files inside an archive of the mods folder off (`off`: the game's own put back, the changed
/// one kept aside) or back on (`restore`), in `txn`: the archive is staged once as a new file (from the mods copy), the
/// files kept aside are staged or deleted with it. A file in both lists stays as it is (turned off, with the
/// changed one still aside: that is what turning it on and off again gives).
#[allow(clippy::too_many_arguments)]
fn switch_inside(txn: &mut Txn<'_>, game: &Game, roots: &GameRoots, keys: Option<&Keys>, top: &str, restore: &[Part], off: &[Part], kinds: &HashMap<Part, Kind>) -> Result<Vec<(Staged, String)>, Error> {
    let back: Vec<&Part> = restore.iter().filter(|p| !off.contains(p)).collect();
    let away: Vec<&Part> = off.iter().filter(|p| !restore.contains(p)).collect();
    if back.is_empty() && away.is_empty() {
        return Ok(Vec::new());
    }
    let rel = RelPath::parse(top)?;
    let mut dest = roots.mods_dest(&rel)?;
    let base = roots.read_dest(&dest)?.ok_or_else(|| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{top} is not in the mods folder"))))?;
    let len = base.metadata()?.len();
    let original = game.original(top);
    let mut editor = ArchiveEditor::open_file(base, rel.name(), keys)?;
    for (item, off) in back.into_iter().map(|p| (p, false)).chain(away.into_iter().map(|p| (p, true))) {
        let kind = kinds.get(item).copied().unwrap_or(Kind::Modified);
        switch_item(txn, roots, &mut editor, &original, keys, item, kind, off)?;
    }
    let name = rel.name().to_string();
    let staged = txn.stage_sized(&mut dest, len, move |out| editor.save_to(out, &mut scratch_file).map(drop).map_err(std::io::Error::other))?;
    Ok(vec![(staged, name)])
}

/// One file of [`switch_inside`].
#[allow(clippy::too_many_arguments)]
fn switch_item(txn: &mut Txn<'_>, roots: &GameRoots, editor: &mut ArchiveEditor, original: &Path, keys: Option<&Keys>, item: &str, kind: Kind, off: bool) -> Result<(), Error> {
    let (_, nested, file) = locate(item).ok_or_else(|| Error::Package(format!("{item} is not inside an archive")))?;
    let rel = RelPath::parse(item)?;
    let (name, folders) = rel.parts().split_last().expect("a path has a name");
    let folders: Vec<&str> = std::iter::once(INSIDE).chain(folders.iter().map(String::as_str)).collect();
    let marker = format!("{name}{DELETED}");
    let kept = |leaf: &str| kept_dest(roots, &folders.iter().copied().chain([leaf]).collect::<Vec<_>>());
    let ed = crate::convert::inner(editor, &nested, keys)?;
    if off {
        // Files move as stored (still compressed and encrypted), never
        // decoded: scripts and NG-encrypted files cannot be encrypted again.
        match kind {
            Kind::Deleted => {
                put_back(ed, original, keys, &nested, &file)?;
                txn.stage(&mut kept(&marker)?, |_| Ok(()))?;
            }
            Kind::Added => {
                let stored = ed.stored(&file)?.to_bytes();
                txn.stage(&mut kept(name)?, |f| f.write_all(&stored))?;
                ed.delete(&file)?;
            }
            _ => {
                let changed = ed.stored(&file)?.to_bytes();
                put_back(ed, original, keys, &nested, &file)?;
                txn.stage(&mut kept(name)?, |f| f.write_all(&changed))?;
            }
        }
    } else if let Some(dir) = roots.owned_folder("mods_disabled", &folders, false)? {
        if dir.kind(&marker)? == Some(Entry::File) {
            ed.delete(&file)?;
            txn.delete(&kept(&marker)?)?;
        } else if dir.kind(name)? == Some(Entry::File) {
            let mut aside = Vec::new();
            dir.open_read(name)?.read_to_end(&mut aside)?;
            match StoredFile::from_bytes(&aside)? {
                Some(stored) => ed.add_stored(&file, stored)?,
                // Kept aside as the standalone file by an older version.
                None => ed.add_file(&file, aside)?,
            }
            txn.delete(&kept(name)?)?;
        }
    }
    Ok(())
}

/// Puts the game's own `file` back in `ed`, as stored. Decoded and added again
/// only when its encryption cannot be copied (from an AES archive into a
/// mods copy that is not, say).
fn put_back(ed: &mut ArchiveEditor, original: &Path, keys: Option<&Keys>, nested: &[String], file: &str) -> Result<(), Error> {
    match ed.add_stored(file, stored_in(original, keys, nested, file)?) {
        Err(paleto_rpf::Error::Unsupported(_)) => Ok(ed.add_file(file, read_in(original, keys, nested, file)?)?),
        done => Ok(done?),
    }
}

/// A file inside an archive on disk (nested archives named in `nested`), as stored.
fn stored_in(disk: &Path, keys: Option<&Keys>, nested: &[String], file: &str) -> Result<StoredFile, Error> {
    let mut archive = Archive::open(disk, keys)?;
    for n in nested {
        let idx = archive.find(n).ok_or_else(|| Error::Package(format!("{n} not found in {}", disk.display())))?;
        archive = archive.open_child(idx, keys)?;
    }
    let idx = archive.find(file).ok_or_else(|| Error::Package(format!("{file} not found in {}", disk.display())))?;
    Ok(archive.stored(idx)?)
}

/// Puts back every file a hunt inside an archive turned off, in `txn`.
fn restore_inside(txn: &mut Txn<'_>, game: &Game, roots: &GameRoots, keys: Option<&Keys>) -> Result<Vec<(Staged, String)>, Error> {
    let kept = disabled_dir(game).join(INSIDE);
    if !kept.is_dir() {
        return Ok(Vec::new());
    }
    let mut by_archive: BTreeMap<String, Vec<Part>> = BTreeMap::new();
    for (path, _) in crate::vfs::local_files(&kept)? {
        let item = path.strip_prefix(&kept).unwrap().to_string_lossy().replace('\\', "/");
        let item = item.trim_end_matches(DELETED).to_string();
        if let Some((top, _, _)) = locate(&item) {
            by_archive.entry(top).or_default().push(item);
        }
    }
    let mut staged = Vec::new();
    for (top, items) in by_archive {
        staged.extend(switch_inside(txn, game, roots, keys, &top, &items, &[], &HashMap::new())?);
    }
    Ok(staged)
}

/// Turns a part (see [`parts`]) off or back on, outside a hunt: it is moved to
/// `mods_disabled/` and back. Refused while a hunt is going on, which owns that folder.
pub fn set_enabled(game: &Game, part: &str, enabled: bool) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, if enabled { "turning a mod on" } else { "turning a mod off" })?;
    if load(game).is_some() {
        return Err(Error::Package("a hunt is going on: answer it or stop it first".into()));
    }
    let known = if enabled { turned_off(game) } else { parts(game) };
    if !known.iter().any(|p| p == part) {
        return Err(Error::Package(format!("{part} is not a mod that can be turned {}", if enabled { "on" } else { "off" })));
    }
    if enabled { enable(&roots, part) } else { disable(&roots, part) }
}

/// Where a part is kept when it is off: the folder in `mods_disabled/` and the name in it.
fn disabled_place(roots: &GameRoots, rel: &RelPath, create: bool) -> Result<Option<(Dir, String)>, Error> {
    let (name, folders) = rel.parts().split_last().expect("a path has a name");
    let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
    Ok(disabled_folder(roots, &folders, create)?.map(|dir| (dir, name.clone())))
}

fn disable(roots: &GameRoots, part: &str) -> Result<(), Error> {
    let rel = RelPath::parse(part)?;
    let from = roots.dest(&rel)?;
    if from.existing.is_none() {
        return Ok(());
    }
    let (to_dir, to_name) = disabled_place(roots, &rel, true)?.expect("made above");
    let shown = roots.game.shown.join("mods_disabled").join(part);
    move_part(&from.parent, &from.name, &to_dir, &to_name, &shown)
}

fn enable(roots: &GameRoots, part: &str) -> Result<(), Error> {
    let rel = RelPath::parse(part)?;
    let Some((from_dir, from_name)) = disabled_place(roots, &rel, false)? else { return Ok(()) };
    if from_dir.kind(&from_name)?.is_none() {
        return Ok(());
    }
    let mut to = roots.dest(&rel)?;
    roots.ensure_parent(&mut to)?;
    move_part(&from_dir, &from_name, &to.parent, &to.name, &to.path())
}

/// Moves a part from one folder to another of the same game, refusing to replace anything.
fn move_part(from_dir: &Dir, from: &str, to_dir: &Dir, to: &str, shown: &Path) -> Result<(), Error> {
    match from_dir.rename_into(from, to_dir, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(libc::ENOTEMPTY) => Err(Error::Package(format!("{} is in the way", shown.display()))),
        Err(e) => Err(e.into()),
    }
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
        disable(&game.roots().unwrap(), "mods/update/update.rpf").unwrap();
        let Outcome::Test(_) = start_inside(&game, None, "mods/update/update.rpf", &cache).unwrap() else { panic!() };
        stop(&game, None).unwrap();
        assert_eq!(contents(&root.join("mods/update/update.rpf")), modded);
    }

    /// Every file of an archive as stored: scripts stay encrypted, so no keys are needed.
    fn stored_contents(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let a = paleto_rpf::Archive::open(path, None).unwrap();
        a.walk(0).into_iter().filter(|(_, i)| !a.entries[*i].is_dir()).map(|(p, i)| (p, a.read_stored(i).unwrap())).collect()
    }

    /// A resource (RSC7) of `len` bytes, its payload filled from `seed`.
    fn resource(len: usize, seed: u8) -> Vec<u8> {
        let mut data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)).collect();
        data[..4].copy_from_slice(b"RSC7");
        data[4..8].copy_from_slice(&10u32.to_le_bytes());
        data[8..12].copy_from_slice(&0x0000_0001u32.to_le_bytes());
        data[12..16].copy_from_slice(&0xA000_0002u32.to_le_bytes());
        data
    }

    /// Saves `ed` and renames its `.ytd` files `.ysc` in the names table: scripts
    /// cannot be added to an OPEN archive, but the game's archives hold them.
    fn save_with_scripts(ed: ArchiveEditor, path: &Path) {
        ed.save(path).unwrap();
        let mut data = std::fs::read(path).unwrap();
        let word = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let (start, len) = (16 + 16 * word(4), word(8));
        let names = &mut data[start..start + len];
        let at: Vec<usize> = names.windows(5).enumerate().filter(|(_, w)| *w == b".ytd\0").map(|(k, _)| k).collect();
        assert!(!at.is_empty());
        for k in at {
            names[k..k + 5].copy_from_slice(b".ysc\0");
        }
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn looks_inside_an_open_archive_with_scripts() {
        // OPEN archives, as the mods copy of an NG archive is: scripts in them
        // are NG-encrypted, cannot be encrypted again, and are moved as stored.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("update")).unwrap();
        std::fs::create_dir_all(root.join("mods/update")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let mut update = ArchiveEditor::new("update.rpf");
        update.add_file("common/data/a.meta", b"stock".to_vec()).unwrap();
        update.add_file("scripts/x.ytd", resource(256, 1)).unwrap();
        update.add_file("scripts/gone.ytd", resource(128, 2)).unwrap();
        save_with_scripts(update, &root.join("update/update.rpf"));
        let mut update = ArchiveEditor::new("update.rpf");
        update.add_file("common/data/a.meta", b"mod a".to_vec()).unwrap();
        update.add_file("common/data/b.meta", b"new".to_vec()).unwrap();
        // Another size: without keys, changes are found by size or stored bytes.
        update.add_file("scripts/x.ytd", resource(384, 3)).unwrap();
        update.add_file("scripts/new.ytd", resource(160, 4)).unwrap();
        save_with_scripts(update, &root.join("mods/update/update.rpf"));
        let game = Game::open(&root).unwrap();
        let original = stored_contents(&root.join("update/update.rpf"));
        let modded = stored_contents(&root.join("mods/update/update.rpf"));
        let cache = dir.path().join("changes.tsv");
        let suspects: Vec<String> = crate::export::changed_files(&game, None).unwrap().into_iter().map(|c| c.path).collect();
        for script in ["gone.ysc", "new.ysc", "x.ysc"] {
            assert!(suspects.contains(&format!("update/update.rpf/scripts/{script}")), "{suspects:?}");
        }
        assert_eq!(suspects.len(), 5, "{suspects:?}");

        for culprit in &suspects {
            let mut outcome = start_inside(&game, None, "mods/update/update.rpf", &cache).unwrap();
            let found = loop {
                match outcome {
                    Outcome::Found(p) => break p,
                    Outcome::Test(hunt) => {
                        // The files turned off hold the game's own bytes, as stored (or are gone, if added).
                        let now = stored_contents(&root.join("mods/update/update.rpf"));
                        for p in &hunt.disabled {
                            let inner = p.strip_prefix("update/update.rpf/").unwrap();
                            assert_eq!(now.get(inner), original.get(inner), "{p} not turned off");
                        }
                        outcome = answer(&game, None, !hunt.disabled.contains(culprit)).unwrap();
                    }
                }
            };
            assert_eq!(&found, culprit);
            assert_eq!(turned_off(&game), std::slice::from_ref(culprit));
            stop(&game, None).unwrap();
            assert_eq!(stored_contents(&root.join("mods/update/update.rpf")), modded, "not restored after {culprit}");
            assert!(turned_off(&game).is_empty());
        }
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
