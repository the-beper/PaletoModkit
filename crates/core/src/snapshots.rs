//! Snapshots of the mods folder: whole copies kept in the game folder's
//! `mods_snapshots/<time>/`, to go back to a state that worked.
//!
//! Copies use `std::fs::copy`, which on btrfs, XFS and bcachefs shares the
//! data with the original (a reflink): instant and taking no space until a
//! file changes. On other filesystems they are full copies.
//!
//! Every change runs under the game lock and takes no fixed name: a snapshot is
//! built in a folder of a unique name made for it in `mods_snapshots/` and renamed
//! to its final name; a restore copies into a unique folder beside `mods`, then
//! swaps with two renames recorded in a small journal in `.paleto/journal/`
//! (`<id>.restore`), so that one interrupted between them is put right at the next
//! change (see [`recover_restores`]). Only names a run made itself are ever deleted.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::fsroot::{Dir, GameRoots, Kind, random};
use crate::txn::{self, Pending, PendingState, Report};
use crate::{Error, Game};

/// The file inside a snapshot that holds its label.
const LABEL: &str = "snapshot.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Its folder name: the time it was taken, in seconds since 1970.
    pub id: String,
    pub label: String,
    pub dir: PathBuf,
}

impl Snapshot {
    pub fn time(&self) -> Option<SystemTime> {
        self.id.parse().ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
    }

    /// Its copy of the mods folder.
    fn mods(&self) -> PathBuf {
        self.dir.join("mods")
    }
}

pub fn snapshots_dir(game: &Game) -> PathBuf {
    game.root.join("mods_snapshots")
}

/// The snapshots of this game, newest first.
pub fn list(game: &Game) -> Vec<Snapshot> {
    let mut out: Vec<(u64, Snapshot)> = std::fs::read_dir(snapshots_dir(game))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let secs = id.parse::<u64>().ok()?;
            let dir = e.path();
            dir.join("mods").is_dir().then_some(())?;
            let label = std::fs::read_to_string(dir.join(LABEL)).unwrap_or_default().trim().to_string();
            Some((secs, Snapshot { id, label, dir }))
        })
        .collect();
    out.sort_by_key(|(secs, _)| std::cmp::Reverse(*secs));
    out.into_iter().map(|(_, s)| s).collect()
}

/// The `mods_snapshots` folder (a link to another drive is followed: the user chose it); `None` if
/// there is none and `create` is not asked.
fn snapshots_folder(roots: &GameRoots, create: bool) -> Result<Option<Dir>, Error> {
    let game = &roots.game.dir;
    match game.kind("mods_snapshots")? {
        None if !create => Ok(None),
        None => Ok(Some(game.open_or_make_dir("mods_snapshots")?)),
        Some(Kind::Dir | Kind::Link) => Ok(Some(game.open_dir_following("mods_snapshots")?)),
        Some(_) => Err(Error::Package(format!("{} is not a folder", snapshots_dir_of(roots).display()))),
    }
}

fn snapshots_dir_of(roots: &GameRoots) -> PathBuf {
    roots.game.shown.join("mods_snapshots")
}

/// Removes the folder `name` of `dir` with everything in it, one name at a time on open handles:
/// never through a link (a link inside is removed itself). Only for folders this program made.
fn remove_tree(dir: &Dir, name: &str, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other("folders nested more than 64 deep"));
    }
    let inner = dir.open_dir(name)?;
    for child in inner.names()? {
        match inner.kind(&child)? {
            Some(Kind::Dir) => remove_tree(&inner, &child, depth + 1)?,
            Some(_) => inner.unlink(&child)?,
            None => {}
        }
    }
    dir.rmdir(name)
}

/// Takes a snapshot of the mods folder.
pub fn create(game: &Game, label: &str) -> Result<Snapshot, Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "taking a snapshot")?;
    let mods = game.mods_dir();
    put_back(&roots)?;
    if !mods.is_dir() {
        return Err(Error::Package("there is no mods folder to save".into()));
    }
    let snaps = snapshots_folder(&roots, true)?.expect("made above");
    // Built under a name of its own, so a folder that is somebody else's is never touched.
    let (partial, temp) = snaps.mkdir_unique(".partial")?;
    let label = label.trim();
    let built = copy_tree(&mods, &snapshots_dir(game).join(&temp).join("mods")).and_then(|()| {
        let mut file = partial.create_excl(LABEL)?;
        file.write_all(label.as_bytes())?;
        Ok(file.sync_all()?)
    });
    match built.and_then(|()| publish(&snaps, &temp)) {
        Ok(secs) => {
            let _ = snaps.sync();
            Ok(Snapshot { id: secs.to_string(), label: label.to_string(), dir: snapshots_dir(game).join(secs.to_string()) })
        }
        Err(e) => {
            let _ = remove_tree(&snaps, &temp, 0);
            Err(e)
        }
    }
}

/// Renames the finished folder to the current second, or to the next free one (two snapshots in the same second).
fn publish(snaps: &Dir, temp: &str) -> Result<u64, Error> {
    let first = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for secs in first..first + 10_000 {
        let name = secs.to_string();
        if snaps.kind(&name)?.is_none() {
            match snaps.rename(temp, &name, false) {
                Ok(()) => return Ok(secs),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Err(Error::Package("no free snapshot name".into()))
}

/// Puts a snapshot's copy in place of the mods folder. The current mods
/// folder is replaced only once the copy is complete: it is copied into a folder
/// of its own beside `mods`, the old `mods` is renamed aside, the copy renamed to
/// `mods`, and the old one removed only after that worked. A restore that stops
/// between the renames is undone (the old folder put back) at the next change.
pub fn restore(game: &Game, id: &str) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "restoring a snapshot")?;
    let snapshot = list(game).into_iter().find(|s| s.id == id).ok_or_else(|| Error::Package(format!("no snapshot {id}")))?;
    put_back(&roots)?;
    let jdir = roots.tool_folder(&["journal"], true)?.expect("made above");
    let rid = format!("{:016x}", random());
    let (incoming, outgoing) = (incoming_name(&rid), outgoing_name(&rid));
    let jname = format!("{rid}.restore");
    let mut journal = jdir.create_excl(&jname)?;
    let begun = note(&mut journal, &format!("paleto-restore 1\nbegin {rid} {} {}\n", unix_now(), snapshot.id)).and_then(|()| jdir.sync());
    if let Err(e) = begun {
        let _ = jdir.unlink(&jname);
        return Err(e.into());
    }
    match swap(&roots, &snapshot, &mut journal, &incoming, &outgoing) {
        Ok(()) => {
            // The restore is done. The old folder is only garbage now: if it cannot be removed, the journal stays and the next change does it.
            match remove_old(&roots.game.dir, &outgoing) {
                Ok(()) => {
                    let _ = jdir.unlink(&jname);
                    let _ = jdir.sync();
                }
                Err(e) => eprintln!("paleto: the snapshot is restored, but the old mods folder could not be removed ({}): {e}", roots.game.shown.join(&outgoing).display()),
            }
            Ok(())
        }
        Err(e) => {
            // The same code that recovers after a crash puts the old folder back.
            let mut rep = Report::default();
            recover_restores(&roots, &jdir, &mut rep);
            if rep.failures.is_empty() {
                Err(e)
            } else {
                Err(Error::Package(format!("{e}; the mods folder could not be put back as it was: {}", rep.failures.join("; "))))
            }
        }
    }
}

fn incoming_name(rid: &str) -> String {
    format!(".paleto-{rid}-mods.new")
}

fn outgoing_name(rid: &str) -> String {
    format!(".paleto-{rid}-mods.old")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Appends a record to the restore journal and flushes it: it is written before the effect it announces.
fn note(journal: &mut File, line: &str) -> io::Result<()> {
    journal.write_all(line.as_bytes())?;
    journal.sync_data()
}

fn remove_old(game: &Dir, outgoing: &str) -> io::Result<()> {
    match game.kind(outgoing)? {
        Some(Kind::Dir) => remove_tree(game, outgoing, 0),
        _ => Ok(()),
    }
}

/// The copy and the two renames, each announced in the journal first.
fn swap(roots: &GameRoots, snapshot: &Snapshot, journal: &mut File, incoming: &str, outgoing: &str) -> Result<(), Error> {
    let game = &roots.game.dir;
    note(journal, &format!("incoming {incoming}\n"))?;
    let made = game.mkdir(incoming)?;
    copy_tree(&snapshot.mods(), &roots.game.shown.join(incoming))?;
    made.sync()?;
    let (dev, ino) = made.identity()?;
    note(journal, &format!("made {dev} {ino}\n"))?;
    if game.kind("mods")?.is_some() {
        note(journal, &format!("outgoing {outgoing}\n"))?;
        game.rename("mods", outgoing, false)?;
    }
    game.rename(incoming, "mods", false)?;
    game.sync()?;
    // What the files say decides at recovery; a record that cannot be written changes nothing.
    let _ = note(journal, "swapped\n");
    Ok(())
}

/// A restore of an older version that failed between its two renames left the mods folder only
/// in `.mods.replaced`: puts it back (a rename, nothing is deleted) rather than lose it.
fn put_back(roots: &GameRoots) -> Result<(), Error> {
    let game = &roots.game.dir;
    if game.kind("mods")?.is_none() && game.kind(".mods.replaced")? == Some(Kind::Dir) {
        game.rename(".mods.replaced", "mods", false)?;
    }
    Ok(())
}

// ---- the restore journal ----

/// What a restore journal says: the names the run made, as far as it got.
#[derive(Debug, Default)]
struct Restore {
    id: String,
    snapshot: String,
    incoming: Option<String>,
    /// The device and inode of the incoming folder, once it was complete.
    made: Option<(u64, u64)>,
    outgoing: Option<String>,
    swapped: bool,
}

/// Parses a restore journal; `Ok(None)` if nothing was ever recorded in it. Only whole lines count (a torn last line is ignored)
/// and the names must be the ones its own id gives, so a damaged or forged journal cannot point recovery at another name.
fn parse_restore(text: &str) -> Result<Option<Restore>, String> {
    let whole = text.rfind('\n').map_or("", |end| &text[..=end]);
    let mut lines = whole.lines();
    match lines.next() {
        None => return Ok(None),
        Some("paleto-restore 1") => {}
        Some(other) => return Err(format!("unknown header {other:?}")),
    }
    let mut r = Restore::default();
    for line in lines {
        let f: Vec<&str> = line.split(' ').collect();
        match f.as_slice() {
            ["begin", id, _, snapshot] if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) => (r.id, r.snapshot) = (id.to_string(), snapshot.to_string()),
            ["incoming", name] if r.id.len() == 16 && *name == incoming_name(&r.id) => r.incoming = Some(name.to_string()),
            ["made", dev, ino] => r.made = Some((dev.parse().map_err(|_| "bad record")?, ino.parse().map_err(|_| "bad record")?)),
            ["outgoing", name] if r.id.len() == 16 && *name == outgoing_name(&r.id) => r.outgoing = Some(name.to_string()),
            ["swapped"] => r.swapped = true,
            _ => return Err(format!("unreadable record {line:?}")),
        }
    }
    Ok((!r.id.is_empty()).then_some(r))
}

fn restore_journals(jdir: &Dir) -> Vec<String> {
    let mut names: Vec<String> = jdir.names().unwrap_or_default().into_iter().filter(|n| n.ends_with(".restore")).collect();
    names.sort();
    names
}

fn read_restore(jdir: &Dir, name: &str) -> Result<Option<Restore>, String> {
    let mut text = String::new();
    jdir.open_read(name).and_then(|f| f.take(1 << 16).read_to_string(&mut text)).map_err(|e| format!("cannot be read: {e}"))?;
    parse_restore(&text)
}

/// The ids of the restores that have a journal: their `.paleto-<id>-*` names are accounted for.
pub(crate) fn restore_ids(jdir: &Dir) -> Vec<String> {
    restore_journals(jdir).iter().filter_map(|n| read_restore(jdir, n).ok().flatten()).map(|r| r.id).collect()
}

/// Interrupted restores, for [`txn::pending`].
pub(crate) fn pending_restores(roots: &GameRoots, jdir: &Dir) -> Vec<Pending> {
    let mut out = Vec::new();
    for name in restore_journals(jdir) {
        match read_restore(jdir, &name) {
            Ok(None) => {}
            Ok(Some(r)) => {
                let files = [r.incoming.as_deref(), r.outgoing.as_deref()].into_iter().flatten().map(|n| roots.game.shown.join(n)).collect();
                out.push(Pending { label: format!("restoring snapshot {}", r.snapshot), state: if r.swapped { PendingState::Committed } else { PendingState::Interrupted }, files, journal: name });
            }
            Err(why) => out.push(Pending { journal: name, label: String::new(), state: PendingState::Unreadable(why), files: Vec::new() }),
        }
    }
    out
}

/// Puts right the restores that were interrupted, deciding from the folders themselves (as the
/// transaction engine does with files): the swap went through if `mods` is the incoming folder,
/// and then the old one is removed; otherwise the old one is put back (renamed, never copied) and the
/// incoming one removed. What cannot be decided is kept, with its journal, and reported in `rep`.
pub(crate) fn recover_restores(roots: &GameRoots, jdir: &Dir, rep: &mut Report) {
    for name in restore_journals(jdir) {
        match read_restore(jdir, &name) {
            Ok(None) => {
                // Nothing was ever recorded in it, so nothing was changed for it.
                let _ = jdir.unlink(&name);
            }
            Ok(Some(r)) => recover_restore(roots, jdir, &name, &r, rep),
            Err(why) => rep.failures.push(format!("the restore journal {name} is damaged ({why}); nothing was deleted on its word, its `.paleto-*` folders are listed by `leftovers`")),
        }
    }
}

fn recover_restore(roots: &GameRoots, jdir: &Dir, jname: &str, r: &Restore, rep: &mut Report) {
    let game = &roots.game.dir;
    let shown = |n: &str| roots.game.shown.join(n).display().to_string();
    let label = format!("restoring snapshot {}", r.snapshot);
    let here = |n: &Option<String>| match n {
        Some(n) => game.kind(n).map(|k| k.is_some()).map_err(|e| format!("{}: cannot be looked at: {e}", shown(n))),
        None => Ok(false),
    };
    let (Ok(_), Ok(out_here), Ok(mods_here)) = (here(&r.incoming), here(&r.outgoing), game.kind("mods").map(|k| k.is_some())) else {
        return rep.failures.push(format!("the operation \"{label}\" cannot be recovered: the folders cannot be looked at; journal {jname} kept"));
    };
    let mods_is_new = mods_here && r.made.is_some_and(|m| game.open_dir_following("mods").and_then(|d| d.identity()).is_ok_and(|id| id == m));
    // Our own unique names only, and only if they are folders.
    let remove = |n: &Option<String>| -> Result<(), String> {
        let Some(n) = n else { return Ok(()) };
        match game.kind(n) {
            Ok(None) => Ok(()),
            Ok(Some(Kind::Dir)) => remove_tree(game, n, 0).map_err(|e| format!("{} could not be removed: {e}", shown(n))),
            Ok(Some(_)) => Err(format!("{} is not a folder any more, left alone", shown(n))),
            Err(e) => Err(format!("{}: cannot be looked at: {e}", shown(n))),
        }
    };
    let (done, result): (&str, Result<(), String>) = if mods_is_new {
        ("finished the restore that was interrupted after the swap (the old mods folder removed)", remove(&r.outgoing))
    } else if !mods_here && out_here {
        // Between the two renames: the old folder is only under its aside name.
        let put = game.rename(r.outgoing.as_deref().expect("here"), "mods", false).map_err(|e| format!("the old mods folder cannot be put back from {}: {e}", shown(r.outgoing.as_deref().expect("here"))));
        ("undid the restore that was interrupted: the old mods folder is back", put.and_then(|()| remove(&r.incoming)))
    } else if !mods_here && r.outgoing.is_some() {
        ("", Err(format!("the mods folder was set aside as {} and is gone", shown(r.outgoing.as_deref().expect("some")))))
    } else if mods_here && out_here {
        ("", Err(format!("both the mods folder and {} exist and the mods folder is not the restored copy; left as they are", shown(r.outgoing.as_deref().expect("here")))))
    } else {
        ("undid the restore that was interrupted: the mods folder was not touched", remove(&r.incoming))
    };
    match result {
        Ok(()) => {
            let _ = jdir.unlink(jname);
            let _ = jdir.sync();
            if mods_is_new {
                rep.finished += 1;
            } else {
                rep.rolled_back += 1;
            }
            rep.log.push(done.to_string());
        }
        Err(why) => rep.failures.push(format!("operation \"{label}\": {why}; journal {jname} kept ({inc} {out})", inc = r.incoming.as_deref().map(shown).unwrap_or_default(), out = r.outgoing.as_deref().map(shown).unwrap_or_default())),
    }
}

pub fn delete(game: &Game, id: &str) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "deleting a snapshot")?;
    let snapshot = list(game).into_iter().find(|s| s.id == id).ok_or_else(|| Error::Package(format!("no snapshot {id}")))?;
    let snaps = snapshots_folder(&roots, false)?.ok_or_else(|| Error::Package(format!("no snapshot {id}")))?;
    // Renamed first, so a deletion that stops half way never leaves a snapshot that lists but is incomplete.
    let doomed = snaps.rename_unique(&snapshot.id, ".deleting")?;
    remove_tree(&snaps, &doomed, 0).map_err(|e| Error::Package(format!("snapshot {id} is no longer listed, but its files could not all be deleted ({e}); the rest is in {}", snapshots_dir(game).join(&doomed).display())))
}

/// Bytes in a folder (what a full copy would take).
pub fn size(dir: &Path) -> u64 {
    crate::vfs::local_files(dir).unwrap_or_default().iter().filter_map(|(p, _)| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// Whether copies in this folder share their data (btrfs, XFS, bcachefs),
/// from the mount table: the filesystem of the longest mount point holding it.
pub fn shares_copies(dir: &Path) -> bool {
    let Ok(dir) = std::fs::canonicalize(dir) else { return false };
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, point, fs) = (f.next()?, f.next()?, f.next()?);
            // Spaces in mount points are written as \040.
            let point = point.replace("\\040", " ");
            dir.starts_with(&point).then(|| (point.len(), fs.to_string()))
        })
        .max_by_key(|(len, _)| *len)
        .is_some_and(|(_, fs)| matches!(fs.as_str(), "btrfs" | "xfs" | "bcachefs"))
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"one").unwrap();
        std::fs::write(root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), b"car").unwrap();
        let game = Game::open(root).unwrap();

        let first = create(&game, " works ").unwrap();
        assert_eq!(first.label, "works");
        // A broken state: a file changed, one added, one removed.
        std::fs::write(root.join("mods/x64e.rpf"), b"two").unwrap();
        std::fs::write(root.join("mods/new.rpf"), b"new").unwrap();
        std::fs::remove_file(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap();
        let second = create(&game, "broken").unwrap();
        let ids: Vec<String> = list(&game).into_iter().map(|s| s.label).collect();
        assert_eq!(ids, ["broken", "works"]);
        assert!(size(&second.mods()) > 0);

        restore(&game, &first.id).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64e.rpf")).unwrap(), b"one");
        assert_eq!(std::fs::read(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap(), b"car");
        assert!(!root.join("mods/new.rpf").exists());
        // The snapshot stays as it was, and can be restored again.
        std::fs::write(root.join("mods/x64e.rpf"), b"three").unwrap();
        assert_eq!(std::fs::read(first.mods().join("x64e.rpf")).unwrap(), b"one");
        assert!(!root.join(".mods.restoring").exists() && !root.join(".mods.replaced").exists());

        delete(&game, &second.id).unwrap();
        assert_eq!(list(&game).len(), 1);
        assert!(restore(&game, &second.id).is_err());
    }

    #[test]
    fn a_failed_restore_loses_no_mods() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"old").unwrap();
        let game = Game::open(root).unwrap();
        let first = create(&game, "old").unwrap();
        std::fs::write(root.join("mods/x64e.rpf"), b"current").unwrap();
        // A restore stopped between its two renames: the mods folder is only in `.mods.replaced`.
        std::fs::rename(root.join("mods"), root.join(".mods.replaced")).unwrap();

        // Saving first keeps it, and the restore does not delete it with the leftovers.
        let saved = create(&game, "before").unwrap();
        assert_eq!(std::fs::read(saved.mods().join("x64e.rpf")).unwrap(), b"current");
        restore(&game, &first.id).unwrap();
        assert_eq!(std::fs::read(root.join("mods/x64e.rpf")).unwrap(), b"old");
        assert!(!root.join(".mods.replaced").exists());
    }
}
