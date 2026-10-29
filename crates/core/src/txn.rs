//! The transaction engine: a set of file changes in the game or mods folder
//! that is journaled, so that after an error, a crash or a power cut every file
//! holds its complete old or its complete new content and the operation is
//! either rolled back or finished, never left half-way unnoticed.
//!
//! What this guarantees and what it does not:
//!
//! * Each file is replaced by a rename of a complete, flushed new file, so a
//!   reader sees the old or the new one (except in "move mode", on filesystems
//!   without hard links, where the file is absent for an instant).
//! * A change of several files is NOT atomic: while it commits a running game
//!   can see a mix, and after a crash the mix stays until recovery (the next
//!   [`Txn::begin`], or [`recover`]) restores the old set, or, once every file
//!   is in place, removes the backups. It is recoverable, not atomic.
//! * Recovery can itself fail (a backup deleted by hand, a folder made
//!   read-only). It then keeps the journal and the backups and says what is
//!   left and where; "nothing was installed" is said only when it is true.
//! * Crash safety relies on the filesystem honouring `fsync`.
//!
//! Protocol (see [`journal`] for the file): `begin` takes the game lock and
//! journals; each change is announced in the journal (flushed) before its effect:
//! a new file `.paleto-<txn>-<op>.new` next to its destination (exclusive), and
//! for a replaced file a hard link `.paleto-<txn>-<op>.old` to the original.
//! After `commit` is journaled each file is renamed into place in plan order
//! (a create never replaces anything), then `committed`, then the backups go.
//! Rolling back looks at the files themselves, not at what the journal hoped:
//! the same code runs in the process and in recovery.

mod journal;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use crate::Error;
use crate::fsroot::{Dir, Existing, Fingerprint, GameRoots, Kind, LockGuard, Resolved, RootKind, random};
use crate::game::Game;
use crate::relpath::RelPath;
use journal::{End, JOp, Journal, Mode, OpKind, ParseError, RootRec, State};

/// What went wrong in a transaction that had begun to change files, and what is left of it.
#[derive(Debug)]
pub struct TxnFailure {
    pub cause: Error,
    /// What the operation had done and logged until then.
    pub log: Vec<String>,
    /// Every change was undone.
    pub undone: bool,
    /// Each file that could not be restored, with where its backup is.
    pub failures: Vec<String>,
    /// A journal of the operation was kept, so the next run can finish the recovery (the
    /// operations that do not yet run as a transaction have none: what is left stays as it is).
    pub journaled: bool,
    /// What the operation does, for "(nothing was {verb})": "installed" or "changed".
    pub verb: &'static str,
}

impl fmt::Display for TxnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.cause)?;
        if self.undone && self.failures.is_empty() {
            write!(f, " (nothing was {})", self.verb)
        } else if !self.journaled {
            write!(f, ": the changes could not be fully undone ({}); the files named, with the backups next to them, were left as they are", self.failures.join("; "))
        } else {
            write!(f, ": the changes could not be fully undone ({}); the backups and the journal in .paleto/journal were kept, and the next run offers to finish the recovery", self.failures.join("; "))
        }
    }
}

/// What was done by recovery.
#[derive(Debug, Default)]
pub struct Report {
    /// Interrupted operations that had reached every file and were finished (backups removed).
    pub finished: usize,
    /// Interrupted operations that were undone.
    pub rolled_back: usize,
    /// What was done, one line each.
    pub log: Vec<String>,
    /// What could not be done: the files involved, their backups, and why.
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingState {
    /// Stopped before every file was in place: will be rolled back.
    Interrupted,
    /// Every file was in place; only backups are left to remove.
    Committed,
    /// A rollback that could not finish.
    Stuck,
    /// The journal cannot be read (damaged, or written by a newer version); the reason.
    Unreadable(String),
}

/// A journal left by an earlier run.
#[derive(Debug, Clone)]
pub struct Pending {
    pub journal: String,
    pub label: String,
    pub state: PendingState,
    /// The files it changes, and the backups of the replaced ones.
    pub files: Vec<PathBuf>,
}

/// A file staged by [`Txn::stage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Staged(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    /// Finished: committed, rolled back, or stuck.
    Done,
    /// The process "died" (fault injection in tests): nothing may be touched any more.
    Dead,
}

pub struct Txn<'r> {
    roots: &'r GameRoots,
    id: String,
    journal: File,
    /// Where the journal ends (a failed append is cut off there).
    jlen: u64,
    jdir: Dir,
    jname: String,
    ops: Vec<JOp>,
    /// The folder of each operation; operations in one folder share one handle (a package has thousands of files).
    dirs: Vec<Arc<Dir>>,
    dir_cache: HashMap<String, Arc<Dir>>,
    /// The destinations planned so far, lower-cased (one name must not be changed twice).
    planned: HashSet<String>,
    staged: Vec<Option<File>>,
    rooted: [bool; 2],
    committing: bool,
    phase: Phase,
    /// What the rollback that ended the transaction could not undo (see [`Txn::abort`]).
    left: Vec<String>,
    recovered: Vec<String>,
    _lock: LockGuard<'r>,
}

fn dead() -> bool {
    #[cfg(test)]
    {
        crate::fsroot::faults::crashed()
    }
    #[cfg(not(test))]
    {
        false
    }
}

// Runs once when a rollback starts (tests damage the folder to make it fail).
#[cfg(test)]
thread_local! {
    pub(crate) static ON_ROLLBACK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn staged_name(txn: &str, i: usize) -> String {
    format!(".paleto-{txn}-{i}.new")
}

fn backup_name(txn: &str, i: usize) -> String {
    format!(".paleto-{txn}-{i}.old")
}

fn sync_file(file: &File) -> io::Result<()> {
    fault!(SyncFile);
    file.sync_all()
}

fn append(file: &File, at: u64, line: &str) -> io::Result<()> {
    fault!(Journal);
    file.write_all_at(line.as_bytes(), at)?;
    fault!(SyncFile);
    file.sync_data()
}

fn is_missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

fn unsafe_dest(r: &Resolved, why: &str) -> Error {
    Error::UnsafePath { path: r.path().display().to_string(), why: why.to_string() }
}

/// Whether two states of a file are the same: the same bytes at both ends and
/// size, and the same inode or modification time (inodes change across
/// remounts of FAT and exFAT, so they alone do not decide). The middle of a
/// large file is not looked at: a program that changes it in place and keeps
/// the size and the time is not noticed.
fn same_state(a: &Fingerprint, b: &Fingerprint) -> bool {
    a.size == b.size && a.head_tail == b.head_tail && (a.same_file(b) || a.mtime_ns == b.mtime_ns)
}

fn matches(seen: &Existing, want: &Existing) -> bool {
    match (seen, want) {
        (Existing::File(a), Existing::File(b)) => same_state(a, b),
        (Existing::Link(a), Existing::Link(b)) => a == b,
        _ => false,
    }
}

fn matches_opt(seen: &Option<Existing>, want: &Option<Existing>) -> bool {
    match (seen, want) {
        (None, None) => true,
        (Some(a), Some(b)) => matches(a, b),
        _ => false,
    }
}

fn is_staged(seen: &Existing, op: &JOp) -> bool {
    matches!((seen, &op.made), (Existing::File(a), Some(m)) if same_state(a, m))
}

fn same_inode(a: &Existing, b: &Existing) -> bool {
    matches!((a, b), (Existing::File(a), Existing::File(b)) if a.same_file(b))
}

/// The key of an operation's destination in `planned` (names differing only in case are one name).
fn dest_key(op: &JOp) -> String {
    format!("{}/{}/{}", usize::from(op.anchor == RootKind::Mods), op.dir.join("/"), op.name).to_ascii_lowercase()
}

fn path_of(roots: &GameRoots, op: &JOp, name: &str) -> PathBuf {
    let base = roots.anchor_root(op.anchor).map_or_else(|_| if op.anchor == RootKind::Mods { roots.game.shown.join("mods") } else { roots.game.shown.clone() }, |r| r.shown.clone());
    base.join(op.dir.iter().collect::<PathBuf>()).join(name)
}

// ---- rolling back (in the process and in recovery) ----

#[derive(Debug, Default)]
struct Rolled {
    failures: Vec<String>,
    notes: Vec<String>,
}

/// Undoes `ops` in reverse by looking at what is on disk. `committing`: the
/// journal says `commit` was reached, so a target may already be replaced;
/// before that only temporary names, backup links and new folders exist.
/// `refused` (recovery only, else empty) holds a reason for each operation the policy refuses: it is reported, not touched.
fn rollback_ops(roots: &GameRoots, txn: &str, ops: &[JOp], committing: bool, refused: &[Option<String>]) -> Rolled {
    let mut r = Rolled::default();
    for (i, op) in ops.iter().enumerate().rev() {
        if dead() {
            r.failures.push("interrupted again while undoing".into());
            break;
        }
        if let Some(Some(why)) = refused.get(i) {
            r.failures.push(why.clone());
            continue;
        }
        rollback_op(roots, txn, i, op, committing, &mut r);
    }
    r
}

/// The reason, for each operation of a journal, that the write policy refuses it now (recovery must not
/// touch what a planted or damaged journal points at: originals, programs, the tool's own folders).
fn vet_ops(roots: &GameRoots, txn: &str, ops: &[JOp]) -> Vec<Option<String>> {
    let mods_made = ops.iter().any(|o| o.kind == OpKind::Mkdir && o.anchor == RootKind::Game && o.dir.is_empty() && o.name.eq_ignore_ascii_case("mods"));
    ops.iter()
        .enumerate()
        .map(|(i, op)| {
            let replaces = matches!(op.kind, OpKind::Replace | OpKind::Delete);
            // A file this operation staged and put in place is not one of the game's own, whatever its name: it may be undone.
            let ours = matches!(op.kind, OpKind::Create | OpKind::Replace) && roots.open_anchor(op.anchor, &op.dir).and_then(|d| Ok(d.stat(&op.name)?)).is_ok_and(|seen| seen.is_some_and(|s| is_staged(&s, op)));
            roots.check_journal_op(op.anchor, &op.dir, &op.name, op.kind == OpKind::Mkdir, replaces, ours, mods_made).err().map(|e| {
                let backup = if replaces { format!(" (backup: {})", path_of(roots, op, &backup_name(txn, i)).display()) } else { String::new() };
                format!("{}: not touched, the journal names it but it may not be changed: {e}{backup}", path_of(roots, op, &op.name).display())
            })
        })
        .collect()
}

fn rollback_op(roots: &GameRoots, txn: &str, i: usize, op: &JOp, committing: bool, r: &mut Rolled) {
    let (tmp, bak) = (staged_name(txn, i), backup_name(txn, i));
    let shown = |name: &str| path_of(roots, op, name).display().to_string();
    let backup = || format!("backup: {}", shown(&bak));
    let dir = match roots.open_anchor(op.anchor, &op.dir) {
        Ok(d) => d,
        Err(Error::Io(e)) if is_missing(&e) || e.raw_os_error() == Some(libc::ENOTDIR) => return gone(op, committing, &shown(&op.name), r),
        Err(Error::UnsafePath { .. }) if op.anchor == RootKind::Mods => return gone(op, committing, &shown(&op.name), r),
        Err(e) => return r.failures.push(format!("{}: its folder cannot be opened: {e}", shown(&op.name))),
    };
    let sync = || {
        let _ = dir.sync();
    };
    let unlink = |name: &str, r: &mut Rolled| match dir.unlink(name) {
        Ok(()) => true,
        Err(e) if is_missing(&e) => true,
        Err(e) => {
            r.failures.push(format!("{}: cannot be removed: {e}", shown(name)));
            false
        }
    };
    if op.kind == OpKind::Mkdir {
        if op.kept {
            return;
        }
        // Only a folder this operation recorded as made is removed: one without a `made` record may be
        // another program's (it appeared where this operation meant to make it), even if `kept` was never written.
        let Some(made) = &op.made else {
            if matches!(dir.kind(&op.name), Ok(Some(_))) {
                r.notes.push(format!("{}: left in place, it is not recorded as made by this operation", shown(&op.name)));
            }
            return;
        };
        match dir.kind(&op.name) {
            Ok(None) => {}
            Ok(Some(Kind::Dir)) => {
                match dir.open_dir(&op.name).and_then(|d| d.fingerprint()) {
                    Ok(fp) if fp.same_file(made) => {}
                    Ok(_) => return r.notes.push(format!("{}: left alone, it is not the folder this operation made", shown(&op.name))),
                    Err(e) => return r.failures.push(format!("{}: cannot be looked at: {e}", shown(&op.name))),
                }
                match dir.rmdir(&op.name) {
                    Ok(()) => sync(),
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => r.notes.push(format!("{}: made by this operation but not empty, left in place", shown(&op.name))),
                    Err(e) => r.failures.push(format!("{}: the folder made by this operation cannot be removed: {e}", shown(&op.name))),
                }
            }
            Ok(Some(_)) => r.notes.push(format!("{}: is not a folder any more, left alone", shown(&op.name))),
            Err(e) => r.failures.push(format!("{}: cannot be looked at: {e}", shown(&op.name))),
        }
        return;
    }
    // A staged name only ever holds new content that was never in place.
    let mut clean = unlink(&tmp, r);
    let stat = |name: &str, r: &mut Rolled| match dir.stat(name) {
        Ok(s) => Some(s),
        Err(e) => {
            r.failures.push(format!("{}: cannot be looked at: {e}", shown(name)));
            None
        }
    };
    let Some(target) = stat(&op.name, r) else { return };
    match op.kind {
        OpKind::Create => {
            if committing && let Some(t) = &target {
                if is_staged(t, op) {
                    clean &= unlink(&op.name, r);
                } else if op.did {
                    // It was put in place and is not what was put there: not ours to remove, and not as it was before.
                    r.failures.push(format!("{}: was created by this operation and changed by something else since; left as it is", shown(&op.name)));
                    clean = false;
                }
            }
            if clean {
                sync();
            }
        }
        OpKind::Replace | OpKind::Delete => {
            let Some(bak_state) = stat(&bak, r) else { return };
            let want = op.expect.as_ref().expect("a replace or delete has what it replaces");
            let bak_ok = bak_state.as_ref().is_some_and(|b| matches(b, want));
            if !committing {
                // Nothing was renamed yet: at most the backup link of a replace is there.
                if let Some(b) = &bak_state {
                    if bak_ok || target.as_ref().is_some_and(|t| same_inode(t, b)) {
                        unlink(&bak, r);
                    } else {
                        r.failures.push(format!("{}: a backup that is not the original was found and kept ({})", shown(&op.name), backup()));
                    }
                }
                sync();
                return;
            }
            let t_staged = op.kind == OpKind::Replace && target.as_ref().is_some_and(|t| is_staged(t, op));
            let t_orig = target.as_ref().is_some_and(|t| matches(t, want));
            if t_staged {
                if bak_ok {
                    match dir.rename(&bak, &op.name, true) {
                        Ok(()) => sync(),
                        Err(e) => r.failures.push(format!("{}: the original cannot be put back: {e} ({})", shown(&op.name), backup())),
                    }
                } else {
                    r.failures.push(format!("{}: the new content is in place and the original cannot be put back: its backup is gone or changed ({})", shown(&op.name), backup()));
                }
            } else if t_orig {
                // Not touched: the backup, if there, is a second name of the same data.
                match &bak_state {
                    Some(_) if bak_ok => {
                        unlink(&bak, r);
                        sync();
                    }
                    Some(_) => r.failures.push(format!("{}: a backup that is not the original was found and kept ({})", shown(&op.name), backup())),
                    None => {}
                }
            } else if target.is_none() {
                if bak_ok {
                    match dir.rename(&bak, &op.name, false) {
                        Ok(()) => sync(),
                        Err(e) => r.failures.push(format!("{}: the original cannot be put back: {e} ({})", shown(&op.name), backup())),
                    }
                } else {
                    r.failures.push(format!("{}: is gone and so is the original: its backup is missing or changed ({})", shown(&op.name), backup()));
                }
            } else {
                r.failures.push(format!("{}: was changed by something else since; left as it is ({})", shown(&op.name), backup()));
            }
        }
        OpKind::Mkdir => unreachable!("handled above"),
    }
}

/// An operation whose folder is gone: nothing to undo, unless it had replaced or deleted something.
fn gone(op: &JOp, committing: bool, name: &str, r: &mut Rolled) {
    if committing && matches!(op.kind, OpKind::Replace | OpKind::Delete) {
        r.failures.push(format!("{name}: its folder is gone, so the original cannot be restored"));
    }
}

/// Writes `end` into a journal at `at` (where its last whole record ends), through the handle given, so that
/// bytes of a torn record before that point cannot be left in front of it: whatever follows is overwritten and cut off.
/// Best effort: what the journal says is only what was hoped, recovery looks at the files.
fn write_end(file: &File, at: u64, open_line: bool, end: &End) {
    let line = format!("{}{}", if open_line { "\n" } else { "" }, journal::end(end));
    if append(file, at, &line).is_ok() {
        let _ = file.set_len(at + line.len() as u64);
    }
}

/// Ends a rollback in the journal: a clean one removes the journal, otherwise
/// it is marked stuck and kept. `write` puts the `end` record in at the right place.
fn finish_rollback(jdir: &Dir, jname: &str, rolled: &Rolled, write: impl FnOnce(&End)) {
    let end = if rolled.failures.is_empty() { End::RolledBack } else { End::Stuck(rolled.failures.join("; ")) };
    write(&end);
    if rolled.failures.is_empty() {
        let _ = jdir.unlink(jname);
        let _ = jdir.sync();
    }
}

fn txn_error(cause: Error, log: Vec<String>, rolled: &Rolled) -> Error {
    Error::Txn(Box::new(TxnFailure { cause, log, undone: rolled.failures.is_empty(), failures: rolled.failures.clone(), journaled: true, verb: "installed" }))
}

/// Gives the caller's `log` and `verb` to an error from [`Txn::stage`], [`Txn::delete`] or a `commit*`:
/// a plain error there means the undo was clean ("nothing was {verb}"); an [`Error::Txn`] already says what
/// is left and passes through, with `log` in front of what it logged and `verb` for its message. The error of
/// a call on a transaction that had already ended says nothing about the undo: it is returned as it is.
pub fn failure(e: Error, mut log: Vec<String>, verb: &'static str) -> Error {
    if matches!(&e, Error::Io(io) if io.get_ref().is_some_and(|inner| inner.is::<Finished>())) {
        return e;
    }
    match e {
        Error::Txn(mut f) => {
            log.append(&mut f.log);
            f.log = log;
            f.verb = verb;
            Error::Txn(f)
        }
        cause => Error::Txn(Box::new(TxnFailure { cause, log, undone: true, failures: Vec::new(), journaled: false, verb })),
    }
}

/// What a call on a transaction that is over fails with.
#[derive(Debug)]
struct Finished;

impl fmt::Display for Finished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the transaction is already finished")
    }
}

impl std::error::Error for Finished {}

/// Free space that must stay after a staged file is written.
const FREE_MARGIN: u64 = 1 << 30;

/// A size for a message, in the largest unit that keeps it above 1.
fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MiB", b as f64 / (1u64 << 20) as f64),
        b => format!("{b} bytes"),
    }
}

/// `need` bytes leave [`FREE_MARGIN`] free where `dest` will be made.
fn enough_space(dest: &Resolved, need: u64) -> Result<(), Error> {
    let free = dest.parent.free_space()?;
    if need.checked_add(FREE_MARGIN).is_some_and(|total| total <= free) {
        return Ok(());
    }
    let folder = dest.path().parent().map(|p| p.display().to_string()).unwrap_or_default();
    Err(Error::Io(io::Error::new(io::ErrorKind::StorageFull, format!("{folder}: not enough free space: {} are needed and at least {} must stay free, but only {} are free", size_text(need), size_text(FREE_MARGIN), size_text(free)))))
}

// ---- recovery ----

/// A journal file by name, as read.
type Scanned = (String, Result<Journal, ParseError>);

fn scan(jdir: &Dir) -> Result<Vec<Scanned>, Error> {
    let mut names: Vec<String> = jdir.names()?.into_iter().filter(|n| n.ends_with(".log")).collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let parsed = scan_one(jdir, &name);
        out.push((name, parsed));
    }
    Ok(out)
}

fn scan_one(jdir: &Dir, name: &str) -> Result<Journal, ParseError> {
    match jdir.open_read(name) {
        Ok(file) => {
            let mut bytes = Vec::new();
            match file.take(journal::MAX_SIZE + 1).read_to_end(&mut bytes) {
                Ok(_) if bytes.len() as u64 > journal::MAX_SIZE => Err(ParseError::Corrupt("larger than any journal".into())),
                Ok(_) => journal::parse(&bytes),
                Err(e) => Err(ParseError::Corrupt(format!("cannot be read: {e}"))),
            }
        }
        Err(e) => Err(ParseError::Corrupt(format!("cannot be opened: {e}"))),
    }
}

fn files_of(roots: &GameRoots, j: &Journal) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (i, op) in j.ops.iter().enumerate() {
        match op.kind {
            OpKind::Mkdir => {}
            OpKind::Create => files.push(path_of(roots, op, &op.name)),
            OpKind::Replace | OpKind::Delete => {
                files.push(path_of(roots, op, &op.name));
                files.push(path_of(roots, op, &backup_name(&j.txn, i)));
            }
        }
    }
    files
}

/// Whether the roots this journal names are the ones open now.
fn check_roots(roots: &GameRoots, j: &Journal) -> Result<(), String> {
    for rec in &j.roots {
        // A mods folder that is gone has taken what the operation did there with it: what was in it is reported by the
        // rollback (`gone`), not an obstacle to recovering the rest.
        if rec.kind == RootKind::Mods && roots.mods_missing() {
            continue;
        }
        let now = roots.anchor_root(rec.kind).map_err(|e| format!("the {} folder it used cannot be used now: {e}", journal_root(rec.kind)))?;
        if !now.id.same_file(&rec.fp) && now.shown.display().to_string() != rec.shown {
            return Err(format!("the {} folder is not the one it used ({})", journal_root(rec.kind), rec.shown));
        }
    }
    Ok(())
}

fn journal_root(kind: RootKind) -> &'static str {
    match kind {
        RootKind::Game => "game",
        RootKind::Mods => "mods",
    }
}

/// Finishes or undoes every journal of the game. `retry_stuck`: also tries again those a
/// rollback could not finish earlier (otherwise they are only reported).
fn recover_all(roots: &GameRoots, jdir: &Dir, retry_stuck: bool) -> Result<Report, Error> {
    let mut rep = Report::default();
    // An interrupted snapshot restore may have left the mods folder aside: put it back before anything looks at it.
    crate::snapshots::recover_restores(roots, jdir, &mut rep);
    for (name, parsed) in scan(jdir)? {
        if dead() {
            rep.failures.push("interrupted again while recovering".into());
            break;
        }
        match parsed {
            Err(ParseError::Empty) => {
                // Nothing was ever recorded in it, so nothing was changed for it.
                let _ = jdir.unlink(&name);
            }
            Err(ParseError::Version(v)) => rep.failures.push(format!("journal {name} was written by a newer version of this program (format {v}); it was left alone")),
            Err(ParseError::Corrupt(why)) => rep.failures.push(format!("journal {name} is damaged ({why}); nothing was deleted on its word, its `.paleto-*` files are listed by `leftovers`")),
            Ok(j) => recover_journal(roots, jdir, &name, &j, retry_stuck, &mut rep),
        }
    }
    Ok(rep)
}

/// Writes `end` into the journal of an earlier run, over whatever torn bytes follow its last whole record.
fn write_end_of(jdir: &Dir, name: &str, j: &Journal, end: &End) {
    if let Ok(f) = jdir.open_rw(name) {
        write_end(&f, j.good_len, j.open_line, end);
    }
}

fn recover_journal(roots: &GameRoots, jdir: &Dir, name: &str, j: &Journal, retry_stuck: bool, rep: &mut Report) {
    let state = j.state();
    if state == State::Done {
        let _ = jdir.unlink(name);
        return;
    }
    let files = || files_of(roots, j).iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    if state == State::Stuck && !retry_stuck {
        rep.failures.push(format!("the operation \"{}\" could not be undone earlier and is waiting for recovery; files: {}", j.label, files()));
        return;
    }
    if let Err(why) = check_roots(roots, j) {
        rep.failures.push(format!("the operation \"{}\" cannot be recovered: {why}; files: {}", j.label, files()));
        return;
    }
    let refused = vet_ops(roots, &j.txn, &j.ops);
    if state == State::Committed {
        let (mut left, mut refused_left) = (Vec::new(), Vec::new());
        for (i, op) in j.ops.iter().enumerate() {
            if !matches!(op.kind, OpKind::Replace | OpKind::Delete) {
                continue;
            }
            if let Some(Some(why)) = refused.get(i) {
                refused_left.push(format!("operation \"{}\": {why}", j.label));
                continue;
            }
            let bak = backup_name(&j.txn, i);
            match roots.open_anchor(op.anchor, &op.dir) {
                Ok(dir) => match dir.unlink(&bak) {
                    Ok(()) => {
                        let _ = dir.sync();
                    }
                    Err(e) if is_missing(&e) => {}
                    Err(e) => left.push(format!("{}: {e}", path_of(roots, op, &bak).display())),
                },
                Err(Error::Io(e)) if is_missing(&e) => {}
                // The mods folder is gone, and its backups with it.
                Err(Error::UnsafePath { .. }) if op.anchor == RootKind::Mods && roots.mods_missing() => {}
                Err(e) => left.push(format!("{}: {e}", path_of(roots, op, &bak).display())),
            }
        }
        if !refused_left.is_empty() {
            rep.failures.extend(refused_left);
            return;
        }
        if !left.is_empty() {
            // The operation is complete; only its backups could not be removed. The next run tries again.
            rep.log.push(format!("the operation \"{}\" had finished; these backups could not be removed yet: {}", j.label, left.join("; ")));
            return;
        }
        write_end_of(jdir, name, j, &End::Ok);
        let _ = jdir.unlink(name);
        let _ = jdir.sync();
        rep.finished += 1;
        rep.log.push(format!("finished the operation \"{}\" that was interrupted after its files were in place (backups removed)", j.label));
        return;
    }
    let rolled = rollback_ops(roots, &j.txn, &j.ops, j.commit, &refused);
    finish_rollback(jdir, name, &rolled, |end| write_end_of(jdir, name, j, end));
    rep.log.extend(rolled.notes);
    if rolled.failures.is_empty() {
        rep.rolled_back += 1;
        rep.log.push(format!("undid the operation \"{}\" that was interrupted; every file is as it was", j.label));
    } else {
        rep.failures.extend(rolled.failures.iter().map(|f| format!("operation \"{}\": {f}", j.label)));
    }
}

/// [`recover_all`] on the journals of both places (`.paleto/journal` of the game and the fallback in the data folder).
fn recover_every(roots: &GameRoots, retry_stuck: bool) -> Result<Report, Error> {
    let mut rep = Report::default();
    for jdir in roots.journal_dirs()? {
        let one = recover_all(roots, &jdir, retry_stuck)?;
        rep.finished += one.finished;
        rep.rolled_back += one.rolled_back;
        rep.log.extend(one.log);
        rep.failures.extend(one.failures);
    }
    Ok(rep)
}

fn recovery_needed(failures: &[String]) -> Error {
    Error::RecoveryNeeded(format!("an earlier operation was interrupted and could not be undone, so nothing else is changed until it is: {}", failures.join("; ")))
}

/// Journals left by earlier runs, not counting one that is running now in another program.
pub fn pending(g: &Game) -> Vec<Pending> {
    let Ok(roots) = g.roots() else { return Vec::new() };
    pending_in(&roots)
}

fn pending_in(roots: &GameRoots) -> Vec<Pending> {
    if roots.lock_is_held_elsewhere() {
        return Vec::new();
    }
    let Ok(dirs) = roots.journal_dirs() else { return Vec::new() };
    let mut out = Vec::new();
    for jdir in dirs {
        let Ok(found) = scan(&jdir) else { continue };
        out.extend(crate::snapshots::pending_restores(roots, &jdir));
        pending_of(roots, found, &mut out);
    }
    out
}

fn pending_of(roots: &GameRoots, found: Vec<Scanned>, out: &mut Vec<Pending>) {
    for (name, parsed) in found {
        match parsed {
            Ok(j) if j.state() == State::Done => {}
            Ok(j) => {
                let state = match j.state() {
                    State::Committed => PendingState::Committed,
                    State::Stuck => PendingState::Stuck,
                    _ => PendingState::Interrupted,
                };
                out.push(Pending { files: files_of(roots, &j), journal: name, label: j.label, state });
            }
            Err(ParseError::Empty) => {}
            Err(ParseError::Version(v)) => out.push(Pending { journal: name, label: String::new(), state: PendingState::Unreadable(format!("written by a newer version (format {v})")), files: Vec::new() }),
            Err(ParseError::Corrupt(why)) => out.push(Pending { journal: name, label: String::new(), state: PendingState::Unreadable(why), files: Vec::new() }),
        }
    }
}

/// Finishes or undoes what earlier runs left, retrying rollbacks that were stuck.
/// `Err` only if the game is busy or the journals cannot be listed; what could
/// not be recovered is in [`Report::failures`].
pub fn recover(g: &Game) -> Result<Report, Error> {
    recover_in(&g.roots()?)
}

fn recover_in(roots: &GameRoots) -> Result<Report, Error> {
    let _lock = roots.lock("recovering an interrupted operation")?;
    recover_every(roots, true)
}

/// The user's explicit decision to stop a journal from blocking: it is moved to `journal/abandoned/` and
/// ignored from then on. Nothing it names is touched: no backup, staged file or new file is deleted or
/// put back, so the report lists every one of them for the user to look at (and `leftovers` lists the
/// `.paleto-*` names). For a journal that recovery cannot finish, such as one of an operation in a mods
/// folder that is gone. `id` is the name `pending` gives, with or without `.log`. [`Error::Busy`] while a change runs.
pub fn abandon(g: &Game, id: &str) -> Result<Report, Error> {
    abandon_in(&g.roots()?, id)
}

fn abandon_in(roots: &GameRoots, id: &str) -> Result<Report, Error> {
    let id = id.strip_suffix(".log").unwrap_or(id);
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidInput, format!("{id:?} is not the name of a journal"))));
    }
    let name = format!("{id}.log");
    let _lock = roots.lock("abandoning an interrupted operation")?;
    let mut found = None;
    for jdir in roots.journal_dirs()? {
        if jdir.kind(&name)? == Some(Kind::File) {
            found = Some(jdir);
            break;
        }
    }
    let Some(jdir) = found else { return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("there is no journal {name}")))) };
    let (label, files) = match scan_one(&jdir, &name) {
        Ok(j) => {
            let mut files = files_of(roots, &j);
            for (i, op) in j.ops.iter().enumerate() {
                if matches!(op.kind, OpKind::Create | OpKind::Replace) {
                    files.push(path_of(roots, op, &staged_name(&j.txn, i)));
                }
            }
            (j.label.clone(), files)
        }
        Err(why) => (String::new(), vec![PathBuf::from(format!("(the journal cannot be read: {why:?}; its `.paleto-*` files are listed by `leftovers`)"))]),
    };
    let abandoned = jdir.open_or_make_dir("abandoned")?;
    let mut moved = None;
    for n in 0..100 {
        let to = if n == 0 { name.clone() } else { format!("{name}.{n}") };
        match jdir.rename_into(&name, &abandoned, &to) {
            Ok(()) => {
                moved = Some(to);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let Some(moved) = moved else { return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, "no free name in the folder of abandoned journals"))) };
    let _ = abandoned.sync();
    let _ = jdir.sync();
    let mut rep = Report::default();
    rep.log.push(format!("abandoned the operation \"{label}\" (journal {name}, moved to the folder `abandoned` as {moved}): nothing it names was touched or deleted, and it blocks nothing now"));
    rep.log.extend(files.iter().map(|f| format!("left as it is: {}", f.display())));
    Ok(rep)
}

/// `.paleto-*` names in the game folder (the mods folder included) that no journal
/// accounts for: left by a crash that lost its journal, or by an archive save
/// (`.paleto-save-*`). Only listed, never deleted: a `.old` may be the only copy of something.
pub fn leftovers(g: &Game) -> Vec<PathBuf> {
    let Ok(roots) = g.roots() else { return Vec::new() };
    leftovers_in(&roots)
}

fn leftovers_in(roots: &GameRoots) -> Vec<PathBuf> {
    let mut ids = Vec::new();
    for jdir in roots.journal_dirs().unwrap_or_default() {
        if let Ok(found) = scan(&jdir) {
            ids.extend(found.into_iter().filter_map(|(_, j)| j.ok().map(|j| format!(".paleto-{}-", j.txn))));
            ids.extend(crate::snapshots::restore_ids(&jdir).into_iter().map(|id| format!(".paleto-{id}-")));
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![(roots.game.shown.clone(), 0usize)];
    if let Some(mods) = roots.mods()
        && !mods.canon.starts_with(&roots.game.canon)
    {
        stack.push((mods.shown.clone(), 0));
    }
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            if name.starts_with(".paleto-") && !ids.iter().any(|id| name.starts_with(id.as_str())) {
                out.push(entry.path());
            }
            // The originals are never written, and `.paleto` holds journals, not temporaries.
            let top = dir == roots.game.shown;
            let skip = top && (name == ".paleto" || name.eq_ignore_ascii_case("x64") || name.eq_ignore_ascii_case("update"));
            if kind.is_dir() && !skip && depth < 64 {
                // Snapshots are whole copies of the mods folder: only the names in `mods_snapshots` are looked at.
                let depth = if top && name == "mods_snapshots" { 64 } else { depth + 1 };
                stack.push((entry.path(), depth));
            }
        }
    }
    out.sort();
    out
}

/// Replaces one file as a transaction of one operation.
pub fn replace_one(roots: &GameRoots, path: &RelPath, label: &str, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<(), Error> {
    let mut txn = Txn::begin(roots, label)?;
    let mut dest = roots.dest(path)?;
    txn.stage(&mut dest, write)?;
    txn.commit(Vec::new()).map(drop)
}

/// [`replace_one`] for a destination already resolved (to a place [`GameRoots::dest`] would not
/// choose, such as the mods copy of a game file). Begins the transaction first and, as a failure
/// leaves nothing changed, `dest` may be looked at again after an error.
pub fn replace_resolved(roots: &GameRoots, dest: &mut Resolved, label: &str, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<(), Error> {
    let mut txn = Txn::begin(roots, label)?;
    txn.stage(dest, write)?;
    txn.commit(Vec::new()).map(drop)
}

/// Takes the lock of the game for a change that is not a transaction (a snapshot, a folder
/// moved aside, a file made by hand), as [`Txn::begin`] does for one, and first deals with
/// what an interrupted change left: undone or finished, so the new change starts from a
/// whole state. [`Error::Busy`] if another change is running; [`Error::RecoveryNeeded`] if
/// an earlier one could not be undone. Taken again by the thread that holds it, it only
/// counts (and recovers nothing: a change of this program may be half way).
pub fn guard<'r>(roots: &'r GameRoots, label: &str) -> Result<LockGuard<'r>, Error> {
    let lock = roots.lock(label)?;
    if lock.is_outer() {
        let report = recover_every(roots, false)?;
        for line in report.log.iter().chain(&report.failures) {
            eprintln!("paleto: {line}");
        }
        if !report.failures.is_empty() {
            return Err(recovery_needed(&report.failures));
        }
    }
    Ok(lock)
}

// ---- the transaction ----

impl<'r> Txn<'r> {
    /// Takes the lock of the game and starts a journal. Journals left by earlier
    /// runs are dealt with first: one that reached every file is finished, any
    /// other undone, and what that did is in [`Txn::recovered`]. If one cannot
    /// be undone, [`Error::RecoveryNeeded`] says which files are involved.
    pub fn begin(roots: &'r GameRoots, label: &str) -> Result<Txn<'r>, Error> {
        let lock = roots.lock(label)?.claim_txn()?;
        let jdir = roots.journal_dir()?;
        let report = recover_every(roots, false)?;
        for line in report.log.iter().chain(&report.failures) {
            eprintln!("paleto: {line}");
        }
        if !report.failures.is_empty() {
            return Err(recovery_needed(&report.failures));
        }
        let (journal, id, jname) = {
            let mut tries = 0;
            loop {
                let id = format!("{:016x}", random());
                let jname = format!("{id}.log");
                match jdir.create_excl(&jname) {
                    Ok(f) => break (f, id, jname),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
                    Err(e) => return Err(e.into()),
                }
            }
        };
        let start = format!("{}{}", journal::header(), journal::begin(&id, std::process::id(), unix_now(), label));
        let first = append(&journal, 0, &start).and_then(|()| jdir.sync());
        if let Err(e) = first {
            if !dead() {
                let _ = jdir.unlink(&jname);
            }
            return Err(e.into());
        }
        Ok(Txn { roots, id, journal, jlen: start.len() as u64, jdir, jname, ops: Vec::new(), dirs: Vec::new(), dir_cache: HashMap::new(), planned: HashSet::new(), staged: Vec::new(), rooted: [false; 2], committing: false, phase: Phase::Open, left: Vec::new(), recovered: report.log, _lock: lock })
    }

    /// What recovery did when this transaction began.
    pub fn recovered(&self) -> &[String] {
        &self.recovered
    }

    fn ensure_open(&self) -> Result<(), Error> {
        if self.phase == Phase::Open { Ok(()) } else { Err(Error::Io(io::Error::other(Finished))) }
    }

    fn put(&mut self, line: &str) -> io::Result<()> {
        match append(&self.journal, self.jlen, line) {
            Ok(()) => {
                self.jlen += line.len() as u64;
                Ok(())
            }
            Err(e) => {
                // Cut off what may have been half written, so the next record starts on a line of its own.
                if !dead() {
                    let _ = self.journal.set_len(self.jlen);
                }
                Err(e)
            }
        }
    }

    /// An error raised by the caller while the transaction is open (a command of a package that cannot be
    /// applied): everything staged is undone, and the answer is always an [`Error::Txn`] carrying `log`. It says
    /// "(nothing was installed)" only if the undo was clean, else what is left and where; [`failure`] changes the verb.
    /// If an error of `stage` or `delete` had already ended the transaction, what its rollback could not undo is
    /// reported again; if that was clean, the error is `cause` as it is: this does not know what else the caller did.
    pub fn abort(mut self, cause: Error, log: Vec<String>) -> Error {
        if self.phase == Phase::Done {
            if self.left.is_empty() {
                return cause;
            }
            return txn_error(cause, log, &Rolled { failures: self.left.clone(), notes: Vec::new() });
        }
        self.fail(cause, log, true)
    }

    /// Undoes everything (idempotent). Ends the transaction either way.
    fn rollback_now(&mut self) -> Rolled {
        if self.phase != Phase::Open {
            return Rolled::default();
        }
        if dead() {
            self.phase = Phase::Dead;
            return Rolled::default();
        }
        #[cfg(test)]
        if let Some(hook) = ON_ROLLBACK.with(|h| h.borrow_mut().take()) {
            hook();
        }
        let rolled = rollback_ops(self.roots, &self.id, &self.ops, self.committing, &[]);
        let (journal, jlen) = (&self.journal, self.jlen);
        finish_rollback(&self.jdir, &self.jname, &rolled, |end| write_end(journal, jlen, false, end));
        self.phase = Phase::Done;
        self.left = rolled.failures.clone();
        rolled
    }

    /// An error during the transaction: undo, and say so honestly. Before the
    /// commit began (`wrap` false) a clean undo returns the cause itself.
    fn fail(&mut self, cause: Error, log: Vec<String>, wrap: bool) -> Error {
        if dead() {
            self.phase = Phase::Dead;
            return cause;
        }
        let rolled = self.rollback_now();
        if rolled.failures.is_empty() && !wrap { cause } else { txn_error(cause, log, &rolled) }
    }

    fn note_root(&mut self, kind: RootKind) -> Result<(), Error> {
        let slot = usize::from(kind == RootKind::Mods);
        if self.rooted[slot] {
            return Ok(());
        }
        let root = self.roots.anchor_root(kind)?;
        let line = journal::root(&RootRec { kind, fp: root.dir.fingerprint()?, shown: root.shown.display().to_string() });
        self.put(&line)?;
        self.rooted[slot] = true;
        Ok(())
    }

    /// Announces an operation in the folder `dir` (the one it was resolved to).
    fn add_op(&mut self, op: JOp, dir: &Dir) -> Result<usize, Error> {
        self.note_root(op.anchor)?;
        let i = self.ops.len();
        self.put(&journal::op(i, &op))?;
        let key = format!("{}/{}", usize::from(op.anchor == RootKind::Mods), op.dir.join("/"));
        let shared = self.dir_cache.get(&key).filter(|d| d.identity().ok().is_some_and(|id| dir.identity().ok() == Some(id))).cloned();
        let dir = match shared {
            Some(d) => d,
            None => {
                let d = Arc::new(dir.try_clone()?);
                self.dir_cache.insert(key, d.clone());
                d
            }
        };
        if op.kind != OpKind::Mkdir {
            self.planned.insert(dest_key(&op));
        }
        self.ops.push(op);
        self.dirs.push(dir);
        self.staged.push(None);
        Ok(i)
    }

    fn touches(&self, dest: &Resolved) -> bool {
        self.planned.contains(&format!("{}/{}/{}", usize::from(dest.anchor() == RootKind::Mods), dest.logical.join("/"), dest.name).to_ascii_lowercase())
    }

    /// Makes the folders `dest` needs, one at a time, each announced first.
    fn make_parents(&mut self, dest: &mut Resolved) -> Result<(), Error> {
        while let Some(name) = dest.missing.first().cloned() {
            self.roots.verify(dest)?;
            let op = JOp { kind: OpKind::Mkdir, anchor: dest.anchor(), dir: dest.logical.clone(), name: name.clone(), expect: None, mode: Mode::Move, made: None, kept: false, did: false };
            let i = self.add_op(op, &dest.parent)?;
            let (made, kept) = match dest.parent.mkdir(&name) {
                Ok(dir) => (dir, false),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match dest.parent.kind(&name)? {
                    Some(Kind::Dir) => (dest.parent.open_dir(&name)?, true),
                    Some(Kind::Link) => return Err(unsafe_dest(dest, &format!("\"{name}\" appeared as a link while it was being made"))),
                    _ => return Err(unsafe_dest(dest, &format!("\"{name}\" is not a folder"))),
                },
                Err(e) => return Err(e.into()),
            };
            if kept {
                self.ops[i].kept = true;
                self.put(&journal::kept(i))?;
            } else {
                // Recorded before anything else is done with it: a rollback removes only a folder it knows was made here.
                let fp = match made.fingerprint() {
                    Ok(fp) => fp,
                    Err(e) => {
                        let _ = dest.parent.rmdir(&name);
                        return Err(e.into());
                    }
                };
                self.ops[i].made = Some(fp);
                self.put(&journal::made(i, &fp))?;
                dest.parent.sync()?;
            }
            dest.parent = made;
            dest.logical.push(name);
            dest.missing.remove(0);
        }
        Ok(())
    }

    /// Stages a new file for `dest`: made (with the folders it needs) next to it
    /// under a name of this transaction's, filled by `write`, flushed, and journaled.
    /// The destination is only touched by [`Txn::commit`]. On an error the
    /// transaction is rolled back, and the error is the cause if that was clean.
    pub fn stage(&mut self, dest: &mut Resolved, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<Staged, Error> {
        self.ensure_open()?;
        match self.stage_inner(dest, write) {
            Ok(s) => Ok(s),
            Err(e) => Err(self.fail(e, Vec::new(), false)),
        }
    }

    fn stage_inner(&mut self, dest: &mut Resolved, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<Staged, Error> {
        self.roots.check_write(dest)?;
        let kind = match &dest.existing {
            None => OpKind::Create,
            Some(Existing::File(_)) => OpKind::Replace,
            Some(Existing::Link(_)) => {
                // A link is replaced, never written through, but only one that stays inside the game or mods folder.
                self.roots.read_dest(dest)?;
                OpKind::Replace
            }
            Some(Existing::Dir) => return Err(unsafe_dest(dest, "that is a folder, not a file")),
            Some(Existing::Other) => return Err(unsafe_dest(dest, "that is not a regular file")),
        };
        if self.touches(dest) {
            return Err(Error::Conflict(format!("{} is changed twice in one transaction", dest.path().display())));
        }
        self.make_parents(dest)?;
        let mode = if kind == OpKind::Replace && dest.parent.caps().hard_links { Mode::Link } else { Mode::Move };
        let op = JOp { kind, anchor: dest.anchor(), dir: dest.logical.clone(), name: dest.name.clone(), expect: dest.existing.clone(), mode, made: None, kept: false, did: false };
        let i = self.add_op(op, &dest.parent)?;
        let (tmp, bak) = (staged_name(&self.id, i), backup_name(&self.id, i));
        let mut file = dest.parent.create_excl(&tmp)?;
        // A replacement keeps the permission bits of the file it replaces (an executable stays one);
        // a new file, or one that replaces a link, gets the usual 0644.
        // Never setuid, setgid or sticky, and always readable by the owner: the staged file is opened again to be validated.
        let perms = match &dest.existing {
            Some(Existing::File(_)) => (dest.parent.mode_of(&dest.name)?.unwrap_or(0o644) & 0o777) | 0o400,
            _ => 0o644,
        };
        file.set_permissions(std::fs::Permissions::from_mode(perms))?;
        if mode == Mode::Link
            && let Err(e) = dest.parent.link(&dest.name, &bak)
        {
            // A filesystem that cannot hard-link (exFAT, some network mounts): keep the original by renaming it aside at commit.
            if !matches!(e.raw_os_error(), Some(libc::EPERM | libc::EOPNOTSUPP | libc::EXDEV | libc::EMLINK)) && e.kind() != io::ErrorKind::Unsupported {
                return Err(e.into());
            }
            self.ops[i].mode = Mode::Move;
            self.put(&journal::mode_move(i))?;
        }
        write(&mut file)?;
        sync_file(&file)?;
        dest.parent.sync()?;
        let fp = Fingerprint::of(&file)?;
        self.put(&journal::made(i, &fp))?;
        self.ops[i].made = Some(fp);
        self.staged[i] = Some(file);
        Ok(Staged(i))
    }

    /// [`Txn::stage`] after checking that `need` bytes (what `write` will write, over-estimated if need be)
    /// leave at least 1 GiB free on the filesystem of `dest`. Nothing is created if they do not: the error
    /// names the folder and the sizes, and ends the transaction like any error of `stage`.
    pub fn stage_sized(&mut self, dest: &mut Resolved, need: u64, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<Staged, Error> {
        self.ensure_open()?;
        if let Err(e) = enough_space(dest, need) {
            return Err(self.fail(e, Vec::new(), false));
        }
        self.stage(dest, write)
    }

    /// Plans the removal of `dest` (the link itself, if it is one); it happens at
    /// commit. Nothing to do if there is nothing there.
    pub fn delete(&mut self, dest: &Resolved) -> Result<(), Error> {
        self.ensure_open()?;
        match self.delete_inner(dest) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.fail(e, Vec::new(), false)),
        }
    }

    fn delete_inner(&mut self, dest: &Resolved) -> Result<(), Error> {
        self.roots.check_write(dest)?;
        match &dest.existing {
            None => return Ok(()),
            Some(Existing::Dir) => return Err(unsafe_dest(dest, "that is a folder, not a file")),
            Some(Existing::Other) => return Err(unsafe_dest(dest, "that is not a regular file")),
            Some(Existing::File(_) | Existing::Link(_)) => {}
        }
        if self.touches(dest) {
            return Err(Error::Conflict(format!("{} is changed twice in one transaction", dest.path().display())));
        }
        let op = JOp { kind: OpKind::Delete, anchor: dest.anchor(), dir: dest.logical.clone(), name: dest.name.clone(), expect: dest.existing.clone(), mode: Mode::Move, made: None, kept: false, did: false };
        self.add_op(op, &dest.parent)?;
        Ok(())
    }

    /// Closes the handle of a staged file that is not needed any more (a package of
    /// thousands of files would otherwise hold thousands open); [`Txn::staged`] must not be asked for it after.
    pub fn release(&mut self, s: Staged) {
        self.staged[s.0] = None;
    }

    /// The staged file, to look at before committing (for validation).
    pub fn staged(&self, s: Staged) -> &File {
        self.staged[s.0].as_ref().expect("a staged file")
    }

    /// Puts every staged file in place, in the order they were planned.
    /// Returns `log`, with a note if some backup could not be removed afterwards.
    /// On an error everything is undone: see [`Error::Txn`]. A target that was
    /// changed by somebody else since it was read is [`Error::Conflict`] and is not overwritten.
    pub fn commit(self, log: Vec<String>) -> Result<Vec<String>, Error> {
        self.commit_validated(log, |_, _| Ok(()))
    }

    /// [`Txn::commit`] after `check` has looked at each staged file, re-opened from
    /// disk (an archive must still parse, a list must name the pack). An error is a clean undo.
    pub fn commit_validated(mut self, log: Vec<String>, mut check: impl FnMut(Staged, &File) -> Result<(), Error>) -> Result<Vec<String>, Error> {
        self.ensure_open()?;
        if let Err(e) = self.validate(&mut check) {
            return Err(self.fail(e, log, false));
        }
        // The lock and the journal must still be what the names say: if somebody removed or replaced them, stop and undo.
        if let Err(e) = self.check_held() {
            return Err(self.fail(e, log, false));
        }
        if let Err(e) = self.apply_all() {
            return Err(self.fail(e, log, true));
        }
        Ok(self.finish(log))
    }

    /// The game lock is still the one the names say, and the journal file still has a name in its folder.
    fn check_held(&self) -> Result<(), Error> {
        self.roots.check_lock()?;
        if self.journal.metadata()?.nlink() == 0 || !self.jdir.names_file(&self.jname, &self.journal)? {
            return Err(Error::Io(io::Error::other(format!("the journal {} was removed or replaced while the operation ran", self.jname))));
        }
        Ok(())
    }

    fn validate(&mut self, check: &mut impl FnMut(Staged, &File) -> Result<(), Error>) -> Result<(), Error> {
        for i in 0..self.ops.len() {
            let op = &self.ops[i];
            if matches!(op.kind, OpKind::Create | OpKind::Replace) {
                let file = self.dirs[i].open_read(&staged_name(&self.id, i))?;
                let seen = Fingerprint::of(&file)?;
                if !op.made.as_ref().is_some_and(|m| same_state(&seen, m)) {
                    return Err(Error::Conflict(format!("{}: the staged file was changed before it was put in place", path_of(self.roots, op, &op.name).display())));
                }
                check(Staged(i), &file)?;
            }
            self.check_target(i)?;
        }
        Ok(())
    }

    /// The target is still what it was when the operation was planned.
    fn check_target(&self, i: usize) -> Result<(), Error> {
        let op = &self.ops[i];
        if op.kind == OpKind::Mkdir {
            return Ok(());
        }
        let seen = self.dirs[i].stat(&op.name)?;
        if matches_opt(&seen, &op.expect) {
            return Ok(());
        }
        Err(Error::Conflict(format!("{} was changed by something else after it was read; nothing was written over it", path_of(self.roots, op, &op.name).display())))
    }

    fn apply_all(&mut self) -> Result<(), Error> {
        self.committing = true;
        self.put(&journal::commit())?;
        for i in 0..self.ops.len() {
            self.apply(i)?;
        }
        self.put(&journal::committed())?;
        Ok(())
    }

    fn apply(&mut self, i: usize) -> Result<(), Error> {
        let op = &self.ops[i];
        if op.kind == OpKind::Mkdir {
            return Ok(());
        }
        self.roots.verify_at(op.anchor, &op.dir, &self.dirs[i])?;
        let dir = &self.dirs[i];
        let (tmp, bak) = (staged_name(&self.id, i), backup_name(&self.id, i));
        let conflict = |e: io::Error| if e.kind() == io::ErrorKind::AlreadyExists { Error::Conflict(format!("{} appeared while it was being put in place", path_of(self.roots, op, &op.name).display())) } else { e.into() };
        match (op.kind, op.mode) {
            (OpKind::Create, _) => dir.rename(&tmp, &op.name, false).map_err(conflict)?,
            (OpKind::Replace, Mode::Link) => {
                self.check_target(i)?;
                dir.rename(&tmp, &op.name, true)?;
            }
            (OpKind::Replace, Mode::Move) => {
                self.check_target(i)?;
                dir.rename(&op.name, &bak, false).map_err(conflict)?;
                dir.rename(&tmp, &op.name, false).map_err(conflict)?;
            }
            (OpKind::Delete, _) => {
                self.check_target(i)?;
                dir.rename(&op.name, &bak, false).map_err(conflict)?;
            }
            (OpKind::Mkdir, _) => unreachable!("skipped above"),
        }
        dir.sync()?;
        self.put(&journal::did(i))?;
        self.ops[i].did = true;
        Ok(())
    }

    /// After `committed`: remove the backups and the journal. Failing here does not
    /// undo anything (the files are in place): the journal stays and the next run finishes.
    fn finish(&mut self, mut log: Vec<String>) -> Vec<String> {
        let mut left = Vec::new();
        for (i, op) in self.ops.iter().enumerate() {
            if !matches!(op.kind, OpKind::Replace | OpKind::Delete) {
                continue;
            }
            match self.dirs[i].unlink(&backup_name(&self.id, i)) {
                Ok(()) => {
                    let _ = self.dirs[i].sync();
                }
                Err(e) if is_missing(&e) => {}
                Err(e) => left.push(format!("{}: {e}", path_of(self.roots, op, &backup_name(&self.id, i)).display())),
            }
        }
        if left.is_empty() {
            let _ = self.put(&journal::end(&End::Ok));
            let _ = self.jdir.unlink(&self.jname);
            let _ = self.jdir.sync();
        } else {
            log.push(format!("everything was installed, but these backups could not be removed and are removed at the next run: {}", left.join("; ")));
        }
        self.phase = Phase::Done;
        log
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        // A transaction dropped without a commit (an error path, a panic) is undone. If that cannot be
        // finished the journal says so and the next `begin` reports it; callers that want the report
        // at once end the transaction with an error from `stage` or `commit`, which carry it.
        self.rollback_now();
    }
}
