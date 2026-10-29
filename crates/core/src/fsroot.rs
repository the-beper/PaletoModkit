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
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::ThreadId;

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
}

/// The game lock as this `GameRoots` holds it. The lock itself is an `flock` on
/// `.paleto/lock`, which belongs to the open file, so every `GameRoots` (and so
/// every operation and every thread of a program) excludes every other one;
/// inside one `GameRoots` the thread that holds it may take it again.
#[derive(Debug, Default)]
struct LockSlot {
    file: Option<File>,
    depth: usize,
    owner: Option<ThreadId>,
    label: String,
    /// A transaction is open (only one at a time).
    txn: bool,
}

/// Holds the game lock; releases it when the last guard of the thread is dropped.
#[derive(Debug)]
pub struct LockGuard<'r> {
    roots: &'r GameRoots,
    txn: bool,
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
fn find_ci(dir: &Dir, name: &str) -> io::Result<Option<(String, Kind)>> {
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
        let roots = GameRoots { game, mods: OnceLock::new(), refused: OnceLock::new(), lock: Mutex::new(LockSlot::default()) };
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

    fn resolve(&self, kind: RootKind, comps: &[String], p: &RelPath) -> Result<Resolved, Error> {
        let shown = p.to_string();
        let root = match (kind, if kind == RootKind::Mods { self.mods_state()? } else { None }) {
            (RootKind::Game, _) => &self.game,
            (RootKind::Mods, Some(mods)) => mods,
            (RootKind::Mods, None) => {
                // No mods folder yet: the first write makes it, then the folders below it.
                let mut missing = vec!["mods".to_string()];
                missing.extend(comps[..comps.len() - 1].iter().cloned());
                return Ok(Resolved { root: kind, parent: self.game.dir.try_clone()?, logical: Vec::new(), name: comps[comps.len() - 1].clone(), existing: None, missing, anchor: RootKind::Game, base: self.game.shown.clone() });
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
        let r = Resolved { root: kind, parent: w.stack.pop().expect("the root"), logical: w.logical, name: w.name, existing: w.existing, missing: w.missing, anchor: kind, base: root.shown.clone() };
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
            if is_game_data_parts(&full) {
                return Err(bad("that is the game's own data (a link leads there); changes go to the mods folder"));
            }
            if tool_owned(&full[0]) {
                return Err(bad("that folder belongs to this tool"));
            }
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
    /// journals: made if `create`, `None` if it is not there. A link or a file
    /// on the way is refused, so nothing is ever written through one.
    pub(crate) fn tool_folder(&self, names: &[&str], create: bool) -> Result<Option<Dir>, Error> {
        let mut dir = self.game.dir.try_clone()?;
        let mut shown = self.game.shown.clone();
        for name in std::iter::once(".paleto").chain(names.iter().copied()) {
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
                return Ok(LockGuard { roots: self, txn: false });
            }
            return Err(Error::Busy(format!("{} (in this program)", slot.label)));
        }
        slot.file = Some(self.acquire(label)?);
        (slot.depth, slot.owner, slot.label, slot.txn) = (1, Some(me), label.to_string(), false);
        Ok(LockGuard { roots: self, txn: false })
    }

    /// Opens and locks `.paleto/lock`; the file is never deleted.
    fn acquire(&self, label: &str) -> Result<File, Error> {
        let paleto = self.tool_folder(&[], true)?.expect("made above");
        let shown = self.game.shown.join(".paleto/lock");
        for _ in 0..2 {
            let file = paleto.open_lock("lock", true).map_err(|e| if e.raw_os_error() == Some(libc::ELOOP) { unsafe_path(&shown, "the lock file is a link") } else { e.into() })?;
            if !file.metadata()?.is_file() {
                return Err(unsafe_path(&shown, "the lock file is not a regular file"));
            }
            if !Dir::try_lock(&file)? {
                return Err(Error::Busy(holder_of(&file)));
            }
            // The file may have been replaced between the open and the lock: lock what the name means now.
            if paleto.names_file("lock", &file)? {
                // What it says is only for whoever finds the lock busy.
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let text = format!("{} {} {}\n", std::process::id(), now, label.replace(['\n', '\r'], " "));
                let _ = file.set_len(0).and_then(|()| std::os::unix::fs::FileExt::write_all_at(&file, text.as_bytes(), 0));
                return Ok(file);
            }
            Dir::unlock(&file);
        }
        Err(changed(&shown))
    }

    /// Whether another holder has the lock right now (it is not taken).
    pub(crate) fn lock_is_held_elsewhere(&self) -> bool {
        let Ok(Some(paleto)) = self.tool_folder(&[], false) else { return false };
        let Ok(file) = paleto.open_lock("lock", false) else { return false };
        match Dir::try_lock(&file) {
            Ok(true) => {
                Dir::unlock(&file);
                false
            }
            Ok(false) => true,
            Err(_) => false,
        }
    }

    /// Makes the folders `r` needs, one at a time under the handle of the
    /// last one that exists. A folder somebody else made meanwhile is used if
    /// it is a real folder.
    pub fn ensure_parent(&self, r: &mut Resolved) -> Result<(), Error> {
        if r.missing.is_empty() {
            return Ok(());
        }
        self.verify(r)?;
        while let Some(name) = r.missing.first().cloned() {
            let made = match r.parent.mkdir(&name) {
                Ok(dir) => dir,
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
    /// becomes a file of its own, as edits rewrite it.
    pub fn mods_copy(&self, rel: &RelPath) -> Result<Option<PathBuf>, Error> {
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
        self.replace(&mut dest, |out| {
            io::copy(&mut from, out)?;
            out.set_permissions(from.metadata()?.permissions())
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
        }
    }
}

/// Writes `name` of `dir` whole through a new file of an unpredictable name,
/// renamed over it. The temporary is removed on failure; it is the only name
/// this ever removes, and it made it itself.
pub fn replace_file(dir: &Dir, name: &str, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let (mut file, tmp) = dir.create_unique()?;
    let result = write(&mut file).and_then(|()| file.sync_all()).and_then(|()| dir.rename(&tmp, name, true));
    if result.is_err() {
        let _ = dir.unlink(&tmp);
    } else {
        let _ = dir.sync();
    }
    result
}
