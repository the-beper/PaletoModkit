//! Confined access to the game folder and the mods folder. A destination is
//! resolved one name at a time on open directory handles (never as a path
//! string), so a link planted anywhere on the way cannot lead a write out of
//! the folder it must stay in, and nothing is written to the originals.
//!
//! * A link inside a root is followed only if it stays inside that root
//!   (absolute targets must start with the root's own path); a `..` that would
//!   leave the root, a loop, or a link that leaves is refused ([`Error::Escape`]).
//! * The last name is never followed for writing: a rename replaces a link and
//!   an unlink removes the link itself.
//! * Loose files in the game folder never land on the game's own data (judged
//!   on the resolved path, so `scripts -> x64` does not help), programs, the
//!   `mods` folder or the folders this tool keeps its own files in.
//! * The mods folder may be a link (`mods -> mods_profileA`, or to another
//!   drive), but not to the game folder, a folder holding it, or the game's data.
//! * Originals are read through any link, as the game does.
//!
//! Journaling and the multi-file commit are in the transaction engine
//! ([`crate::txn`]); this layer gives it directory handles that cannot be
//! redirected, and the lock that keeps two programs from changing one game at once.

/// Tells the fault injection of the tests (see `faults`) that a system call is
/// about to be made; compiles to nothing outside tests.
macro_rules! fault {
    ($point:ident) => {
        #[cfg(test)]
        {
            if let Some(e) = $crate::fsroot::faults::hit($crate::fsroot::faults::Point::$point) {
                return Err(e.into());
            }
        }
    };
}

#[cfg(test)]
pub mod faults;
mod linux;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::ThreadId;

use sha2::{Digest, Sha256};

pub use linux::{Dir, Fingerprint};
pub(crate) use linux::random;

use crate::Error;
use crate::relpath::{RelPath, is_game_data_parts};

/// What a name is, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Link,
    Other,
}

/// What a destination's last name is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    File(Fingerprint),
    Dir,
    /// A link, by its target.
    Link(OsString),
    /// A pipe, a device, a socket.
    Other,
}

/// What the filesystem and kernel can do. Tests turn parts off to exercise the
/// paths of exFAT (no hard links) and of old kernels or seccomp (no renameat2,
/// no openat2) on any filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub hard_links: bool,
    pub noreplace: bool,
    pub openat2: bool,
}

impl Caps {
    pub const ALL: Caps = Caps { hard_links: true, noreplace: true, openat2: true };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Game,
    Mods,
}

/// A folder everything below it is confined to.
#[derive(Debug)]
pub struct Root {
    pub dir: Dir,
    pub kind: RootKind,
    pub id: Fingerprint,
    /// How it was reached (links not resolved), for messages and for paths handed out.
    pub shown: PathBuf,
    /// Its real place, which absolute links inside it may start with.
    pub canon: PathBuf,
    fallback: bool,
}

/// The game folder and, if there is one, its mods folder.
#[derive(Debug)]
pub struct GameRoots {
    pub game: Root,
    /// Set once the folder exists (the first write may create it, after this was opened).
    mods: OnceLock<Root>,
    /// Why the mods folder cannot be used: its path and the reason.
    refused: OnceLock<(PathBuf, String)>,
    lock: Mutex<LockSlot>,
    /// The tool's data folder, where the lock and the journals go if the game folder cannot be written to.
    data: PathBuf,
}

/// The game lock as this `GameRoots` holds it. The lock itself is an `flock` on
/// `.paleto/lock`, which belongs to the open file, so every `GameRoots` (and so
/// every operation and every thread of a program) excludes every other one;
/// inside one `GameRoots` the thread that holds it may take it again.
#[derive(Debug, Default)]
struct LockSlot {
    file: Option<File>,
    /// The folder `file` is in (`.paleto` of the game, or the fallback in the data folder).
    home: Option<Dir>,
    fallback: bool,
    depth: usize,
    owner: Option<ThreadId>,
    label: String,
    /// A transaction is open (only one at a time).
    txn: bool,
}

/// Holds the game lock; releases it when the last guard of the thread is dropped.
/// Not `Send`: the lock belongs to the thread that took it (and so does a transaction that holds one).
#[derive(Debug)]
pub struct LockGuard<'r> {
    roots: &'r GameRoots,
    txn: bool,
    /// The first taking of the lock, not a re-entry of the thread that holds it.
    outer: bool,
    _not_send: PhantomData<*const ()>,
}

// Runs once between the locking of the lock file and the check that it is still the one named (tests swap it there).
#[cfg(test)]
thread_local! {
    pub(crate) static AFTER_FLOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// A destination found by [`GameRoots::dest`], not yet written.
#[derive(Debug)]
pub struct Resolved {
    /// The folder the write must stay in.
    pub root: RootKind,
    /// The deepest folder that exists on the way (a handle: later calls cannot be redirected).
    pub parent: Dir,
    /// The link-free names from the root to `parent`.
    pub logical: Vec<String>,
    /// The name as it is on disk (or as the package spells it if it is new).
    pub name: String,
    pub existing: Option<Existing>,
    /// Folders still to be made between `parent` and `name`.
    pub missing: Vec<String>,
    /// Which root `logical` is relative to (the game's, while the mods folder is yet to be made).
    anchor: RootKind,
    base: PathBuf,
    /// Set by [`GameRoots::owned_dest`]: the tool's own folder this lies in, which the write policy then accepts.
    owned: Option<Owned>,
}

/// A folder of the game folder the tool keeps its own files in, where a transaction may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owned {
    /// `mods_disabled/`: parts of the mods put aside.
    Disabled,
    /// `mods_legacy/`: backups of archives before a conversion.
    Legacy,
}

impl Owned {
    fn folder(self) -> &'static str {
        match self {
            Owned::Disabled => "mods_disabled",
            Owned::Legacy => "mods_legacy",
        }
    }

    /// The owned folder `name` (the first name of a path in the game folder) is, spelled exactly: the tool only
    /// ever makes these names, so another case is somebody else's folder (on a case-sensitive filesystem).
    fn of(name: &str) -> Option<Owned> {
        [Owned::Disabled, Owned::Legacy].into_iter().find(|o| name == o.folder())
    }
}

/// Why a folder the tool keeps its own files in must not be written to, judged on identities because a mount
/// point is not a link and gets past `O_NOFOLLOW`: it is on another device than the game folder, or is
/// one of the `others` (the game folder, the mods folder, the game's `x64` and `update`), which is what a bind mount of
/// one of them over it looks like.
fn owned_refusal(folder: &Fingerprint, game: &Fingerprint, others: &[(&str, Fingerprint)]) -> Option<String> {
    if folder.dev != game.dev {
        return Some("is on another device than the game folder (a mount point)".to_string());
    }
    others.iter().find(|(_, o)| folder.same_file(o)).map(|(name, _)| format!("is the {name} folder under another name (a mount point)"))
}

impl Resolved {
    /// The root `logical` is relative to.
    pub(crate) fn anchor(&self) -> RootKind {
        self.anchor
    }

    /// Where it is, as a path through the root as it was reached (links inside the root resolved).
    pub fn path(&self) -> PathBuf {
        let mut path = self.base.clone();
        path.extend(self.logical.iter().chain(&self.missing));
        path.push(&self.name);
        path
    }

    /// The names from the root to the destination, link-free.
    pub fn relative(&self) -> Vec<String> {
        self.logical.iter().chain(&self.missing).chain(std::iter::once(&self.name)).cloned().collect()
    }
}

struct Walked {
    stack: Vec<Dir>,
    logical: Vec<String>,
    missing: Vec<String>,
    name: String,
    existing: Option<Existing>,
}

/// Folders the tool keeps its own files in, at the top of the game folder.
fn tool_owned(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [".paleto", "mods_snapshots", "mods_disabled", "mods_legacy"].contains(&name.as_str()) || name.starts_with(".mods.") || name.starts_with(".paleto-")
}

/// Original game data by its top-level name: what the game streams, or a program.
fn original_name(name: &str) -> bool {
    is_game_data_parts(&[name.to_string()]) || name.to_ascii_lowercase().ends_with(".exe")
}

fn unsafe_path(path: &Path, why: impl Into<String>) -> Error {
    Error::UnsafePath { path: path.display().to_string(), why: why.into() }
}

fn changed(path: &Path) -> Error {
    Error::Io(io::Error::other(format!("{}: the folder changed while it was being used", path.display())))
}

/// `path` with `.` and `..` resolved as written (links are not looked at).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn normal_parts(path: &Path) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(n) => out.push(n.to_str().ok_or_else(|| unsafe_path(path, "a link whose target is not valid UTF-8"))?.to_string()),
            Component::ParentDir => out.push("..".into()),
            _ => {}
        }
    }
    Ok(out)
}

/// The entry of `dir` called `name`, or else the one that differs only in ASCII
/// case (game files are named inconsistently): exact wins, else the first in order.
pub(crate) fn find_ci(dir: &Dir, name: &str) -> io::Result<Option<(String, Kind)>> {
    if let Some(kind) = dir.kind(name)? {
        return Ok(Some((name.to_string(), kind)));
    }
    let mut hits: Vec<String> = dir.names()?.into_iter().filter(|n| n.eq_ignore_ascii_case(name)).collect();
    hits.sort();
    for hit in hits {
        if let Some(kind) = dir.kind(&hit)? {
            return Ok(Some((hit, kind)));
        }
    }
    Ok(None)
}

const MAX_HOPS: usize = 40;
const MAX_DEPTH: usize = 64;

impl Root {
    /// Whether a link at `through` may be followed to `target`, and the names to go on with.
    fn splice(&self, target: &OsStr, shown: &str, through: &Path) -> Result<(bool, Vec<String>), Error> {
        let t = Path::new(target);
        let rest = if t.is_absolute() {
            t.strip_prefix(&self.canon).or_else(|_| t.strip_prefix(&self.shown)).map_err(|_| Error::Escape { path: shown.into(), through: through.display().to_string() })?
        } else {
            t
        };
        Ok((t.is_absolute(), normal_parts(rest)?))
    }

    /// Resolves `comps` from this root, following links that stay inside it.
    /// `follow_final` also follows a link at the last name (to read through it).
    fn walk(&self, comps: &[String], follow_final: bool, shown: &str) -> Result<Walked, Error> {
        let mut stack = vec![self.dir.try_clone()?];
        let mut logical: Vec<String> = Vec::new();
        let mut pending: VecDeque<String> = comps.iter().cloned().collect();
        let (mut hops, mut retried) = (0, false);
        let mut through = self.shown.clone();
        let bad = |why: &str| Error::UnsafePath { path: shown.to_string(), why: why.to_string() };
        while let Some(c) = pending.pop_front() {
            if c == ".." {
                if logical.is_empty() {
                    return Err(Error::Escape { path: shown.to_string(), through: through.display().to_string() });
                }
                stack.pop();
                logical.pop();
                continue;
            }
            let last = pending.is_empty();
            let top = stack.last().expect("the root is never popped");
            let Some((actual, kind)) = find_ci(top, &c)? else {
                let mut rest: Vec<String> = std::iter::once(c).chain(pending).collect();
                let name = rest.pop().expect("at least this name");
                if rest.iter().any(|r| r == "..") {
                    return Err(bad("a link to a folder that does not exist"));
                }
                return Ok(Walked { stack, logical, missing: rest, name, existing: None });
            };
            match kind {
                Kind::Dir if !last || follow_final => match top.open_dir(&actual) {
                    Ok(dir) => {
                        stack.push(dir);
                        logical.push(actual);
                        retried = false;
                        if stack.len() > MAX_DEPTH {
                            return Err(bad("more than 64 folders deep"));
                        }
                    }
                    // A folder swapped for a link or a file since we looked: look again once.
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) && !retried => {
                        retried = true;
                        pending.push_front(c);
                    }
                    Err(e) => return Err(e.into()),
                },
                Kind::Dir | Kind::File | Kind::Other if last => {
                    let existing = top.stat(&actual)?;
                    return Ok(Walked { stack, logical, missing: Vec::new(), name: actual, existing });
                }
                Kind::File | Kind::Other => return Err(bad(&format!("\"{actual}\" is a file, not a folder"))),
                Kind::Link if last && !follow_final => {
                    let existing = top.stat(&actual)?;
                    return Ok(Walked { stack, logical, missing: Vec::new(), name: actual, existing });
                }
                Kind::Dir => unreachable!("a folder is entered above"),
                Kind::Link => {
                    hops += 1;
                    if hops > MAX_HOPS {
                        return Err(bad("too many links in a row (a link loop?)"));
                    }
                    through = self.shown.join(logical.iter().collect::<PathBuf>()).join(&actual);
                    let target = top.read_link(&actual)?;
                    let (absolute, comps) = self.splice(&target, shown, &through)?;
                    if absolute {
                        stack.truncate(1);
                        logical.clear();
                    }
                    for comp in comps.into_iter().rev() {
                        pending.push_front(comp);
                    }
                }
            }
        }
        Err(bad("that is a folder, not a file"))
    }

    /// Opens a file by `comps`, following every link, as the game does (the
    /// originals of a game may live on another drive). `None` if there is none.
    fn open_free(&self, comps: &[String]) -> Result<Option<File>, Error> {
        let mut dir = self.dir.try_clone()?;
        for (i, c) in comps.iter().enumerate() {
            let Some((actual, _)) = find_ci(&dir, c)? else { return Ok(None) };
            let missing = |e: &io::Error| matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR));
            if i + 1 == comps.len() {
                // Only a regular file is ever opened: not what a link leads to if that is a pipe or a device.
                if dir.kind_following(&actual)? != Some(Kind::File) {
                    return Ok(None);
                }
                return match dir.open_read_following(&actual) {
                    Ok(file) => Ok(Some(file)),
                    Err(e) if missing(&e) || e.kind() == io::ErrorKind::InvalidInput => Ok(None),
                    Err(e) => Err(e.into()),
                };
            }
            dir = match dir.open_dir_following(&actual) {
                Ok(d) => d,
                Err(e) if missing(&e) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
        }
        Ok(None)
    }

    /// Checks that `held` is still the folder at `logical` of this root, and
    /// that nothing on the way is a link: openat2 where there is one, else the
    /// same walk by hand.
    pub fn verify(&self, logical: &[String], held: &Dir) -> Result<(), Error> {
        let want = held.identity()?;
        let rel = if logical.is_empty() { ".".to_string() } else { logical.join("/") };
        let mut got = None;
        if !self.fallback && self.dir.caps().openat2 {
            match self.dir.identity_beneath(&rel) {
                Ok(id) => got = Some(id),
                Err(e) if linux::syscall_missing(&e) => linux::mark_openat2_missing(),
                Err(_) => return Err(changed(&self.shown.join(&rel))),
            }
        }
        let got = match got {
            Some(id) => id,
            None => {
                let mut dir = self.dir.try_clone()?;
                for name in logical {
                    dir = dir.open_dir(name).map_err(|_| changed(&self.shown.join(&rel)))?;
                }
                dir.identity()?
            }
        };
        if got == want { Ok(()) } else { Err(changed(&self.shown.join(&rel))) }
    }

    /// Makes `verify` use the walk by hand, as on a kernel without openat2.
    #[cfg(test)]
    pub fn force_fallback(&mut self) {
        self.fallback = true;
    }
}

/// The mods folder if it is usable, or the reason it is not: its path and why.
type ModsFolder = (Option<Root>, Option<(PathBuf, String)>);

/// Opens the mods folder of a game folder and checks that it is a place the
/// tool may write to; neither a folder nor a reason if there is none.
fn open_mods(game: &Root) -> Result<ModsFolder, Error> {
    let shown = game.shown.join("mods");
    let refuse = |why: &str| Ok((None, Some((shown.clone(), why.to_string()))));
    let dir = match game.dir.kind("mods")? {
        None => return Ok((None, None)),
        Some(Kind::Dir) => game.dir.open_dir("mods")?,
        Some(Kind::Link) => match game.dir.open_dir_following("mods") {
            Ok(dir) => dir,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)) => return refuse("is a link to something that is not a folder"),
            Err(e) => return Err(e.into()),
        },
        Some(_) => return refuse("is not a folder"),
    };
    let Ok(canon) = std::fs::canonicalize(&shown) else { return refuse("cannot be resolved") };
    let id = dir.fingerprint()?;
    if std::fs::metadata(&canon).map(|m| (m.dev(), u128::from(m.ino()))).ok() != Some((id.dev, id.ino)) {
        return Err(changed(&shown));
    }
    if id.same_file(&game.id) {
        return refuse("is the game folder itself");
    }
    if game.canon.starts_with(&canon) {
        return refuse("is a folder that holds the game folder");
    }
    if let Ok(rel) = canon.strip_prefix(&game.canon) {
        let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        if original_name(&first) {
            return refuse("is inside the game's own data");
        }
        if tool_owned(&first) {
            return refuse("is inside a folder this tool keeps its own files in");
        }
    }
    // The game's data may be links to other drives: the mods folder must not overlap those places.
    for name in game.dir.names()? {
        if original_name(&name)
            && game.dir.kind(&name)? == Some(Kind::Link)
            && let Ok(real) = std::fs::canonicalize(game.canon.join(&name))
            && (canon.starts_with(&real) || real.starts_with(&canon))
        {
            return refuse("overlaps the game's own data");
        }
    }
    Ok((Some(Root { dir, kind: RootKind::Mods, id, shown, canon, fallback: false }), None))
}

impl GameRoots {
    /// Opens the game folder (following links to it: a Steam library is often
    /// reached through one) and its mods folder.
    pub fn open(root: &Path) -> Result<Self, Error> {
        let shown = lexical(&std::path::absolute(root)?);
        let dir = Dir::open_path(&shown)?;
        let canon = std::fs::canonicalize(&shown)?;
        let id = dir.fingerprint()?;
        if std::fs::metadata(&canon).map(|m| (m.dev(), u128::from(m.ino()))).ok() != Some((id.dev, id.ino)) {
            return Err(changed(&shown));
        }
        let game = Root { dir, kind: RootKind::Game, id, shown, canon, fallback: false };
        let roots = GameRoots { game, mods: OnceLock::new(), refused: OnceLock::new(), lock: Mutex::new(LockSlot::default()), data: crate::game::data_dir() };
        // A mods folder that cannot be used is only an error for what needs it.
        match roots.mods_state() {
            Ok(_) | Err(Error::UnsafePath { .. }) => Ok(roots),
            Err(e) => Err(e),
        }
    }

    /// The mods folder if it exists and may be used; `None` if there is none (yet).
    pub fn mods(&self) -> Option<&Root> {
        self.mods.get()
    }

    /// Looks at the mods folder again if it was not there or not usable when last looked at.
    fn mods_state(&self) -> Result<Option<&Root>, Error> {
        if let Some(mods) = self.mods.get() {
            return Ok(Some(mods));
        }
        if self.refused.get().is_none() {
            match open_mods(&self.game)? {
                (Some(mods), _) => {
                    let _ = self.mods.set(mods);
                    return Ok(self.mods.get());
                }
                (None, Some(refused)) => {
                    let _ = self.refused.set(refused);
                }
                (None, None) => return Ok(None),
            }
        }
        let (path, why) = self.refused.get().expect("set above");
        Err(unsafe_path(path, format!("the mods folder {why}")))
    }

    fn root(&self, kind: RootKind) -> Result<&Root, Error> {
        match kind {
            RootKind::Game => Ok(&self.game),
            RootKind::Mods => self.mods.get().ok_or_else(|| unsafe_path(&self.game.shown.join("mods"), "the mods folder does not exist")),
        }
    }

    /// Where a game-relative path is written: data files the game streams
    /// (archives, `update/`, `x64/`) and anything under `mods/` go to the mods
    /// folder, everything else (ASI plugins, scripts, configs) to the game folder.
    pub fn dest(&self, p: &RelPath) -> Result<Resolved, Error> {
        if p.in_mods_folder() {
            self.resolve(RootKind::Mods, &p.parts()[1..], p)
        } else if p.is_game_data() {
            self.resolve(RootKind::Mods, p.parts(), p)
        } else {
            self.resolve(RootKind::Game, p.parts(), p)
        }
    }

    /// The path in the mods folder, whatever it is (for copies of originals).
    pub fn mods_dest(&self, p: &RelPath) -> Result<Resolved, Error> {
        self.resolve(RootKind::Mods, p.parts(), p)
    }

    /// Refuses `top`, a held tool-owned folder of the game folder, if it is the game folder, the mods folder or
    /// the game's `x64` or `update` under another name, or on another device (see [`owned_refusal`]).
    fn check_owned_folder(&self, top: &Dir, shown: &Path) -> Result<(), Error> {
        let mut others = vec![("game", self.game.id)];
        for name in ["x64", "update"] {
            if let Ok(Some((actual, _))) = find_ci(&self.game.dir, name)
                && let Ok(dir) = self.game.dir.open_dir_following(&actual)
                && let Ok(id) = dir.fingerprint()
            {
                others.push((name, id));
            }
        }
        if let Some(mods) = self.mods.get() {
            others.push(("mods", mods.id));
        }
        match owned_refusal(&top.fingerprint()?, &self.game.id, &others) {
            Some(why) => Err(unsafe_path(shown, format!("this tool's own folder {why}"))),
            None => Ok(()),
        }
    }

    /// `rel` in one of the tool's own folders of the game folder, for a transaction to write (a backup, a part put
    /// aside): the names are matched exactly, a link or a file anywhere on the way (the folder itself and the last
    /// name included) is refused so nothing is ever written through one, and the folders that are missing are made by
    /// the transaction. The write policy accepts the result only for its own `top`.
    pub fn owned_dest(&self, top: Owned, rel: &RelPath) -> Result<Resolved, Error> {
        let names: Vec<&str> = std::iter::once(top.folder()).chain(rel.parts().iter().map(String::as_str)).collect();
        let (name, folders) = names.split_last().expect("the folder and at least one name");
        let mut parent = self.game.dir.try_clone()?;
        let mut shown = self.game.shown.clone();
        let (mut logical, mut missing) = (Vec::new(), Vec::new());
        for folder in folders {
            shown.push(folder);
            if !missing.is_empty() {
                missing.push(folder.to_string());
                continue;
            }
            match parent.kind(folder)? {
                None => missing.push(folder.to_string()),
                Some(Kind::Dir) => match parent.open_dir(folder) {
                    Ok(dir) => {
                        if logical.is_empty() {
                            self.check_owned_folder(&dir, &shown)?;
                        }
                        parent = dir;
                        logical.push(folder.to_string());
                    }
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => return Err(unsafe_path(&shown, "this tool's own folder is a link or a file, not a folder")),
                    Err(e) => return Err(e.into()),
                },
                Some(_) => return Err(unsafe_path(&shown, "this tool's own folder is a link or a file, not a folder")),
            }
        }
        shown.push(name);
        let existing = if missing.is_empty() { parent.stat(name)? } else { None };
        if matches!(existing, Some(Existing::Link(_))) {
            return Err(unsafe_path(&shown, "that is a link, and nothing is written through one in this tool's own folder"));
        }
        Ok(Resolved { root: RootKind::Game, parent, logical, name: name.to_string(), existing, missing, anchor: RootKind::Game, base: self.game.shown.clone(), owned: Some(top) })
    }

    fn resolve(&self, kind: RootKind, comps: &[String], p: &RelPath) -> Result<Resolved, Error> {
        let shown = p.to_string();
        let root = match (kind, if kind == RootKind::Mods { self.mods_state()? } else { None }) {
            (RootKind::Game, _) => &self.game,
            (RootKind::Mods, Some(mods)) => mods,
            (RootKind::Mods, None) => {
                // No mods folder yet: the first write makes it, then the folders below it.
                let mut missing = vec!["mods".to_string()];
                missing.extend(comps[..comps.len() - 1].iter().cloned());
                return Ok(Resolved { root: kind, parent: self.game.dir.try_clone()?, logical: Vec::new(), name: comps[comps.len() - 1].clone(), existing: None, missing, anchor: RootKind::Game, base: self.game.shown.clone(), owned: None });
            }
        };
        let mut w = root.walk(comps, false, &shown)?;
        // The mods folder is no place for loose files, however it is reached (`mods` may be a
        // link to `mods_profileA`, and `scripts` another to it): compare the folders themselves.
        let mods = match (kind, self.mods_state()) {
            (RootKind::Game, Ok(mods)) => mods,
            // A mods folder that cannot be used cannot be written to either.
            (RootKind::Game, Err(Error::UnsafePath { .. })) | (RootKind::Mods, _) => None,
            (RootKind::Game, Err(e)) => return Err(e),
        };
        if let Some(mods) = mods {
            for dir in &w.stack {
                if dir.fingerprint()?.same_file(&mods.id) {
                    return Err(unsafe_path(&root.shown.join(w.logical.iter().collect::<PathBuf>()), "that is the mods folder, reached by another name or a link"));
                }
            }
        }
        let r = Resolved { root: kind, parent: w.stack.pop().expect("the root"), logical: w.logical, name: w.name, existing: w.existing, missing: w.missing, anchor: kind, base: root.shown.clone(), owned: None };
        self.policy(&r, false)?;
        Ok(r)
    }

    /// What may never be written, judged on the resolved path. `exe` also
    /// refuses programs (checked when writing; the caller may want to skip them quietly).
    fn policy(&self, r: &Resolved, exe: bool) -> Result<(), Error> {
        let bad = |why: &str| unsafe_path(&r.path(), why);
        if exe && r.name.to_ascii_lowercase().ends_with(".exe") {
            return Err(bad("programs are never replaced or deleted"));
        }
        if exe && self.is_game_file(r) {
            return Err(bad("is one of the game's own files in the game folder: packages may not replace or delete .dll or .txt files there (mod loaders excepted)"));
        }
        if r.root == RootKind::Game {
            let full = r.relative();
            if full[0].eq_ignore_ascii_case("mods") {
                return Err(bad("that is the mods folder, reached by a link"));
            }
            // The tool's own folders are written only through `owned_dest`, and only in the one it was made for
            // (what it keeps there, backups of archives, is named like game data below it, and is not).
            match r.owned {
                Some(o) if full[0] == o.folder() => {}
                Some(_) => return Err(bad("that is not in the folder this tool made it for")),
                None if is_game_data_parts(&full) => return Err(bad("that is the game's own data (a link leads there); changes go to the mods folder")),
                None if tool_owned(&full[0]) => return Err(bad("that folder belongs to this tool")),
                None => {}
            }
        } else if r.owned.is_some() {
            return Err(bad("that is not in the game folder"));
        }
        Ok(())
    }

    /// Whether `r` is an existing `.dll` or `.txt` file directly in the game
    /// folder (its own libraries and notes), which packages may not replace or
    /// delete. Mod loaders (`dinput8.dll`, `ScriptHookV.dll`, `ScriptHookVDotNet*`)
    /// and `.asi` plugins are not among them, nor are new files or those in subfolders.
    pub fn is_game_file(&self, r: &Resolved) -> bool {
        let name = r.name.to_ascii_lowercase();
        let loader = name == "dinput8.dll" || name == "scripthookv.dll" || name.starts_with("scripthookvdotnet");
        r.root == RootKind::Game && r.logical.is_empty() && r.missing.is_empty() && !matches!(r.existing, None | Some(Existing::Dir)) && (name.ends_with(".dll") || name.ends_with(".txt")) && !loader
    }

    /// Whether the mods folder is not there (as opposed to there but not usable).
    pub(crate) fn mods_missing(&self) -> bool {
        matches!(self.mods_state(), Ok(None))
    }

    /// The write policy again for an operation a journal names, which recovery runs before it touches
    /// anything: a journal is a file in a folder others may write to, and must not be able to point recovery at
    /// the game's own data, a program, the mods folder by another name or the tool's folders. `folder`: the
    /// operation makes a folder; `replaces`: it replaces or deletes a file; `ours`: the file at the target is the very one this operation staged (it matches the journal's `made`), which the rule for the game's own `.dll` and `.txt` files does not cover; `mods_made`: the same journal makes
    /// the mods folder (its operations below it are then anchored at the game folder).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_journal_op(&self, anchor: RootKind, dir: &[String], name: &str, folder: bool, replaces: bool, ours: bool, mods_made: bool) -> Result<(), Error> {
        let shown = self.anchor_root(anchor).map_or_else(|_| self.game.shown.clone(), |r| r.shown.clone()).join(dir.iter().collect::<PathBuf>()).join(name);
        let bad = |why: &str| unsafe_path(&shown, why);
        let first_is_mods = dir.first().is_some_and(|d| d.eq_ignore_ascii_case("mods"));
        if anchor == RootKind::Game && mods_made && folder && dir.is_empty() && name.eq_ignore_ascii_case("mods") {
            return Ok(());
        }
        let root = if anchor == RootKind::Game && mods_made && first_is_mods { RootKind::Mods } else { anchor };
        let lower = name.to_ascii_lowercase();
        if !folder && lower.ends_with(".exe") {
            return Err(bad("programs are never replaced or deleted"));
        }
        if root == RootKind::Game {
            let loader = lower == "dinput8.dll" || lower == "scripthookv.dll" || lower.starts_with("scripthookvdotnet");
            if !ours && !folder && dir.is_empty() && (lower.ends_with(".dll") || lower.ends_with(".txt")) && !loader && (replaces || self.game.dir.kind(name)?.is_some()) {
                return Err(bad("is one of the game's own files in the game folder"));
            }
            let full: Vec<String> = dir.iter().cloned().chain(std::iter::once(name.to_string())).collect();
            if full[0].eq_ignore_ascii_case("mods") {
                return Err(bad("that is the mods folder, reached by a link"));
            }
            // What the tool itself writes in `mods_disabled` and `mods_legacy` (the folder, or something below it, an
            // archive's backup included) may be undone; `.paleto`, `mods_snapshots`, `.mods.*` and the rest of its names never are.
            let in_owned = Owned::of(&full[0]).is_some() && (!dir.is_empty() || folder);
            if !in_owned && is_game_data_parts(&full) {
                return Err(bad("that is the game's own data"));
            }
            if !in_owned && tool_owned(&full[0]) {
                return Err(bad("that folder belongs to this tool"));
            }
            if in_owned {
                match self.game.dir.kind(&full[0])? {
                    None => {}
                    Some(Kind::Dir) => match self.game.dir.open_dir(&full[0]) {
                        Ok(top) => self.check_owned_folder(&top, &self.game.shown.join(&full[0]))?,
                        Err(e) => return Err(bad(&format!("this tool's own folder cannot be opened: {e}"))),
                    },
                    Some(_) => return Err(bad("this tool's own folder is a link or a file, not a folder")),
                }
            }
            if let Some(mods) = self.mods.get() {
                let mut at = self.game.dir.try_clone()?;
                for step in dir {
                    at = match at.open_dir(step) {
                        Ok(d) => d,
                        Err(_) => break,
                    };
                    if at.fingerprint()?.same_file(&mods.id) {
                        return Err(bad("that is the mods folder, reached by another name or a link"));
                    }
                }
            }
        }
        Ok(())
    }

    /// What every write to `r` must pass, whatever writes it: the policy
    /// (programs, game data, the mods folder, the tool's folders) and the check
    /// that the folders are still where they were found.
    pub fn check_write(&self, r: &Resolved) -> Result<(), Error> {
        self.policy(r, true)?;
        self.verify(r)
    }

    /// Checks the folders of `r` are still where they were found.
    pub fn verify(&self, r: &Resolved) -> Result<(), Error> {
        self.root(r.anchor)?.verify(&r.logical, &r.parent)
    }

    /// The root of this kind, looking at the mods folder again if it was not there before.
    pub(crate) fn anchor_root(&self, kind: RootKind) -> Result<&Root, Error> {
        match kind {
            RootKind::Game => Ok(&self.game),
            RootKind::Mods => self.mods_state()?.ok_or_else(|| unsafe_path(&self.game.shown.join("mods"), "the mods folder does not exist")),
        }
    }

    /// [`Root::verify`] for the root of this kind.
    pub(crate) fn verify_at(&self, anchor: RootKind, logical: &[String], held: &Dir) -> Result<(), Error> {
        self.anchor_root(anchor)?.verify(logical, held)
    }

    /// The folder at `logical` below a root, entered one name at a time and never through a link.
    pub(crate) fn open_anchor(&self, anchor: RootKind, logical: &[String]) -> Result<Dir, Error> {
        let mut dir = self.anchor_root(anchor)?.dir.try_clone()?;
        for name in logical {
            dir = dir.open_dir(name)?;
        }
        Ok(dir)
    }

    /// `.paleto/<names>` in the game folder, where the tool keeps its lock and
    /// journals (see [`GameRoots::owned_folder`]).
    pub(crate) fn tool_folder(&self, names: &[&str], create: bool) -> Result<Option<Dir>, Error> {
        self.owned_folder(".paleto", names, create)
    }

    /// `<top>/<names>` in the game folder, a folder this tool keeps its own files in
    /// (`.paleto`, `mods_disabled`): made if `create`, `None` if it is not there. A link
    /// or a file on the way is refused, so nothing is ever written through one.
    pub(crate) fn owned_folder(&self, top: &str, names: &[&str], create: bool) -> Result<Option<Dir>, Error> {
        let mut dir = self.game.dir.try_clone()?;
        let mut shown = self.game.shown.clone();
        for name in std::iter::once(top).chain(names.iter().copied()) {
            shown.push(name);
            let next = if create {
                dir.open_or_make_dir(name)
            } else {
                match dir.kind(name)? {
                    None => return Ok(None),
                    Some(_) => dir.open_dir(name),
                }
            };
            dir = match next {
                Ok(d) => d,
                Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => return Err(unsafe_path(&shown, "this tool's own folder is a link or a file, not a folder")),
                Err(e) => return Err(e.into()),
            };
        }
        Ok(Some(dir))
    }

    /// Takes the lock of this game, which every change to the game or mods folder
    /// holds for its whole run: [`Error::Busy`] if another program, another
    /// `GameRoots` or another thread holds it. The thread that holds it may take it
    /// again, so code under a transaction can call helpers that lock. The lock is
    /// the kernel's: it is gone when the holder exits or dies, never stale.
    pub fn lock(&self, label: &str) -> Result<LockGuard<'_>, Error> {
        let me = std::thread::current().id();
        let mut slot = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.depth > 0 {
            if slot.owner == Some(me) {
                slot.depth += 1;
                return Ok(LockGuard { roots: self, txn: false, outer: false, _not_send: PhantomData });
            }
            return Err(Error::Busy(format!("{} (in this program)", slot.label)));
        }
        let (file, home, fallback) = self.acquire(label)?;
        (slot.file, slot.home, slot.fallback) = (Some(file), Some(home), fallback);
        (slot.depth, slot.owner, slot.label, slot.txn) = (1, Some(me), label.to_string(), false);
        Ok(LockGuard { roots: self, txn: false, outer: true, _not_send: PhantomData })
    }

    /// Opens and locks `.paleto/lock`; the file is never deleted. If the game folder cannot be written to
    /// (read-only mount, no permission), the lock and the journals go to the fallback home in the data
    /// folder instead, so that the mods folder (which may be on another drive) can still be written.
    /// Two programs that cannot both write the game folder may then not see each other's lock.
    fn acquire(&self, label: &str) -> Result<(File, Dir, bool), Error> {
        match self.acquire_at(false, label) {
            Err(Error::Io(first)) if matches!(first.raw_os_error(), Some(libc::EROFS | libc::EACCES | libc::EPERM)) => match self.acquire_at(true, label) {
                Ok((file, home)) => Ok((file, home, true)),
                Err(Error::Busy(who)) => Err(Error::Busy(who)),
                Err(second) => Err(Error::Io(io::Error::new(first.kind(), format!("{}: the game folder cannot be written to ({first}), and the fallback for the lock and the journal in the data folder cannot be used ({second})", self.game.shown.display())))),
            },
            other => other.map(|(file, home)| (file, home, false)),
        }
    }

    fn acquire_at(&self, fallback: bool, label: &str) -> Result<(File, Dir), Error> {
        let shown = self.lock_path(fallback);
        for _ in 0..2 {
            let home = if fallback { self.data_home(true)?.expect("made above") } else { self.tool_folder(&[], true)?.expect("made above") };
            let file = home.open_lock("lock", true).map_err(|e| if e.raw_os_error() == Some(libc::ELOOP) { unsafe_path(&shown, "the lock file is a link") } else { e.into() })?;
            if !file.metadata()?.is_file() {
                return Err(unsafe_path(&shown, "the lock file is not a regular file"));
            }
            if !Dir::try_lock(&file)? {
                return Err(Error::Busy(holder_of(&file)));
            }
            #[cfg(test)]
            if let Some(hook) = AFTER_FLOCK.with(|h| h.borrow_mut().take()) {
                hook();
            }
            // The file, or the folder it is in, may have been replaced between the open and the lock: lock what the names mean now.
            let current = (|| Ok::<_, Error>(home.names_file("lock", &file)? && self.is_home(&home, fallback)?))();
            if let Ok(true) = current {
                // What it says is only for whoever finds the lock busy.
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let text = format!("{} {} {}\n", std::process::id(), now, label.replace(['\n', '\r'], " "));
                let _ = file.set_len(0).and_then(|()| std::os::unix::fs::FileExt::write_all_at(&file, text.as_bytes(), 0));
                return Ok((file, home));
            }
            // Let go now, on an error too: closing the file would not while a child forked meanwhile by another thread still has it open.
            Dir::unlock(&file);
            current?;
        }
        Err(changed(&shown))
    }

    fn lock_path(&self, fallback: bool) -> PathBuf {
        if fallback { self.data.join("journal").join(self.fallback_name()).join("lock") } else { self.game.shown.join(".paleto/lock") }
    }

    /// The name of this game's fallback home: the first 16 hex digits of the SHA-256 of its real path.
    fn fallback_name(&self) -> String {
        Sha256::digest(self.game.canon.as_os_str().as_bytes())[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The fallback home `<data>/journal/<hash>` (it holds `lock` and the journals), made 0700 if `create`; `None` if it is not there.
    /// Each folder below the data folder is the user's own and never a link.
    pub(crate) fn data_home(&self, create: bool) -> Result<Option<Dir>, Error> {
        if create {
            std::fs::create_dir_all(&self.data)?;
        }
        let mut dir = match Dir::open_path(&self.data) {
            Ok(d) => d,
            Err(e) if !create && matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut shown = self.data.clone();
        for name in ["journal".to_string(), self.fallback_name()] {
            shown.push(&name);
            if !create && dir.kind(&name)?.is_none() {
                return Ok(None);
            }
            dir = match dir.open_private(&name, create) {
                Ok(d) => d,
                Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => return Err(unsafe_path(&shown, "this tool's own folder is a link or a file, not a folder")),
                Err(e) => return Err(e.into()),
            };
        }
        Ok(Some(dir))
    }

    /// Whether `home` is still what the game's `.paleto` (or the fallback home) names.
    fn is_home(&self, home: &Dir, fallback: bool) -> Result<bool, Error> {
        if !fallback {
            return Ok(self.game.dir.names_dir(".paleto", home)?);
        }
        Ok(match self.data_home(false)? {
            Some(now) => now.identity()? == home.identity()?,
            None => false,
        })
    }

    /// Checks that the lock this thread holds is still what the names say: `lock` is still the locked file and
    /// `.paleto` (or the fallback home) still the folder it is in. Whoever removed or replaced them could let
    /// another program lock the game while this one goes on, so a change checks it before it commits.
    pub(crate) fn check_lock(&self) -> Result<(), Error> {
        let slot = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (Some(file), Some(home)) = (&slot.file, &slot.home) else { return Err(Error::Io(io::Error::other("the game lock is not held"))) };
        if home.names_file("lock", file)? && self.is_home(home, slot.fallback)? {
            return Ok(());
        }
        Err(unsafe_path(&self.lock_path(slot.fallback), "the lock file or its folder was removed or replaced while the operation ran, so another program may hold the lock too"))
    }

    /// The folder for the journals of the lock this thread holds, made if needed: `.paleto/journal`, or the fallback home itself.
    pub(crate) fn journal_dir(&self) -> Result<Dir, Error> {
        let (home, fallback) = {
            let slot = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(home) = &slot.home else { return Err(Error::Io(io::Error::other("the game lock is not held"))) };
            (home.try_clone()?, slot.fallback)
        };
        if fallback {
            return Ok(home);
        }
        home.open_or_make_dir("journal").map_err(|e| if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) { unsafe_path(&self.game.shown.join(".paleto/journal"), "this tool's own folder is a link or a file, not a folder") } else { e.into() })
    }

    /// Every folder of journals there is, in both places (`.paleto/journal` of the game and the fallback home); none is made.
    /// A fallback home that cannot be looked at is left out.
    pub(crate) fn journal_dirs(&self) -> Result<Vec<Dir>, Error> {
        let mut out = Vec::new();
        out.extend(self.tool_folder(&["journal"], false)?);
        out.extend(self.data_home(false).ok().flatten());
        Ok(out)
    }

    /// Whether another holder has the lock right now (it is not taken), in either place.
    pub(crate) fn lock_is_held_elsewhere(&self) -> bool {
        let homes = [self.tool_folder(&[], false).ok().flatten(), self.data_home(false).ok().flatten()];
        homes.into_iter().flatten().any(|home| {
            let Ok(file) = home.open_lock("lock", false) else { return false };
            match Dir::try_lock(&file) {
                Ok(true) => {
                    Dir::unlock(&file);
                    false
                }
                Ok(false) => true,
                Err(_) => false,
            }
        })
    }

    /// Where the lock and the journals go if the game folder cannot be written to (tests point it at a folder of their own).
    #[cfg(test)]
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data = dir;
    }

    /// Makes the folders `r` needs, one at a time under the handle of the
    /// last one that exists. A folder somebody else made meanwhile is used if
    /// it is a real folder.
    pub fn ensure_parent(&self, r: &mut Resolved) -> Result<(), Error> {
        self.ensure_parent_noting(r, &mut |_, _| {})
    }

    /// [`GameRoots::ensure_parent`], telling `note` (the folder it is in and its name) of each
    /// folder it made, as soon as it exists, so that the caller can remove them again.
    pub fn ensure_parent_noting(&self, r: &mut Resolved, note: &mut dyn FnMut(&Dir, &str)) -> Result<(), Error> {
        if r.missing.is_empty() {
            return Ok(());
        }
        self.verify(r)?;
        while let Some(name) = r.missing.first().cloned() {
            let made = match r.parent.mkdir(&name) {
                Ok(dir) => {
                    note(&r.parent, &name);
                    dir
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match r.parent.kind(&name)? {
                    Some(Kind::Dir) => r.parent.open_dir(&name)?,
                    Some(Kind::Link) => return Err(unsafe_path(&r.path(), format!("\"{name}\" appeared as a link while it was being made"))),
                    _ => return Err(unsafe_path(&r.path(), format!("\"{name}\" is not a folder"))),
                },
                Err(e) => return Err(e.into()),
            };
            r.parent = made;
            r.logical.push(name);
            r.missing.remove(0);
        }
        Ok(())
    }

    /// The destination's current file, for a preparatory read: a regular file
    /// never through a link, a link only if what it points to is inside the game
    /// or mods folder (a link that leads out is refused; one that leads nowhere
    /// is nothing to read). `None` if there is no file.
    pub fn read_dest(&self, r: &Resolved) -> Result<Option<File>, Error> {
        match &r.existing {
            None => Ok(None),
            Some(Existing::File(_)) => Ok(Some(r.parent.open_read(&r.name)?)),
            Some(Existing::Dir) => Err(unsafe_path(&r.path(), "that is a folder, not a file")),
            Some(Existing::Other) => Err(unsafe_path(&r.path(), "that is not a regular file")),
            Some(Existing::Link(target)) => self.read_link(r, target),
        }
    }

    fn read_link(&self, r: &Resolved, target: &OsStr) -> Result<Option<File>, Error> {
        if r.parent.kind_following(&r.name)?.is_none() {
            return Ok(None);
        }
        let t = Path::new(target);
        let abs = lexical(&if t.is_absolute() { t.to_path_buf() } else { self.root(r.anchor)?.canon.join(r.logical.iter().collect::<PathBuf>()).join(t) });
        for root in self.mods.get().into_iter().chain(std::iter::once(&self.game)) {
            let Ok(rest) = abs.strip_prefix(&root.canon).or_else(|_| abs.strip_prefix(&root.shown)) else { continue };
            let w = root.walk(&normal_parts(rest)?, true, &r.path().display().to_string())?;
            return match w.existing {
                None => Ok(None),
                Some(Existing::File(_)) => Ok(Some(w.stack.last().expect("the root").open_read(&w.name)?)),
                Some(_) => Err(unsafe_path(&r.path(), "a link that leads to something that is not a file")),
            };
        }
        Err(Error::Escape { path: r.path().display().to_string(), through: r.name.clone() })
    }

    /// The original game file for a game-relative path, read-only, through any link.
    pub fn original(&self, p: &RelPath) -> Result<Option<File>, Error> {
        self.game.open_free(p.parts())
    }

    /// Writes the destination whole: into a new file of an unpredictable name next
    /// to it, then renamed over it (a link there is replaced, never written
    /// through, and a reader sees the old or the new file). Folders it needs are
    /// made. An existing link must stay inside the game or mods folder.
    pub fn replace(&self, r: &mut Resolved, write: impl FnOnce(&mut File) -> io::Result<()>) -> Result<(), Error> {
        self.policy(r, true)?;
        match &r.existing {
            Some(Existing::Dir) => return Err(unsafe_path(&r.path(), "that is a folder, not a file")),
            Some(Existing::Other) => return Err(unsafe_path(&r.path(), "that is not a regular file")),
            Some(Existing::Link(_)) => {
                self.read_dest(r)?;
            }
            _ => {}
        }
        self.ensure_parent(r)?;
        self.verify(r)?;
        replace_file(&r.parent, &r.name, write)?;
        // The file is in place; failing to look at it again is not a failure to write it.
        r.existing = r.parent.stat(&r.name).ok().flatten();
        Ok(())
    }

    /// Deletes the destination's file (the link itself, if it is one), if it is
    /// still the one that was found: a file changed since is a conflict.
    pub fn remove(&self, r: &Resolved) -> Result<(), Error> {
        self.policy(r, true)?;
        let same = match &r.existing {
            None => return Ok(()),
            Some(Existing::Dir) => return Err(unsafe_path(&r.path(), "that is a folder, not a file")),
            Some(want) => r.parent.stat(&r.name)?.as_ref() == Some(want),
        };
        if !same {
            return Err(Error::Io(io::Error::other(format!("{} changed while it was being deleted", r.path().display()))));
        }
        self.verify(r)?;
        r.parent.unlink(&r.name)?;
        Ok(())
    }

    /// Removes the folders of `r` that are left empty, up to (not including) the root.
    pub fn prune_empty(&self, r: &Resolved) -> Result<(), Error> {
        let mut dirs = vec![self.root(r.anchor)?.dir.try_clone()?];
        for name in &r.logical {
            let next = dirs.last().expect("the root").open_dir(name)?;
            dirs.push(next);
        }
        for (i, name) in r.logical.iter().enumerate().rev() {
            if dirs[i].rmdir(name).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// The copy of a game data file in the mods folder, made from the original
    /// when missing (its path, or `None` if neither exists). A copy that is a
    /// link, or shares its data with another name (a hard link, the original),
    /// becomes a file of its own, as edits rewrite it. The copy is made under the game
    /// lock (so [`Error::Busy`] while another change runs) as a journaled one-file
    /// transaction. Not for use inside an open [`crate::txn::Txn`] of the same handle.
    pub fn mods_copy(&self, rel: &RelPath) -> Result<Option<PathBuf>, Error> {
        const LABEL: &str = "copying a game file to the mods folder";
        let _lock = crate::txn::guard(self, LABEL)?;
        let mut dest = self.mods_dest(rel)?;
        let mut src = self.original(rel)?;
        let from = match &dest.existing {
            Some(Existing::File(_)) => {
                let file = dest.parent.open_read(&dest.name)?;
                let m = file.metadata()?;
                let is_src = src.as_ref().and_then(|s| s.metadata().ok()).is_some_and(|s| (s.dev(), s.ino()) == (m.dev(), m.ino()));
                if m.nlink() == 1 && !is_src {
                    return Ok(Some(dest.path()));
                }
                Some(file)
            }
            Some(Existing::Link(_)) => self.read_dest(&dest)?,
            Some(_) => return Err(unsafe_path(&dest.path(), "that is not a regular file")),
            None => None,
        };
        let Some(mut from) = from.or(src.take()) else { return Ok(None) };
        // On btrfs/XFS this is a reflink: instant and takes no extra space.
        crate::txn::replace_resolved(self, &mut dest, LABEL, |out| {
            io::copy(&mut from, out)?;
            // Never setuid, setgid or sticky; and readable and writable by the owner, as validation and later edits must open it.
            out.set_permissions(std::fs::Permissions::from_mode((from.metadata()?.permissions().mode() & 0o777) | 0o600))
        })?;
        Ok(Some(dest.path()))
    }
}

/// Who holds a lock, from what they wrote in the file (`pid unix label`).
fn holder_of(file: &File) -> String {
    let mut buf = [0u8; 512];
    let n = std::os::unix::fs::FileExt::read_at(file, &mut buf, 0).unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]);
    let mut parts = text.trim().splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(pid), Some(_), Some(label)) if !label.is_empty() => format!("{label} (process {pid})"),
        _ => "another operation on this game".to_string(),
    }
}

impl LockGuard<'_> {
    /// Whether this is the first taking of the lock (nothing else of this program is in the middle of a change).
    pub(crate) fn is_outer(&self) -> bool {
        self.outer
    }

    /// Marks the lock as taken for a transaction: only one at a time per `GameRoots`.
    pub(crate) fn claim_txn(mut self) -> Result<Self, Error> {
        let mut slot = self.roots.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.txn {
            return Err(Error::Busy(format!("{} (a transaction is already open here)", slot.label)));
        }
        slot.txn = true;
        self.txn = true;
        drop(slot);
        Ok(self)
    }
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let mut slot = self.roots.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if self.txn {
            slot.txn = false;
        }
        slot.depth = slot.depth.saturating_sub(1);
        if slot.depth == 0 {
            slot.owner = None;
            if let Some(file) = slot.file.take() {
                Dir::unlock(&file);
            }
            slot.home = None;
        }
    }
}

/// Writes `name` of `dir` whole through a new file of an unpredictable name,
/// renamed over it. The temporary is removed on failure; it is the only name
/// this ever removes, and it made it itself. A file that is replaced keeps its permission bits.
pub fn replace_file(dir: &Dir, name: &str, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let mode = dir.mode_of(name)?;
    let (mut file, tmp) = dir.create_unique()?;
    let result = (|| {
        if let Some(mode) = mode {
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        write(&mut file)
    })()
    .and_then(|()| file.sync_all())
    .and_then(|()| dir.rename(&tmp, name, true));
    if result.is_err() {
        let _ = dir.unlink(&tmp);
    } else {
        let _ = dir.sync();
    }
    result
}
