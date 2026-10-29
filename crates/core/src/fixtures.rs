//! Helpers shared by the write-safety tests: synthetic game folders and
//! packages, folder snapshots and a leftover-name scan.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use paleto_rpf::ArchiveEditor;

use crate::fsroot::faults::{self, Mode as Fault};
use crate::game::{Edition, Game};
use crate::{Error, txn};

/// An archive at `path` holding `files`.
pub(crate) fn rpf(path: &Path, name: &str, files: &[(&str, &[u8])]) {
    let mut ed = ArchiveEditor::new(name);
    for (inner, data) in files {
        ed.add_file(inner, data.to_vec()).unwrap();
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    ed.save(path).unwrap();
}

/// An `update.rpf` whose DLC list names `packs`.
pub(crate) fn update_rpf(path: &Path, packs: &[&str]) {
    let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
    let list = format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n");
    rpf(path, "update.rpf", &[("common/data/dlclist.xml", list.as_bytes())]);
}

/// A `dlc.rpf` with a setup2.xml naming it `name_hash`.
pub(crate) fn dlc_pack(path: &Path, name_hash: &str) {
    let setup = format!("<SSetupData><nameHash>{name_hash}</nameHash></SSetupData>");
    rpf(path, "dlc.rpf", &[("setup2.xml", setup.as_bytes())]);
}

/// A game folder `dir/game`: the edition's executable, `update/update.rpf`
/// (DLC list with `mpheist`), `x64/a.rpf` (with `a.txt`) and `scripts/orig.asi`.
pub(crate) fn game_folder(dir: &Path, edition: Edition) -> Game {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(root.join(edition.exe_name()), b"exe").unwrap();
    update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
    rpf(&root.join("x64/a.rpf"), "a.rpf", &[("a.txt", b"a")]);
    std::fs::write(root.join("scripts/orig.asi"), b"orig").unwrap();
    Game::open(&root).unwrap()
}

/// A package `dir/name` with these commands and `content/` files.
pub(crate) fn oiv(dir: &Path, name: &str, commands: &str, files: &[(&str, &[u8])], deflate: bool) -> PathBuf {
    let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content>{commands}</content></package>"#);
    let entries = std::iter::once(("assembly.xml".to_string(), assembly.into_bytes())).chain(files.iter().map(|(n, d)| (n.to_string(), d.to_vec())));
    oiv_raw(dir, name, entries, deflate)
}

/// A zip `dir/name` of exactly these entries.
pub(crate) fn oiv_raw(dir: &Path, name: &str, entries: impl IntoIterator<Item = (String, Vec<u8>)>, deflate: bool) -> PathBuf {
    let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
    let opts = zip::write::SimpleFileOptions::default().compression_method(method);
    let path = dir.join(name);
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    for (entry, data) in entries {
        zip.start_file(entry, opts).unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap();
    path
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Dir,
    File(Vec<u8>),
    /// A symbolic link, by its target.
    Link(PathBuf),
}

/// Every name under `dir` (relative, '/' separated), links not followed, except the tool's own
/// `.paleto` folder at its top (the lock and the journals: not part of what a test compares).
pub(crate) fn snapshot(dir: &Path) -> BTreeMap<String, Entry> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if rel == ".paleto" {
                continue;
            }
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_symlink() {
                out.insert(rel, Entry::Link(std::fs::read_link(&path).unwrap()));
            } else if kind.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(base, &path, out);
            } else {
                out.insert(rel, Entry::File(std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

pub(crate) fn assert_unchanged(before: &BTreeMap<String, Entry>, after: &BTreeMap<String, Entry>) {
    let mut diffs = Vec::new();
    for (name, old) in before {
        match after.get(name) {
            None => diffs.push(format!("removed {name}")),
            Some(new) if new != old => diffs.push(format!("changed {name}")),
            Some(_) => {}
        }
    }
    diffs.extend(after.keys().filter(|n| !before.contains_key(*n)).map(|n| format!("added {n}")));
    assert!(diffs.is_empty(), "the folder changed: {diffs:?}");
}

/// Temporary names (`.paleto-*`, and the old `.paleto-tmp`, `.paleto-undo`, `.new`, `.partial`) under `dir`.
pub(crate) fn leftovers(dir: &Path) -> Vec<String> {
    snapshot(dir)
        .into_keys()
        .filter(|rel| {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            name.starts_with(".paleto-") || name.contains(".paleto-tmp") || name.contains(".paleto-undo") || name.ends_with(".new") || name.ends_with(".partial")
        })
        .collect()
}

/// Makes a folder read-only; writable again when dropped, so the temporary
/// folder can be removed.
pub(crate) struct ReadOnly(PathBuf);

impl ReadOnly {
    pub(crate) fn new(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
        ReadOnly(path.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

pub(crate) fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

// ---- the fault loop ----

/// The journals left in the game folder.
pub(crate) fn journals(game: &Game) -> Vec<String> {
    std::fs::read_dir(game.root.join(".paleto/journal")).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// Whether a journal of the game has a record of this kind.
pub(crate) fn journal_has(game: &Game, record: &str) -> bool {
    journals(game).iter().any(|n| std::fs::read_to_string(game.root.join(".paleto/journal").join(n)).unwrap_or_default().lines().any(|l| l.split_whitespace().next() == Some(record)))
}

pub(crate) fn state(game: &Game) -> BTreeMap<String, Entry> {
    snapshot(&game.root)
}

/// No journal and no temporary name left.
pub(crate) fn assert_clean(game: &Game, what: &str) {
    assert_eq!(journals(game), Vec::<String>::new(), "{what}: a journal is left");
    assert_eq!(leftovers(&game.root), Vec::<String>::new(), "{what}: temporary names are left");
}

pub(crate) fn without_temps(mut s: BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    s.retain(|k, _| !k.rsplit('/').next().unwrap().starts_with(".paleto-"));
    s
}

/// Fails with the names that differ, never with the contents (they are whole archives).
pub(crate) fn assert_same(what: &str, want: &BTreeMap<String, Entry>, got: &BTreeMap<String, Entry>) {
    let mut diffs: Vec<String> = want.iter().filter(|(k, v)| got.get(*k) != Some(*v)).map(|(k, _)| format!("{} {k}", if got.contains_key(k) { "changed" } else { "missing" })).collect();
    diffs.extend(got.keys().filter(|k| !want.contains_key(*k)).map(|k| format!("extra {k}")));
    assert!(diffs.is_empty(), "{what}: {diffs:?}");
}

/// A name of the tool's own (the top folders it keeps, `.mods.*`, a `.paleto-*` temporary): never forgiven as a leftover.
fn tool_name(rel: &str) -> bool {
    let top = rel.split('/').next().unwrap_or(rel);
    [".paleto", "mods_snapshots", "mods_disabled", "mods_legacy"].contains(&top) || top.starts_with(".mods.") || rel.split('/').any(|c| c.starts_with(".paleto-"))
}

/// `now` without the empty folders that are in neither `want` nor `old` and have no tool-owned name, nested ones
/// included: what a crash between journaling that a folder is made and journaling that it was leaves, since recovery
/// cannot prove such a folder is its own and keeps it. Everything else (a file, a link, a folder with something in it) stays.
pub(crate) fn without_leaked_dirs(mut now: BTreeMap<String, Entry>, want: &BTreeMap<String, Entry>, old: &BTreeMap<String, Entry>) -> BTreeMap<String, Entry> {
    loop {
        let leaked: Vec<String> = now.iter().filter(|(k, e)| **e == Entry::Dir && !want.contains_key(*k) && !old.contains_key(*k) && !tool_name(k) && !now.keys().any(|o| o.starts_with(&format!("{k}/")))).map(|(k, _)| k.clone()).collect();
        if leaked.is_empty() {
            return now;
        }
        for k in leaked {
            now.remove(&k);
        }
    }
}

/// An error that claims nothing false: the plain cause (nothing had been changed), or a failure that says the undo was complete.
pub(crate) fn honest(e: &Error) -> bool {
    let text = e.to_string();
    text.contains("nothing was installed") || text.contains("nothing was changed") || !matches!(e, Error::Txn(_))
}

/// Fails (or crashes at) the k-th system call of `op`, for every k until none is left to hit, and checks the
/// folder after each. Fail: `op` returns an honest error and nothing changed, or only its cleanup was hit and
/// recovery finishes it. Crash: `txn::recover` leaves every file old or new, as the journal says.
/// `setup` makes a fresh game folder in a directory, with what `op` and `check` need besides it (a package
/// path, say). `op` is the operation and must give the same result every time it runs unfaulted. `check(game,
/// context, what, done)` runs after each point, `done` telling whether the operation had reached every file;
/// it is where a test runs the operation again. Returns the number of points hit.
pub(crate) fn exhaust<C>(mode: Fault, setup: impl Fn(&Path) -> (Game, C), op: impl Fn(&Game, &C) -> Result<Vec<String>, Error>, check: impl Fn(&Game, &C, &str, bool)) -> u64 {
    let (old, new) = {
        let dir = tempfile::tempdir().unwrap();
        let (game, context) = setup(dir.path());
        let old = state(&game);
        op(&game, &context).unwrap_or_else(|e| panic!("the unfaulted operation failed: {e}"));
        assert_clean(&game, "unfaulted");
        let new = state(&game);
        assert!(old != new, "the operation changed nothing");
        (old, new)
    };
    let crash = matches!(mode, Fault::Crash);
    let (mut saw_error, mut saw_committed) = (false, false);
    for k in 1..20_000 {
        let dir = tempfile::tempdir().unwrap();
        let (game, context) = setup(dir.path());
        faults::arm(k, mode);
        let result = op(&game, &context);
        if !faults::disarm() {
            result.unwrap();
            assert_same("no fault", &new, &state(&game));
            assert!(saw_error, "the operation hit no fault point at all");
            assert!(!crash || saw_committed, "no crash happened after `committed`");
            return k - 1;
        }
        let what = format!("{mode:?} at {k}");
        let mut done = false;
        if crash {
            let committed = journal_has(&game, "committed");
            let report = txn::recover(&game).unwrap();
            assert!(report.failures.is_empty(), "{what}: {report:?}");
            let now = state(&game);
            // The journal may be gone too, if the crash was at the last call of the operation: then everything was in place.
            done = committed || now == new;
            let want = if done { &new } else { &old };
            assert_same(&format!("{what}: committed={committed}"), want, &without_leaked_dirs(now, want, &old));
            saw_committed |= done;
            saw_error |= !done;
        } else {
            match result {
                Err(e) => {
                    assert_unchanged(&old, &state(&game));
                    assert!(honest(&e), "{what}: {e}");
                    saw_error = true;
                }
                Ok(_) => {
                    // The fault hit the cleanup after the commit: the files are in place and recovery finishes it.
                    assert_same(&what, &new, &without_temps(state(&game)));
                    done = true;
                    txn::recover(&game).unwrap();
                }
            }
        }
        assert_clean(&game, &what);
        check(&game, &context, &what, done);
        assert_clean(&game, &what);
    }
    panic!("more than 20000 fault points")
}
