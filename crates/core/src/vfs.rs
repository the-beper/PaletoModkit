//! Paths that go through archives: "update/update.rpf/dlc_patch/x.rpf/file".

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::fsroot::{Dir, Existing, GameRoots, Kind, Resolved};
use crate::relpath::RelPath;
use crate::txn::{self, Staged, Txn};
use crate::{Edition, Error, Game};

/// An archive file on disk plus a path inside it, which may cross nested archives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePath {
    pub disk: PathBuf,
    pub inner: Vec<String>,
}

impl ArchivePath {
    pub fn new(disk: PathBuf) -> Self {
        ArchivePath { disk, inner: Vec::new() }
    }

    /// Splits a path at the first component that is a file on disk.
    pub fn parse(path: &str) -> Option<Self> {
        let mut disk = PathBuf::new();
        if path.starts_with('/') {
            disk.push("/");
        }
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            disk.push(part);
            if disk.is_file() {
                return Some(ArchivePath { disk, inner: parts[i + 1..].iter().map(|s| s.to_string()).collect() });
            }
        }
        None
    }

    pub fn join(&self, name: &str) -> Self {
        let mut p = self.clone();
        p.inner.push(name.to_string());
        p
    }

    pub fn parent(&self) -> Option<Self> {
        let mut p = self.clone();
        p.inner.pop().map(|_| p)
    }

    pub fn inner_str(&self) -> String {
        self.inner.join("/")
    }

    /// Opens the innermost archive and returns it with the entry at this path
    /// (0, the root, when the path ends at an archive).
    pub fn open(&self, keys: Option<&Keys>) -> Result<(Archive, usize), Error> {
        let mut archive = Archive::open(&self.disk, keys)?;
        let mut cur = 0;
        for part in &self.inner {
            if archive.entries[cur].is_archive() {
                archive = archive.open_child(cur, keys)?;
                cur = 0;
            }
            cur = archive
                .children(cur)
                .find(|&i| archive.entries[i].name.eq_ignore_ascii_case(part))
                .ok_or_else(|| Error::Rpf(paleto_rpf::Error::NotFound(part.clone())))?;
        }
        if archive.entries[cur].is_archive() {
            archive = archive.open_child(cur, keys)?;
            cur = 0;
        }
        Ok((archive, cur))
    }

    /// Reads the file at this path.
    pub fn read(&self, keys: Option<&Keys>) -> Result<Vec<u8>, Error> {
        let parent = self.parent().ok_or_else(|| Error::Rpf(paleto_rpf::Error::NotAFile(self.disk.display().to_string())))?;
        let (archive, dir) = parent.open(keys)?;
        let name = self.inner.last().unwrap();
        let idx = archive
            .children(dir)
            .find(|&i| archive.entries[i].name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::Rpf(paleto_rpf::Error::NotFound(name.clone())))?;
        Ok(archive.read(idx, keys)?)
    }

    /// The same path in the mods folder when the archive is an original game
    /// file; other archives are returned unchanged. Nothing is copied: [`ArchivePath::edit`]
    /// makes the mods copy when it saves, from the mods copy if there is one, else from
    /// the original (which is only read). An error if neither exists.
    pub fn editable(&self, game: Option<&Game>) -> Result<Self, Error> {
        let Some(game) = game else { return Ok(self.clone()) };
        let Some(rel) = in_game(&self.disk, game).filter(|rel| !rel.starts_with("mods")) else { return Ok(self.clone()) };
        let (roots, rel) = (game.roots()?, RelPath::parse(&rel.to_string_lossy())?);
        let dest = roots.mods_dest(&rel)?;
        if roots.read_dest(&dest)?.is_none() && roots.original(&rel)?.is_none() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{rel} is not in the game folder"))));
        }
        Ok(ArchivePath { disk: dest.path(), inner: self.inner.clone() })
    }

    /// The copy of this archive in the mods folder, if there is one.
    pub fn in_mods(&self, game: &Game) -> Option<Self> {
        let rel = in_game(&self.disk, game)?;
        if rel.starts_with("mods") {
            return None;
        }
        let roots = game.roots().ok()?;
        let dest = roots.mods_dest(&RelPath::parse(&rel.to_string_lossy()).ok()?).ok()?;
        roots.read_dest(&dest).ok()??;
        Some(ArchivePath { disk: dest.path(), inner: self.inner.clone() })
    }

    /// Edits the folder (or nested archive) at this path: `f` gets the
    /// innermost editor and the folder inside it. The archive is saved after.
    ///
    /// An archive of a game folder is saved as the copy in the mods folder, in one transaction
    /// under the game's lock: the base is the mods copy (through its confined folders) or else the
    /// original, which is only read; the result is staged as a new file and renamed into place after
    /// it was opened again, so an error leaves the folders as they were and no path of the game folder
    /// is written through (a link from `mods` back to the originals, or any other, is refused). An
    /// archive outside every game folder is saved where it is, by a rename of a new file over it.
    pub fn edit<T>(
        &self,
        keys: Option<&Keys>,
        f: impl FnOnce(&mut ArchiveEditor, &str) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let Some(game) = owner_of(&self.disk) else {
            let mut root = ArchiveEditor::open(&self.disk, keys)?;
            let (editor, folder) = nested(&mut root, &self.inner, keys)?;
            let out = f(editor, &folder)?;
            root.save(&self.disk)?;
            return Ok(out);
        };
        let roots = game.roots()?;
        let rel = local_rel(&self.disk, &game)?.without_mods();
        let name = rel.name().to_string();
        transact(&roots, "editing an archive", keys, |txn| {
            let mut dest = roots.mods_dest(&rel)?;
            let base = match roots.read_dest(&dest)? {
                Some(file) => file,
                None => roots.original(&rel)?.ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{rel} is not in the game folder"))))?,
            };
            let len = base.metadata()?.len();
            let mut root = ArchiveEditor::open_file(base, &name, keys)?;
            let (editor, folder) = nested(&mut root, &self.inner, keys)?;
            let out = f(editor, &folder)?;
            let staged = txn.stage_sized(&mut dest, len, |file| root.save_to(file, &mut scratch_file).map(drop).map_err(io::Error::other))?;
            Ok((out, vec![(staged, name.clone())]))
        })
    }
}

/// An empty unnamed temporary file, for the archive editor's rebuilding of nested archives.
pub(crate) fn scratch_file() -> io::Result<File> {
    crate::budget::Scratch::spool(|_| Ok(0)).map(|(file, _)| file).map_err(io::Error::other)
}

/// Runs `body` as one transaction of `roots` and commits it. `body` stages and deletes through the
/// transaction and returns its result with the archives it staged, each with its file name: they are opened
/// again from disk before anything is put in place. An error is rolled back and says honestly whether that was
/// clean ("(nothing was changed)" only if it was). Several files are changed one after the other, recoverable
/// but not atomic: see [`crate::txn`].
pub(crate) fn transact<T>(roots: &GameRoots, label: &str, keys: Option<&Keys>, body: impl FnOnce(&mut Txn<'_>) -> Result<(T, Vec<(Staged, String)>), Error>) -> Result<T, Error> {
    let mut txn = Txn::begin(roots, label)?;
    let (out, archives) = match body(&mut txn) {
        Ok(done) => done,
        // The transaction already ended, with a report of what it could not undo.
        Err(e @ Error::Txn(_)) => return Err(txn::failure(e, Vec::new(), "changed")),
        Err(e) => return Err(txn::failure(txn.abort(e, Vec::new()), Vec::new(), "changed")),
    };
    let reopened = |staged: Staged, file: &File| match archives.iter().find(|(s, _)| *s == staged) {
        Some((_, name)) => ArchiveEditor::open_file(file.try_clone()?, name, keys).map(drop).map_err(Error::from),
        None => Ok(()),
    };
    txn.commit_validated(Vec::new(), reopened).map(|_| out).map_err(|e| txn::failure(e, Vec::new(), "changed"))
}

/// Creates a new, empty archive at `rel` (game-relative) in one transaction, never over a file there: a path
/// the game streams (`x64/`, `update/`, any `.rpf`) is made in the mods folder. Returns where it was made.
pub fn new_archive(game: &Game, rel: &RelPath) -> Result<PathBuf, Error> {
    let roots = game.roots()?;
    let name = rel.name().to_string();
    let mut made = PathBuf::new();
    transact(&roots, "creating an archive", None, |txn| {
        let mut dest = roots.dest(rel)?;
        if dest.existing.is_some() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", dest.path().display()))));
        }
        made = dest.path();
        let staged = txn.stage(&mut dest, |file| ArchiveEditor::new(&name).save_to(file, &mut scratch_file).map(drop).map_err(io::Error::other))?;
        Ok(((), vec![(staged, name.clone())]))
    })?;
    Ok(made)
}

/// Walks into the nested archives along `inner`; returns the innermost editor
/// and the remaining folder path.
fn nested<'a>(
    mut editor: &'a mut ArchiveEditor,
    inner: &[String],
    keys: Option<&Keys>,
) -> Result<(&'a mut ArchiveEditor, String), Error> {
    let mut rest: Vec<&str> = Vec::new();
    for part in inner {
        rest.push(part);
        let joined = rest.join("/");
        if part.to_ascii_lowercase().ends_with(".rpf") && editor.exists(&joined) {
            editor = editor.archive(&joined, false, keys)?;
            rest.clear();
        }
    }
    Ok((editor, rest.join("/")))
}

/// How an entry of a modified archive differs from the original game file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Not in the original.
    Added,
    Modified,
}

/// Compares a folder of a modified archive (a mods copy) with the same folder
/// of the original, returning the entries that differ by lowercase name.
/// Files compare by content; folders and nested archives by what they hold
/// (paths and sizes), which needs no reading.
pub fn compare_folder(modded: &ArchivePath, original: &ArchivePath, keys: Option<&Keys>) -> Result<HashMap<String, Change>, Error> {
    let (archive, dir) = modded.open(keys)?;
    let original = original.open(keys).ok();
    let mut out = HashMap::new();
    for i in archive.children(dir) {
        let name = &archive.entries[i].name;
        let found = original.as_ref().and_then(|(o, d)| Some((o, o.children(*d).find(|&j| o.entries[j].name.eq_ignore_ascii_case(name))?)));
        let change = match found {
            None => Some(Change::Added),
            Some((o, j)) => entry_differs(&archive, i, o, j, keys).then_some(Change::Modified),
        };
        if let Some(change) = change {
            out.insert(name.to_ascii_lowercase(), change);
        }
    }
    Ok(out)
}

pub(crate) fn entry_differs(a: &Archive, i: usize, b: &Archive, j: usize, keys: Option<&Keys>) -> bool {
    let (x, y) = (&a.entries[i], &b.entries[j]);
    if x.is_dir() != y.is_dir() || x.is_archive() != y.is_archive() {
        return true;
    }
    if x.is_dir() {
        return tree_differs(a, i, b, j);
    }
    if x.is_archive() {
        return match (a.open_child(i, keys), b.open_child(j, keys)) {
            (Ok(c), Ok(d)) => tree_differs(&c, 0, &d, 0),
            _ => x.size() != y.size(),
        };
    }
    if x.size() != y.size() {
        return true;
    }
    // Untouched files keep their stored bytes; a tool may store the same
    // content differently (compression, encryption), so then compare it.
    match (a.read_stored(i), b.read_stored(j)) {
        (Ok(p), Ok(q)) if p == q => false,
        _ => a.read(i, keys).ok() != b.read(j, keys).ok(),
    }
}

/// Whether two folders hold different paths, or files of different sizes.
fn tree_differs(a: &Archive, d: usize, b: &Archive, e: usize) -> bool {
    let list = |archive: &Archive, dir: usize| -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = archive.walk(dir).into_iter().map(|(path, i)| (path.to_ascii_lowercase(), archive.entries[i].size())).collect();
        entries.sort();
        entries
    };
    list(a, d) != list(b, e)
}

/// Joins a folder inside an archive and a relative path.
pub fn join_inner(folder: &str, rel: &str) -> String {
    if folder.is_empty() { rel.to_string() } else { format!("{folder}/{rel}") }
}

/// Where an entry of an archive is extracted: `rel` (names joined by `/`, as [`paleto_rpf::Archive::walk`] gives
/// them) below `dest`. Refused unless it is a plain relative path: no root or drive, no `.` or `..`, no empty
/// name (joining an absolute path would drop `dest`, and `..` would leave it), whatever the archive holds.
pub fn extract_path(dest: &Path, rel: &str) -> Result<PathBuf, Error> {
    let plain = rel.split('/').all(|name| !matches!(name, "" | "." | "..")) && Path::new(rel).components().all(|c| matches!(c, std::path::Component::Normal(_)));
    if !plain {
        return Err(Error::UnsafePath { path: rel.to_string(), why: "is not a plain relative path, so it is not extracted".into() });
    }
    Ok(dest.join(rel))
}

/// Lists local files below `path` (or `path` itself) with their path relative
/// to `path`'s parent, for adding to an archive. The temporary names of this
/// tool (`.paleto-*`: staged files, backups, folders made aside) are not files of
/// the folder and are left out.
pub fn local_files(path: &Path) -> Result<Vec<(PathBuf, String)>, Error> {
    let base = path.parent().unwrap_or(Path::new(""));
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for entry in std::fs::read_dir(&p)? {
                let entry = entry?;
                if !entry.file_name().to_string_lossy().starts_with(".paleto-") {
                    stack.push(entry.path());
                }
            }
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            out.push((p, rel));
        }
    }
    out.sort();
    Ok(out)
}

/// The game `path` (a file or a folder) belongs to: the nearest folder holding it (or itself) with a game
/// executable, as written or with the links in it resolved (a link deep into a game this program never opened),
/// else a game this program has opened whose links (`mods` on another drive, say) lead to it.
pub fn owner_of(path: &Path) -> Option<Game> {
    let above = |p: &Path| p.ancestors().find_map(|dir| Game::open(dir).ok());
    let abs = std::path::absolute(path).ok()?;
    above(&abs).or_else(|| std::fs::canonicalize(path).ok().and_then(|real| above(&real))).or_else(|| Game::known().into_iter().find(|g| in_game(path, g).is_some()))
}

/// Runs `f` under the lock of the game that `path` belongs to (see [`owner_of`]), if it belongs to one:
/// one change at a time per game, whatever the way the folder was reached. [`Error::Busy`] if the game is busy.
pub fn locked_in_game<T>(path: &Path, label: &str, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let roots = owner_of(path).map(|g| g.roots()).transpose()?;
    let _lock = roots.as_ref().map(|r| crate::txn::guard(r, label)).transpose()?;
    f()
}

// ---- files and folders of a game folder, changed by hand (the file browser) ----

const ORIGINAL: &str = "is an original game file: it is never changed here (its copy in the mods folder can be)";

/// The path of `path` (a file or folder of `game`, see [`in_game`]) relative to the game folder, as written. Only
/// the folder holding it is looked for in the game (so a folder reached by its real place, `mods` being a link to
/// another drive, is found); the last name is appended as it is and never followed: a link is itself the item.
pub fn local_rel(path: &Path, game: &Game) -> Result<RelPath, Error> {
    let not_in_game = || Error::UnsafePath { path: path.display().to_string(), why: format!("is not in the game folder {}", game.root.display()) };
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Err(not_in_game()) };
    let rel = in_game(parent, game).ok_or_else(not_in_game)?.join(name);
    RelPath::parse(&rel.to_string_lossy())
}

/// Whether `rel` names the mods folder itself (the game folder's `mods`, a folder or a link to one).
fn is_mods_itself(rel: &RelPath) -> bool {
    rel.parts().len() == 1 && rel.name().eq_ignore_ascii_case("mods")
}

/// The mods folder as it is in the game folder (named `mods`, as [`GameRoots`] looks for it) and what it is,
/// without following it. `None` if there is none.
fn mods_entry(roots: &GameRoots) -> Result<Option<(String, Kind)>, Error> {
    Ok(roots.game.dir.kind("mods")?.map(|k| ("mods".to_string(), k)))
}

/// Where `rel` is, resolved through the confined folders of the game, for a change by hand (rename, delete,
/// new folder), if the write policy allows it there: not original data, a program, the game's own `.dll` and
/// `.txt` files or this tool's folders. Judged on the resolved place, not on the name as written: a path through
/// a link (`mods/x64` leading to the originals) is refused or lands where the link really leads, and a link that
/// leaves the game and mods folders is refused. `mods/...` is the mods folder, any other path the game folder.
/// The mods folder itself is not resolved here (see [`is_mods_itself`]).
fn local_dest(roots: &GameRoots, rel: &RelPath) -> Result<Resolved, Error> {
    let dest = if rel.in_mods_folder() {
        roots.mods_dest(&rel.without_mods())?
    } else if rel.is_game_data() {
        return Err(Error::UnsafePath { path: rel.to_string(), why: ORIGINAL.to_string() });
    } else {
        roots.dest(rel)?
    };
    roots.check_write(&dest)?;
    Ok(dest)
}

/// What a refusal of [`local_dest`] says to the user.
fn refusal_text(e: &Error) -> String {
    match e {
        Error::UnsafePath { why, .. } if why == ORIGINAL => why.clone(),
        Error::UnsafePath { why, .. } => format!("cannot be changed here: {why}"),
        e => format!("cannot be changed here: {e}"),
    }
}

/// The first of `names` in the folder `dir` that this tool does not rename, delete or add to, if `dir` is in a
/// game folder (the one that holds it, whichever game is open), with why: original game data, programs, the game's
/// own `.dll` and `.txt` files, this tool's folders, or a path that leads out of the game and mods folders
/// through a link. The same check [`delete_local`], [`rename_local`] and [`mkdir_local`] make, to say so before asking.
pub fn local_refusal(dir: &Path, names: &[String]) -> Option<(String, String)> {
    let game = owner_of(dir)?;
    let roots = game.roots().ok()?;
    names.iter().find_map(|name| {
        let rel = local_rel(&dir.join(name), &game).ok()?;
        if is_mods_itself(&rel) {
            return None;
        }
        local_dest(&roots, &rel).err().map(|e| (name.clone(), refusal_text(&e)))
    })
}

/// Deletes files and folders (with all they hold) of a game folder: every file is removed in one transaction
/// (recoverable if interrupted, not atomic for a reader), a link is removed itself and never followed, then the
/// folders left empty with `rmdir`. The mods folder itself goes the same way (its files, then the folder), or, if
/// it is a link, the link alone. Refuses, before changing anything, what [`local_refusal`] names.
pub fn delete_local(game: &Game, rels: &[RelPath]) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "deleting files")?;
    let mut folders: Vec<RelPath> = Vec::new();
    let mut mods_link = None;
    transact(&roots, "deleting files", None, |txn| {
        for rel in rels {
            if is_mods_itself(rel) {
                match mods_entry(&roots)? {
                    None => return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{rel} does not exist")))),
                    Some((name, Kind::Link)) => mods_link = Some(name),
                    Some((name, Kind::Dir)) => {
                        for child in roots.game.dir.open_dir(&name)?.names()? {
                            plan_delete(&roots, txn, &RelPath::parse(&format!("mods/{child}"))?, &mut folders)?;
                        }
                        folders.push(rel.clone());
                    }
                    Some(_) => return Err(Error::UnsafePath { path: rel.to_string(), why: "is not a folder".into() }),
                }
                continue;
            }
            plan_delete(&roots, txn, rel, &mut folders)?;
        }
        Ok(((), Vec::new()))
    })?;
    // A folder is noted after the folders in it: the deepest first. One that cannot go (somebody put a file in
    // it, say) stays, as the empty or full folder it is: the files that were asked for are gone.
    for rel in &folders {
        let _ = remove_folder(&roots, rel);
    }
    let mut left: Vec<String> = Vec::new();
    if let Some(name) = mods_link {
        // The link itself, if it is still one: what it points to is never touched.
        let removed = match roots.game.dir.kind(&name) {
            Ok(Some(Kind::Link)) => roots.game.dir.unlink(&name).and_then(|()| roots.game.dir.sync()),
            Ok(_) => Err(io::Error::other("is no longer a link")),
            Err(e) => Err(e),
        };
        left.extend(removed.err().map(|e| format!("{name}: {e}")));
    }
    if left.is_empty() {
        return Ok(());
    }
    Err(Error::Io(io::Error::other(format!("the files were deleted, but the link could not be removed: {}", left.join("; ")))))
}

/// Plans the deletion of `rel` in `txn`: a file or link at once, a folder file by file; the folders are noted.
fn plan_delete(roots: &GameRoots, txn: &mut Txn<'_>, rel: &RelPath, folders: &mut Vec<RelPath>) -> Result<(), Error> {
    let dest = local_dest(roots, rel)?;
    match &dest.existing {
        None => Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{rel} does not exist")))),
        Some(Existing::Dir) => {
            for name in dest.parent.open_dir(&dest.name)?.names()? {
                plan_delete(roots, txn, &RelPath::parse(&format!("{rel}/{name}"))?, folders)?;
            }
            folders.push(rel.clone());
            Ok(())
        }
        Some(_) => txn.delete(&dest),
    }
}

/// Removes the folder `rel` if it is empty.
fn remove_folder(roots: &GameRoots, rel: &RelPath) -> Result<(), Error> {
    if is_mods_itself(rel) {
        if let Some((name, Kind::Dir)) = mods_entry(roots)? {
            roots.game.dir.rmdir(&name)?;
            let _ = roots.game.dir.sync();
        }
        return Ok(());
    }
    let dest = local_dest(roots, rel)?;
    if dest.existing == Some(Existing::Dir) {
        roots.verify(&dest)?;
        dest.parent.rmdir(&dest.name)?;
        let _ = dest.parent.sync();
    }
    Ok(())
}

/// Device and inode of what `name` of `dir` is (a file or a folder, never followed), if it is one.
fn entry_identity(dir: &Dir, name: &str) -> io::Result<Option<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    match dir.kind(name)? {
        Some(Kind::File) => dir.open_read(name)?.metadata().map(|m| Some((m.dev(), m.ino()))),
        Some(Kind::Dir) => dir.open_dir(name)?.identity().map(Some),
        _ => Ok(None),
    }
}

/// Renames `from` to `to` in `dir` without replacing anything. If `to` appeared meanwhile it is replaced only when it
/// is the very item being renamed (a change of case on a filesystem that does not tell the names apart); else
/// the rename is refused and nothing is changed.
pub(crate) fn rename_noreplace(dir: &Dir, from: &str, to: &str, case_only: bool) -> Result<(), Error> {
    let exists = || Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{to} already exists")));
    match dir.rename(from, to, false) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && case_only => {
            let (a, b) = (entry_identity(dir, from)?, entry_identity(dir, to)?);
            if a.is_some() && a == b { Ok(dir.rename(from, to, true)?) } else { Err(exists()) }
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(libc::ENOTEMPTY) => Err(exists()),
        Err(e) => Err(e.into()),
    }
}

/// Renames `from` (a file or folder of a game folder) to the name `to` in the same folder, under the game's
/// lock, through the confined folders: one `renameat2` that fails rather than replace what is at `to`
/// (atomic and lossless, so not journaled). A name that differs only in case is the same item and is renamed.
/// The mods folder is renamed like any folder, and a link is renamed itself, never what it points to.
pub fn rename_local(game: &Game, from: &RelPath, to: &str) -> Result<(), Error> {
    let new = RelPath::parse(to)?;
    if new.parts().len() != 1 {
        return Err(Error::UnsafePath { path: to.to_string(), why: "is not a single name".into() });
    }
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "renaming a file")?;
    if is_mods_itself(from) {
        return rename_mods(&roots, &new, to);
    }
    let src = local_dest(&roots, from)?;
    if src.existing.is_none() {
        return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{from} does not exist"))));
    }
    if src.name == to {
        return Ok(());
    }
    // A folder of the game folder (or a link to one) becomes the mods folder, if there is none.
    if from.parts().len() == 1 && to.eq_ignore_ascii_case("mods") {
        if !matches!(roots.game.dir.kind(&src.name)?, Some(Kind::Dir | Kind::Link)) {
            return Err(Error::UnsafePath { path: from.to_string(), why: "is not a folder".into() });
        }
        if mods_entry(&roots)?.is_some() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{to} already exists"))));
        }
        rename_noreplace(&roots.game.dir, &src.name, to, false)?;
        let _ = roots.game.dir.sync();
        return Ok(());
    }
    let sibling = RelPath::parse(&from.parts()[..from.parts().len() - 1].iter().map(String::as_str).chain([to]).collect::<Vec<_>>().join("/"))?;
    let dst = local_dest(&roots, &sibling)?;
    if dst.parent.identity()? != src.parent.identity()? {
        return Err(Error::UnsafePath { path: sibling.to_string(), why: "is not in the folder of the item renamed".into() });
    }
    roots.verify(&src)?;
    // What the ASCII case-insensitive lookup found at the new name is the item itself (a change of case
    // alone) or another item the game takes for the same name.
    let case_only = dst.existing.is_some() && dst.name == src.name;
    if dst.existing.is_some() && !case_only {
        return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{to} already exists"))));
    }
    rename_noreplace(&src.parent, &src.name, to, case_only)?;
    let _ = src.parent.sync();
    Ok(())
}

/// [`rename_local`] of the mods folder (or the link that stands for it): to a name the policy allows in the game folder.
fn rename_mods(roots: &GameRoots, new: &RelPath, to: &str) -> Result<(), Error> {
    let Some((name, kind)) = mods_entry(roots)? else { return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, "mods does not exist"))) };
    if !matches!(kind, Kind::Dir | Kind::Link) {
        return Err(Error::UnsafePath { path: name, why: "is not a folder".into() });
    }
    if name == to {
        return Ok(());
    }
    let case_only = name.eq_ignore_ascii_case(to);
    if !case_only {
        if new.is_game_data() {
            return Err(Error::UnsafePath { path: to.to_string(), why: ORIGINAL.to_string() });
        }
        let dst = roots.dest(new)?;
        roots.check_write(&dst)?;
        if dst.existing.is_some() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{to} already exists"))));
        }
    }
    rename_noreplace(&roots.game.dir, &name, to, case_only)?;
    let _ = roots.game.dir.sync();
    Ok(())
}

/// Makes the folder `rel` of a game folder (under the game's lock, through the confined folders); it must not exist.
pub fn mkdir_local(game: &Game, rel: &RelPath) -> Result<(), Error> {
    let roots = game.roots()?;
    let _lock = txn::guard(&roots, "creating a folder")?;
    if is_mods_itself(rel) {
        if mods_entry(&roots)?.is_some() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{rel} already exists"))));
        }
        roots.game.dir.mkdir("mods")?;
        let _ = roots.game.dir.sync();
        return Ok(());
    }
    let mut dest = local_dest(&roots, rel)?;
    if dest.existing.is_some() {
        return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{rel} already exists"))));
    }
    roots.ensure_parent(&mut dest)?;
    dest.parent.mkdir(&dest.name)?;
    let _ = dest.parent.sync();
    Ok(())
}

/// Removes the folder `name` of `dir` and the folders in it that are empty (`Err` if it is not empty then).
pub(crate) fn prune_empty(dir: &Dir, name: &str, depth: usize) -> io::Result<()> {
    let inner = dir.open_dir(name)?;
    if depth < 64 {
        for child in inner.names()? {
            if inner.kind(&child)? == Some(Kind::Dir) {
                let _ = prune_empty(&inner, &child, depth + 1);
            }
        }
    }
    dir.rmdir(name)
}

/// Where `path` is inside the game folder, if it is. Links count as part of
/// the game folder: a path through the game folder (whose `update` or `mods`
/// may link to another drive), or the real place one of its top-level links
/// points to, is inside it. Without this, editing such a path would change
/// the original game file in place instead of its mods copy.
pub fn in_game(path: &Path, game: &Game) -> Option<PathBuf> {
    let (abs, root) = (lexical(path)?, lexical(&game.root)?);
    if let Ok(rel) = abs.strip_prefix(&root) {
        return Some(rel.to_path_buf());
    }
    let real = std::fs::canonicalize(path).ok()?;
    if let Ok(rel) = real.strip_prefix(std::fs::canonicalize(&game.root).ok()?) {
        return Some(rel.to_path_buf());
    }
    std::fs::read_dir(&game.root).ok()?.flatten().find_map(|entry| {
        let target = std::fs::canonicalize(entry.path()).ok().filter(|_| entry.file_type().is_ok_and(|t| t.is_symlink()))?;
        real.strip_prefix(&target).ok().map(|rest| Path::new(&entry.file_name()).join(rest))
    })
}

/// Whether `path` is (or holds) original game data: an archive, `update/`,
/// `x64/` or a game executable, in the game folder outside `mods/`.
pub fn is_original_data(path: &Path, game: &Game) -> bool {
    let Some(rel) = in_game(path, game) else { return false };
    let name = rel.to_string_lossy();
    let exe = [Edition::Legacy, Edition::Enhanced].iter().any(|e| name.eq_ignore_ascii_case(e.exe_name()));
    !rel.starts_with("mods") && (crate::game::is_game_data(&name) || exe)
}

/// The absolute path, with "." and ".." resolved as written (links are kept).
fn lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in std::path::absolute(path).ok()?.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_rpf::ArchiveEditor;

    #[test]
    fn extraction_stays_below_the_destination() {
        let dest = Path::new("/out");
        assert_eq!(extract_path(dest, "a.txt").unwrap(), Path::new("/out/a.txt"));
        assert_eq!(extract_path(dest, "dir/sub/a.txt").unwrap(), Path::new("/out/dir/sub/a.txt"));
        for bad in ["", "/home/u/.bashrc", "/a", "a//b", "a/", "./a", "a/./b", "../a", "a/../b", "a/..", ".."] {
            assert!(matches!(extract_path(dest, bad), Err(Error::UnsafePath { .. })), "{bad:?}");
        }
    }

    #[test]
    fn folder_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (original, modded) = (dir.path().join("common.rpf"), dir.path().join("mods.rpf"));
        let mut editor = ArchiveEditor::new("common.rpf");
        for (path, data) in [("data/ai/handling.meta", "<a/>"), ("data/ai/same.meta", "<same/>"), ("data/ai/sized.meta", "<x/>"), ("data/other.txt", "o")] {
            editor.add_file(path, data.as_bytes().to_vec()).unwrap();
        }
        editor.save(&original).unwrap();
        std::fs::copy(&original, &modded).unwrap();

        // A mods copy: one file replaced, one of the same size changed, one added.
        let mut editor = ArchiveEditor::open(&modded, None).unwrap();
        editor.add_file("data/ai/handling.meta", b"<longer/>".to_vec()).unwrap();
        editor.add_file("data/ai/sized.meta", b"<y/>".to_vec()).unwrap();
        editor.add_file("data/ai/new.meta", b"<n/>".to_vec()).unwrap();
        editor.save(&modded).unwrap();

        let folder = |disk: &PathBuf, inner: &[&str]| ArchivePath { disk: disk.clone(), inner: inner.iter().map(|s| s.to_string()).collect() };
        let ai = compare_folder(&folder(&modded, &["data", "ai"]), &folder(&original, &["data", "ai"]), None).unwrap();
        let mut found: Vec<(&str, Change)> = ai.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        found.sort_by_key(|(k, _)| k.to_string());
        assert_eq!(found, [("handling.meta", Change::Modified), ("new.meta", Change::Added), ("sized.meta", Change::Modified)]);
        // The folder holding them differs; its untouched sibling does not.
        let data = compare_folder(&folder(&modded, &["data"]), &folder(&original, &["data"]), None).unwrap();
        assert_eq!(data.get("ai"), Some(&Change::Modified));
        assert_eq!(data.get("other.txt"), None);
        // An identical copy has no changes.
        std::fs::copy(&original, &modded).unwrap();
        assert!(compare_folder(&folder(&modded, &["data", "ai"]), &folder(&original, &["data", "ai"]), None).unwrap().is_empty());
    }

    /// Game folders made of links (a drive too small for the whole game, or a
    /// clean copy linking the original files) must still have their files
    /// edited in the mods folder, never in place.
    #[test]
    fn links_in_the_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("other drive/update");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let mut editor = ArchiveEditor::new("update.rpf");
        editor.add_file("common/data/dlclist.xml", b"<list/>".to_vec()).unwrap();
        editor.save(&elsewhere.join("update.rpf")).unwrap();
        let original = std::fs::read(elsewhere.join("update.rpf")).unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("update")).unwrap();
        let game = Game::open(&root).unwrap();

        // Through the game folder, and through the place the link points to.
        for disk in [root.join("update/update.rpf"), elsewhere.join("update.rpf")] {
            let path = ArchivePath { disk, inner: vec!["common".into(), "data".into()] };
            let target = path.editable(Some(&game)).unwrap();
            assert_eq!(target.disk, root.join("mods/update/update.rpf"));
            target.edit(None, |e, folder| e.add_file(&join_inner(folder, "new.meta"), b"<n/>".to_vec()).map_err(Error::from)).unwrap();
            assert_eq!(path.in_mods(&game).unwrap().disk, root.join("mods/update/update.rpf"));
        }
        assert_eq!(std::fs::read(elsewhere.join("update.rpf")).unwrap(), original, "original untouched");
        assert_eq!(in_game(&root.join("x/../update/update.rpf"), &game), Some(PathBuf::from("update/update.rpf")));
        assert_eq!(in_game(&dir.path().join("other drive"), &game), None);
    }

    #[test]
    fn editing_an_archive_that_has_no_original_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        let game = Game::open(&root).unwrap();
        let missing = ArchivePath::new(root.join("x64/missing.rpf"));
        let e = missing.editable(Some(&game)).unwrap_err().to_string();
        assert!(e.contains("is not in the game folder"), "{e}");
    }

    #[test]
    fn original_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/update")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        let game = Game::open(&root).unwrap();
        for (rel, original) in [
            ("update", true),
            ("Update/x64/dlcpacks/mpbeach", true),
            ("x64a.rpf", true),
            ("x64", true),
            ("gta5.exe", true),
            ("GTA5_Enhanced.exe", true),
            ("mods", false),
            ("mods/update", false),
            ("mods/x64a.rpf", false),
            ("scripts", false),
            ("ScriptHookV.dll", false),
            ("dinput8.dll", false),
        ] {
            assert_eq!(is_original_data(&root.join(rel), &game), original, "{rel}");
        }
        assert!(!is_original_data(&dir.path().join("x64a.rpf"), &game));
        // A game folder whose x64 is on another drive, reached either way.
        let elsewhere = dir.path().join("other drive/x64");
        std::fs::create_dir_all(elsewhere.join("audio")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("x64")).unwrap();
        assert!(is_original_data(&root.join("x64/audio"), &game));
        assert!(is_original_data(&elsewhere.join("audio"), &game));
    }
}
