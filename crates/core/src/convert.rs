//! Legacy → Enhanced conversion of models, textures and particles.
//!
//! The conversion itself is CodeWalker's, run by `paleto-cwconv`: a small .NET
//! program shipped next to Paleto (see tools/cwconv). This module finds it,
//! hands it the files and puts the results back into the mods folder.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::budget::{Budget, Limits, Scratch};
use crate::fsroot::{Dir, GameRoots, Kind, Owned};
use crate::loader::enhanced_version;
use crate::relpath::RelPath;
use crate::txn::{Staged, Txn};
use crate::vfs::{prune_empty, scratch_file, transact};
use crate::{Error, Game};

/// The file name of the converter.
pub const TOOL: &str = "paleto-cwconv";

/// The converter: `$PALETO_CWCONV`, or `paleto-cwconv` in `../lib/paleto-modkit/cwconv/`
/// next to this program (installed layout), in `cwconv/` next to it (a copied
/// program), or in `../cwconv/` (a cargo build: `packaging/build-cwconv.sh`
/// puts it in `target/cwconv`).
pub fn converter() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PALETO_CWCONV") {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("../lib/paleto-modkit/cwconv"), dir.join("cwconv"), dir.join("../cwconv")]
        .into_iter()
        .map(|d| d.join(TOOL))
        .find(|p| p.is_file())
}

/// Whether a file of this name is a resource the converter handles.
pub fn convertible(name: &str) -> bool {
    enhanced_version(name).is_some()
}

/// The resource version of a standalone resource file, from its header.
pub fn resource_version(data: &[u8]) -> Option<u32> {
    (data.len() >= 8 && &data[..4] == b"RSC7").then(|| u32::from_le_bytes(data[4..8].try_into().unwrap()))
}

/// Whether the Enhanced game needs this file converted.
pub fn needs_conversion(name: &str, data: &[u8]) -> bool {
    enhanced_version(name).is_some_and(|wanted| resource_version(data).is_some_and(|v| v != wanted))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The file in the Enhanced format.
    Converted(Vec<u8>),
    /// The file was already in the Enhanced format.
    Unchanged,
    Failed(String),
}

/// Converts standalone resource files; `files` are (name, data), the name's
/// extension telling the kind. One outcome per file, in order.
pub fn convert(tool: &Path, files: &[(&str, &[u8])]) -> Result<Vec<Outcome>, Error> {
    let dir = scratch_dir()?;
    let result = run(tool, &dir, files);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A new, empty folder for temporary files, only this user's; the caller
/// removes it. A folder of that name already there (made by someone else in a
/// shared `/tmp`) is never used: the next name is tried.
pub(crate) fn scratch_dir() -> Result<PathBuf, Error> {
    use std::os::unix::fs::DirBuilderExt;
    static RUN: AtomicU32 = AtomicU32::new(0);
    loop {
        let dir = std::env::temp_dir().join(format!("paleto-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|_| dir).map_err(Error::from),
        }
    }
}

/// How long the converter may take: a minute, and a minute more per file.
fn time_limit(files: usize) -> Duration {
    Duration::from_secs(60 * (1 + files as u64))
}

/// Runs a program to its end and returns its output, killing it once
/// `limit` has passed: a stuck converter must not hang an install forever.
fn output_within(cmd: &mut Command, limit: Duration) -> Result<std::process::Output, Error> {
    // Read while it runs, so a full pipe never blocks it.
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A program that was written a moment ago can be "busy" while a fork of another thread still holds its file
    // open for writing, until that fork runs its own program: a short wait ends it.
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => break result?,
        }
    };
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let start = Instant::now();
    let status = loop {
        let stop = |child: &mut std::process::Child| {
            let _ = child.kill();
            let _ = child.wait();
        };
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                stop(&mut child);
                return Err(e.into());
            }
        }
        if start.elapsed() > limit {
            stop(&mut child);
            return Err(Error::Convert(format!("{TOOL} took longer than {} s and was stopped", limit.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(std::process::Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

fn run(tool: &Path, dir: &Path, files: &[(&str, &[u8])]) -> Result<Vec<Outcome>, Error> {
    let mut args = Vec::new();
    for (i, (name, data)) in files.iter().enumerate() {
        // Only the extension matters to the converter; the index keeps names apart.
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let (input, output) = (dir.join(format!("{i}.{ext}")), dir.join(format!("{i}.out.{ext}")));
        std::fs::write(&input, data)?;
        args.push(input);
        args.push(output);
    }
    let out = output_within(Command::new(tool).args(&args), time_limit(files.len())).map_err(|e| match e {
        Error::Io(e) => Error::Convert(format!("cannot run {}: {e}", tool.display())),
        e => e,
    })?;
    if !out.status.success() {
        return Err(Error::Convert(format!("{TOOL} failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let report = String::from_utf8_lossy(&out.stdout);
    let mut lines = report.lines();
    let mut outcomes = Vec::new();
    for pair in args.chunks(2) {
        let input = pair[0].to_string_lossy();
        let line = lines.next().unwrap_or_default();
        let outcome = if line == format!("converted {input}") {
            Outcome::Converted(std::fs::read(&pair[1])?)
        } else if line == format!("unchanged {input}") {
            Outcome::Unchanged
        } else if let Some(msg) = line.strip_prefix(&format!("error {input}: ")) {
            Outcome::Failed(msg.to_string())
        } else {
            Outcome::Failed(format!("unexpected answer from {TOOL}: {line:?}"))
        };
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Where the Legacy files replaced by a conversion are kept: `mods_legacy/`
/// in the game folder, next to `mods/`, one folder per run named after the
/// time (seconds since 1970), with the same relative paths as the mods folder.
pub fn backup_dir(game: &Game) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    game.root.join("mods_legacy").join(stamp.to_string())
}

/// A file inside the mods folder's archives: the archives nested in the
/// top-level one, the file inside the innermost, and its full path.
type Placed = (Vec<String>, String, String);

/// The conversions that can be undone: the folders of `mods_legacy/` (named
/// after their time), newest first, with how many files each kept.
pub fn backups(game: &Game) -> Vec<(String, usize)> {
    let mut out: Vec<(u64, String, usize)> = std::fs::read_dir(game.root.join("mods_legacy"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let time = name.parse::<u64>().ok()?;
            let count = crate::vfs::local_files(&e.path()).ok()?.len();
            (count > 0).then_some((time, name, count))
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    out.into_iter().map(|(_, name, count)| (name, count)).collect()
}

/// Undoes a conversion: puts the Legacy files kept in `mods_legacy/<time>/`
/// back into the mods folder's archives, then removes what is left of that folder
/// (the files that went back, and the folders left empty). All or nothing: one
/// transaction under the game's lock puts every archive back and deletes the backups, so an
/// error (or a crash, which the next run undoes) leaves the mods folder and the backups as they were.
/// Report paths are relative to the mods folder; only a backup that is not inside an archive stays, with its reason.
pub fn undo(game: &Game, keys: Option<&Keys>, time: &str) -> Result<Vec<Converted>, Error> {
    let roots = game.roots()?;
    let missing = || Error::Convert(format!("no conversion {time} to undo in {}", game.root.join("mods_legacy").display()));
    if time.parse::<u64>().is_err() {
        return Err(missing());
    }
    let _lock = crate::txn::guard(&roots, "undoing a conversion")?;
    let Some(kept) = roots.owned_folder("mods_legacy", &[time], false)? else { return Err(missing()) };
    let (mut files, mut odd) = (Vec::new(), Vec::new());
    kept_files(&kept, "", 0, &mut files, &mut odd)?;
    files.sort();
    let mut by_archive: BTreeMap<String, Vec<(Placed, String)>> = BTreeMap::new();
    let mut report: Vec<Converted> = odd.into_iter().map(|path| Converted { path, error: Some("not a plain file".into()) }).collect();
    for local in files {
        match split_archives(&local) {
            Some((top, nested, file)) => by_archive.entry(top).or_default().push(((nested, file, local.clone()), local)),
            None => report.push(Converted { path: local, error: Some("not inside an archive".into()) }),
        }
    }
    transact(&roots, "undoing a conversion", keys, |txn| {
        let budget = Budget::new(Limits::PACKAGE);
        let cap = budget.limits().in_memory;
        let mut staged = Vec::new();
        for (top, files) in &by_archive {
            let rel = RelPath::parse(top)?;
            let mut dest = roots.mods_dest(&rel)?;
            let base = roots.read_dest(&dest)?.ok_or_else(|| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{top} is not in the mods folder"))))?;
            let len = base.metadata()?.len();
            let mut editor = ArchiveEditor::open_file(base, rel.name(), keys)?;
            for ((nested, file, _), local) in files {
                let mut backup = open_in(&kept, local)?;
                let size = backup.metadata()?.len();
                inner(&mut editor, nested, keys)?.add_file(file, budget.read(&mut backup, size, cap, local)?)?;
            }
            staged.push((txn.stage_sized(&mut dest, len, move |out| editor.save_to(out, &mut scratch_file).map(drop).map_err(std::io::Error::other))?, rel.name().to_string()));
            for (_, local) in files {
                txn.delete(&roots.owned_dest(Owned::Legacy, &RelPath::parse(&format!("{time}/{local}"))?)?)?;
            }
        }
        Ok(((), staged))
    })?;
    report.extend(by_archive.into_values().flatten().map(|((_, _, path), _)| Converted { path, error: None }));
    // What is left is only folders, and what could not go back: the folders left empty are removed, no more.
    if let Ok(Some(legacy)) = roots.owned_folder("mods_legacy", &[], false) {
        let _ = prune_empty(&legacy, time, 0);
    }
    report.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// The files below `dir` (paths with `/`) and the entries that are neither files nor folders (links, pipes).
fn kept_files(dir: &Dir, prefix: &str, depth: usize, files: &mut Vec<String>, odd: &mut Vec<String>) -> Result<(), Error> {
    for name in dir.names()? {
        let path = format!("{prefix}{name}");
        match dir.kind(&name)? {
            Some(Kind::File) => files.push(path),
            Some(Kind::Dir) if depth < 64 => kept_files(&dir.open_dir(&name)?, &format!("{path}/"), depth + 1, files, odd)?,
            _ => odd.push(path),
        }
    }
    Ok(())
}

/// A file below `dir` by its path with `/`, opened for reading without following a link on the way.
fn open_in(dir: &Dir, path: &str) -> Result<File, Error> {
    let (name, folders) = path.rsplit_once('/').map_or((path, ""), |(folders, name)| (name, folders));
    let mut at = dir.try_clone()?;
    for folder in folders.split('/').filter(|f| !f.is_empty()) {
        at = at.open_dir(folder)?;
    }
    Ok(at.open_read(name)?)
}

/// What happened to one file of the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    /// Relative to the mods folder, as in [`crate::loader::ForeignFile`].
    pub path: String,
    /// `None` when converted, otherwise why not.
    pub error: Option<String>,
}

/// The names below `mods_legacy/` that `backup` (a folder of the game folder) is, for [`GameRoots::owned_dest`].
fn legacy_parts(game: &Game, backup: &Path) -> Result<Vec<String>, Error> {
    let rel = crate::vfs::in_game(backup, game).filter(|rel| rel.starts_with("mods_legacy"));
    let rel = rel.ok_or_else(|| Error::Convert(format!("the Legacy files are kept in {}, not in {}", game.root.join("mods_legacy").display(), backup.display())))?;
    Ok(rel.components().skip(1).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
}

/// Converts files inside the mods folder's archives; `paths` are relative to
/// the mods folder with '/' separators, as [`crate::loader::foreign_files`]
/// gives them. The Legacy originals are kept under `backup` (same relative
/// paths; a folder of `mods_legacy/`) when given. One transaction under the game's
/// lock: each archive is rewritten as a new file once, after all its files, and the backups are
/// new files too (one that exists is kept), all put in place together or all undone (recoverable
/// after a crash, not atomic for a reader). An error converts nothing.
pub fn convert_in_mods(game: &Game, keys: Option<&Keys>, tool: &Path, paths: &[String], backup: Option<&Path>) -> Result<Vec<Converted>, Error> {
    let roots = game.roots()?;
    let backup = backup.map(|b| legacy_parts(game, b)).transpose()?;
    // Top-level archive -> [(nested archives, file inside the innermost), full path].
    let mut by_archive: BTreeMap<String, Vec<Placed>> = BTreeMap::new();
    let mut report = Vec::new();
    for path in paths {
        match split_archives(path) {
            Some((top, nested, file)) => by_archive.entry(top).or_default().push((nested, file, path.clone())),
            None => report.push(Converted { path: path.clone(), error: Some("not inside an archive".into()) }),
        }
    }
    report.extend(transact(&roots, "converting files in the mods folder", keys, |txn| {
        let (mut report, mut staged) = (Vec::new(), Vec::new());
        for (top, files) in &by_archive {
            report.extend(convert_archive(&roots, txn, keys, tool, top, files, backup.as_deref(), &mut staged)?);
        }
        Ok((report, staged))
    })?);
    report.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// Converts files of one archive of the mods folder (`top`, relative to it) in `txn`: its backups and the
/// archive are staged; `files` are (nested archives, file inside the innermost, path for the report and the
/// backup), `backup` the names below `mods_legacy/` of the folder the Legacy files go to.
#[allow(clippy::too_many_arguments)]
fn convert_archive(
    roots: &GameRoots,
    txn: &mut Txn<'_>,
    keys: Option<&Keys>,
    tool: &Path,
    top: &str,
    files: &[Placed],
    backup: Option<&[String]>,
    staged: &mut Vec<(Staged, String)>,
) -> Result<Vec<Converted>, Error> {
    let rel = RelPath::parse(top)?;
    let mut dest = roots.mods_dest(&rel)?;
    let base = roots.read_dest(&dest)?.ok_or_else(|| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{top} is not in the mods folder"))))?;
    let len = base.metadata()?.len();
    let mut editor = ArchiveEditor::open_file(base, rel.name(), keys)?;
    let (report, originals, changed) = convert_files(&mut editor, keys, tool, files)?;
    if changed {
        if let Some(prefix) = backup {
            for (path, original) in originals {
                let names: Vec<&str> = prefix.iter().map(String::as_str).chain([path.as_str()]).collect();
                let mut kept = roots.owned_dest(Owned::Legacy, &RelPath::parse(&names.join("/"))?)?;
                // The Legacy original that is kept there is never overwritten.
                if kept.existing.is_none() {
                    txn.stage_sized(&mut kept, original.len() as u64, |out| out.write_all(&original))?;
                }
            }
        }
        let name = rel.name().to_string();
        staged.push((txn.stage_sized(&mut dest, len, move |out| editor.save_to(out, &mut scratch_file).map(drop).map_err(std::io::Error::other))?, name));
    }
    Ok(report)
}

/// Converts the `files` of `editor` (see [`convert_archive`]) in memory: the report, the Legacy original of
/// each file converted with its path, and whether anything was.
#[allow(clippy::type_complexity)]
fn convert_files(editor: &mut ArchiveEditor, keys: Option<&Keys>, tool: &Path, files: &[Placed]) -> Result<(Vec<Converted>, Vec<(String, Vec<u8>)>, bool), Error> {
    let mut data = Vec::new();
    for (nested, file, _) in files {
        data.push(inner(editor, nested, keys)?.read(file, keys)?);
    }
    let inputs: Vec<(&str, &[u8])> = files.iter().zip(&data).map(|((_, f, _), d)| (f.as_str(), d.as_slice())).collect();
    let outcomes = convert(tool, &inputs)?;
    let mut originals = Vec::new();
    let mut report = Vec::new();
    for (((nested, file, path), original), outcome) in files.iter().zip(data).zip(outcomes) {
        let error = match outcome {
            Outcome::Converted(new) => {
                inner(editor, nested, keys)?.add_file(file, new)?;
                originals.push((path.clone(), original));
                None
            }
            Outcome::Unchanged => Some("already in the Enhanced format".into()),
            Outcome::Failed(e) => Some(e),
        };
        report.push(Converted { path: path.clone(), error });
    }
    let changed = !originals.is_empty();
    Ok((report, originals, changed))
}

/// The files of an archive (nested archives too) that the Enhanced game needs
/// converted, as paths inside it with '/' separators.
pub fn legacy_in_archive(archive: &Archive, keys: Option<&Keys>) -> Vec<String> {
    let mut found = Vec::new();
    let mut queue = vec![(String::new(), None::<Archive>)];
    while let Some((prefix, child)) = queue.pop() {
        let archive = child.as_ref().unwrap_or(archive);
        for (path, i) in archive.walk(0) {
            // The parser refuses odd names; the paths found here go on to edit the archive, so check again.
            if !crate::vfs::is_plain_entry_path(&path) {
                continue;
            }
            let entry = &archive.entries[i];
            if entry.is_archive() {
                if let Ok(c) = archive.open_child(i, keys) {
                    queue.push((format!("{prefix}{path}/"), Some(c)));
                }
            } else if let Some(wanted) = enhanced_version(&entry.name)
                && entry.resource_version().is_some_and(|v| v != wanted)
            {
                found.push(format!("{prefix}{path}"));
            }
        }
    }
    found.sort();
    found
}

/// Converts every file of an archive on disk that the Enhanced game needs
/// converted, saving the archive in place. Report paths are inside the archive.
/// Only for a copy of this program's own in its scratch folder (see [`prepare`]), never a file of a game
/// or mods folder: the save here is a plain rename of a new file over the archive, not a transaction.
pub fn convert_whole_archive(disk: &Path, keys: Option<&Keys>, tool: &Path) -> Result<Vec<Converted>, Error> {
    let paths = legacy_in_archive(&Archive::open(disk, keys)?, keys);
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let name = disk.file_name().unwrap_or_default().to_string_lossy();
    let files: Vec<_> = paths
        .iter()
        .filter_map(|p| split_archives(&format!("{name}/{p}")).map(|(_, nested, file)| (nested, file, p.clone())))
        .collect();
    let mut editor = ArchiveEditor::open(disk, keys)?;
    let (report, _, changed) = convert_files(&mut editor, keys, tool, &files)?;
    if changed {
        editor.save(disk)?;
    }
    Ok(report)
}

/// Files on disk about to be added to an archive (as [`crate::vfs::local_files`]
/// lists them: path, path relative to what was dropped) that the Enhanced
/// game needs converted: loose resources ("cars/car.yft") and resources inside
/// archives ("dlc.rpf/x64/vehicles.rpf/car.yft"), by relative path.
pub fn legacy_in_local(files: &[(PathBuf, String)], keys: Option<&Keys>) -> Vec<String> {
    let mut found = Vec::new();
    for (path, rel) in files {
        let rel = rel.replace('\\', "/");
        if convertible(&rel) {
            if header(path).is_some_and(|h| needs_conversion(&rel, &h)) {
                found.push(rel);
            }
        } else if rel.to_ascii_lowercase().ends_with(".rpf")
            && let Ok(archive) = Archive::open(path, keys)
        {
            found.extend(legacy_in_archive(&archive, keys).into_iter().map(|p| format!("{rel}/{p}")));
        }
    }
    found
}

/// The first bytes of a file, enough for [`needs_conversion`].
fn header(path: &Path) -> Option<[u8; 16]> {
    use std::io::Read;
    let mut buf = [0; 16];
    std::fs::File::open(path).ok()?.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// A file ready to be added: held in memory (converted, or read), or still on disk as it is.
pub enum Prepared {
    Bytes(Vec<u8>),
    /// An archive converted on disk: its folder is already gone, and it is read through the handle (`len` bytes).
    File(File, u64),
    /// Not changed: to be copied from where it is, not held.
    Disk(PathBuf),
}

/// Reads files on disk (as [`legacy_in_local`] takes them), converting the
/// Legacy ones with `tool`: loose resources, and the resources inside archives
/// (the archive on disk is left alone). One data per file, in order, and a
/// log of what was converted or could not be. Files that need no conversion
/// are not read: they stay [`Prepared::Disk`].
pub fn prepare(files: &[(PathBuf, String)], tool: &Path, keys: Option<&Keys>) -> Result<(Vec<Prepared>, Vec<String>), Error> {
    let budget = Budget::new(Limits::PACKAGE);
    let in_memory = budget.limits().in_memory;
    let mut data = Vec::new();
    let mut log = Vec::new();
    let mut loose = Vec::new();
    for (i, (path, rel)) in files.iter().enumerate() {
        let rel = rel.replace('\\', "/");
        if convertible(&rel) && header(path).is_some_and(|h| needs_conversion(&rel, &h)) {
            // Held for the converter; kept on disk as it is unless it is converted.
            loose.push((i, rel, read_whole(&budget, path, in_memory)?));
            data.push(Prepared::Disk(path.clone()));
        } else if rel.to_ascii_lowercase().ends_with(".rpf") && Archive::open(path, keys).is_ok_and(|a| !legacy_in_archive(&a, keys).is_empty()) {
            let scratch = Scratch::new()?;
            let copy = scratch.path().join("archive.rpf");
            let mut file = File::open(path)?;
            let len = file.metadata()?.len();
            budget.copy(&mut file, len, &mut scratch.create("archive.rpf")?, &rel)?;
            let report = convert_whole_archive(&copy, keys, tool)?;
            for r in &report {
                log.push(match &r.error {
                    None => format!("converted {rel}/{}", r.path),
                    Some(e) => format!("{rel}/{}: not converted: {e}", r.path),
                });
            }
            let converted = File::open(&copy)?;
            let len = converted.metadata()?.len();
            data.push(Prepared::File(converted, len));
        } else {
            data.push(Prepared::Disk(path.clone()));
        }
    }
    if !loose.is_empty() {
        let inputs: Vec<(&str, &[u8])> = loose.iter().map(|(_, rel, bytes)| (rel.as_str(), bytes.as_slice())).collect();
        let outcomes = convert(tool, &inputs)?;
        for ((i, rel, _), outcome) in loose.iter().zip(outcomes) {
            match outcome {
                Outcome::Converted(new) => {
                    data[*i] = Prepared::Bytes(new);
                    log.push(format!("converted {rel}"));
                }
                Outcome::Unchanged => {}
                Outcome::Failed(e) => log.push(format!("{rel}: not converted: {e}")),
            }
        }
    }
    Ok((data, log))
}

/// A file on disk into memory, within the budget.
fn read_whole(budget: &Budget, path: &Path, cap: u64) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    budget.read(&mut file, len, cap, &path.display().to_string())
}

/// [`prepare`], with every file in memory: what an archive editor takes. A file over
/// [`Limits::in_memory`] (1 GiB) is refused, and all of them together over `in_memory_total` (4 GiB).
pub fn read_converting(files: &[(PathBuf, String)], tool: &Path, keys: Option<&Keys>) -> Result<(Vec<Vec<u8>>, Vec<String>), Error> {
    let (prepared, log) = prepare(files, tool, keys)?;
    let budget = Budget::new(Limits::PACKAGE);
    let cap = budget.limits().in_memory;
    let data = prepared
        .into_iter()
        .zip(files)
        .map(|(p, (_, rel))| match p {
            Prepared::Bytes(data) => Ok(data),
            Prepared::File(mut file, len) => budget.read(&mut file, len, cap, rel),
            Prepared::Disk(path) => read_whole(&budget, &path, cap),
        })
        .collect::<Result<_, Error>>()?;
    Ok((data, log))
}

/// The archive editor `nested` names inside `editor`.
pub(crate) fn inner<'a>(editor: &'a mut ArchiveEditor, nested: &[String], keys: Option<&Keys>) -> Result<&'a mut ArchiveEditor, Error> {
    let mut ed = editor;
    for name in nested {
        ed = ed.archive(name, false, keys)?;
    }
    Ok(ed)
}

/// "a/b.rpf/c/d.rpf/e.ydr" → ("a/b.rpf", ["c/d.rpf"], "e.ydr").
pub(crate) fn split_archives(path: &str) -> Option<(String, Vec<String>, String)> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    for seg in path.split('/') {
        current.push(seg);
        if seg.to_ascii_lowercase().ends_with(".rpf") {
            parts.push(current.join("/"));
            current.clear();
        }
    }
    if parts.is_empty() || current.is_empty() {
        return None;
    }
    let top = parts.remove(0);
    Some((top, parts, current.join("/")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn splits_archive_paths() {
        assert_eq!(
            split_archives("update/x64/dlcpacks/a/dlc.rpf/x64/vehicles.rpf/car.yft"),
            Some(("update/x64/dlcpacks/a/dlc.rpf".into(), vec!["x64/vehicles.rpf".into()], "car.yft".into()))
        );
        assert_eq!(split_archives("x64e.rpf/props/a.ydr"), Some(("x64e.rpf".into(), vec![], "props/a.ydr".into())));
        assert_eq!(split_archives("loose.ydr"), None);
        assert_eq!(split_archives("x64e.rpf"), None);
    }

    #[test]
    fn checks_versions() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &[0; 8]].concat();
        assert!(needs_conversion("a.YFT", &res(162)));
        assert!(!needs_conversion("a.yft", &res(171)));
        assert!(needs_conversion("a.ytd", &res(13)));
        assert!(!needs_conversion("a.txt", &res(13)));
        assert!(!needs_conversion("a.ydr", b"not a resource"));
    }

    /// A stand-in for paleto-cwconv: version 162 becomes 171, 171 is left
    /// alone, anything else fails.
    pub(crate) fn fake_tool(dir: &Path) -> PathBuf {
        let path = dir.join(TOOL);
        crate::write_script(
            &path,
            r#"#!/bin/sh
while [ $# -gt 0 ]; do
    v=$(od -An -tu4 -j4 -N4 "$1" | tr -d ' ')
    case $v in
        162) { printf 'RSC7\253\000\000\000\000\000\000\240\000\000\000\260'; tail -c +17 "$1"; } > "$2"; echo "converted $1" ;;
        171) echo "unchanged $1" ;;
        *) echo "error $1: version $v" ;;
    esac
    shift 2
done
"#,
        );
        path
    }

    #[test]
    fn converts_inside_mods_archives() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let mut pack = ArchiveEditor::new("dlc.rpf");
        let vehicles = pack.archive("x64/vehicles.rpf", true, None).unwrap();
        vehicles.add_file("car.yft", res(162)).unwrap();
        vehicles.add_file("car_hi.yft", res(171)).unwrap();
        pack.add_file("x64/odd.ydr", res(7)).unwrap();
        let disk = root.join("mods/update/x64/dlcpacks/car/dlc.rpf");
        pack.save(&disk).unwrap();

        let game = Game::open(&root).unwrap();
        let tool = fake_tool(dir.path());
        let backup = root.join("mods_legacy/1700000000");
        let base = "update/x64/dlcpacks/car/dlc.rpf";
        let paths = [format!("{base}/x64/vehicles.rpf/car.yft"), format!("{base}/x64/vehicles.rpf/car_hi.yft"), format!("{base}/x64/odd.ydr")];
        let report = convert_in_mods(&game, None, &tool, &paths, Some(&backup)).unwrap();
        let errors: Vec<_> = report.iter().map(|r| (r.path.rsplit('/').next().unwrap(), r.error.as_deref())).collect();
        assert_eq!(errors, [("odd.ydr", Some("version 7")), ("car.yft", None), ("car_hi.yft", Some("already in the Enhanced format"))]);

        let mut saved = ArchiveEditor::open(&disk, None).unwrap();
        let vehicles = saved.archive("x64/vehicles.rpf", false, None).unwrap();
        assert_eq!(vehicles.read("car.yft", None).unwrap(), res(171));
        assert_eq!(vehicles.read("car_hi.yft", None).unwrap(), res(171));
        assert_eq!(std::fs::read(backup.join(&paths[0])).unwrap(), res(162));
        assert!(!backup.join(&paths[1]).exists());

        // Undoing a conversion kept in the game folder puts the Legacy file back.
        let kept = backup;
        assert_eq!(backups(&game), [("1700000000".to_string(), 1)]);
        let report = undo(&game, None, "1700000000").unwrap();
        assert_eq!(report, [Converted { path: paths[0].clone(), error: None }]);
        let mut saved = ArchiveEditor::open(&disk, None).unwrap();
        assert_eq!(saved.archive("x64/vehicles.rpf", false, None).unwrap().read("car.yft", None).unwrap(), res(162));
        assert!(!kept.exists());
        assert!(backups(&game).is_empty());
        assert!(undo(&game, None, "1700000000").is_err());
    }

    #[test]
    fn legacy_files_with_names_that_are_not_plain_paths_are_left_out() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let mut ed = ArchiveEditor::new("a.rpf");
        for name in ["car.yft", "evil.yft", "x64/odd.yft"] {
            ed.add_file(name, res(162)).unwrap();
        }
        let path = dir.path().join("a.rpf");
        ed.save(&path).unwrap();
        let mut archive = Archive::open(&path, None).unwrap();
        assert_eq!(legacy_in_archive(&archive, None), ["car.yft", "evil.yft", "x64/odd.yft"]);
        // The parser cannot produce these names: make the entry list by hand.
        for (name, odd) in [("evil.yft", "../evil.yft"), ("odd.yft", "../odd.yft")] {
            archive.entries.iter_mut().find(|e| e.name == name).unwrap().name = odd.into();
        }
        assert_eq!(legacy_in_archive(&archive, None), ["car.yft"]);
    }

    #[test]
    fn converts_files_on_disk() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let drop = dir.path().join("car");
        std::fs::create_dir_all(drop.join("stream")).unwrap();
        std::fs::write(drop.join("stream/car.yft"), res(162)).unwrap();
        std::fs::write(drop.join("stream/car_hi.yft"), res(171)).unwrap();
        std::fs::write(drop.join("readme.txt"), b"hi").unwrap();
        let mut pack = ArchiveEditor::new("dlc.rpf");
        pack.archive("x64/vehicles.rpf", true, None).unwrap().add_file("car.yft", res(162)).unwrap();
        pack.save(&drop.join("dlc.rpf")).unwrap();

        let files = crate::vfs::local_files(&drop).unwrap();
        let legacy = legacy_in_local(&files, None);
        assert_eq!(legacy, ["car/dlc.rpf/x64/vehicles.rpf/car.yft", "car/stream/car.yft"]);

        let tool = fake_tool(dir.path());
        let (data, log) = read_converting(&files, &tool, None).unwrap();
        assert_eq!(log, ["converted car/dlc.rpf/x64/vehicles.rpf/car.yft", "converted car/stream/car.yft"]);
        let by_name: std::collections::HashMap<&str, &Vec<u8>> = files.iter().map(|(_, r)| r.as_str()).zip(&data).collect();
        assert_eq!(by_name["car/stream/car.yft"], &res(171));
        assert_eq!(by_name["car/stream/car_hi.yft"], &res(171));
        assert_eq!(by_name["car/readme.txt"], &b"hi".to_vec());
        // The pack is converted in memory; the one on disk is untouched.
        let converted = dir.path().join("converted.rpf");
        std::fs::write(&converted, by_name["car/dlc.rpf"]).unwrap();
        let mut pack = ArchiveEditor::open(&converted, None).unwrap();
        assert_eq!(pack.archive("x64/vehicles.rpf", false, None).unwrap().read("car.yft", None).unwrap(), res(171));
        assert_eq!(legacy_in_archive(&Archive::open(&drop.join("dlc.rpf"), None).unwrap(), None), ["x64/vehicles.rpf/car.yft"]);
    }

    #[test]
    fn files_that_need_no_conversion_stay_on_disk() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<(PathBuf, String)> = [("car.yft", res(162)), ("car_hi.yft", res(171)), ("readme.txt", b"hi".to_vec())].into_iter().map(|(name, data)| {
            std::fs::write(dir.path().join(name), data).unwrap();
            (dir.path().join(name), name.to_string())
        }).collect();
        let (data, log) = prepare(&files, &fake_tool(dir.path()), None).unwrap();
        assert_eq!(log, ["converted car.yft"]);
        assert!(matches!(&data[0], Prepared::Bytes(b) if *b == res(171)));
        assert!(matches!(&data[1], Prepared::Disk(p) if *p == files[1].0));
        assert!(matches!(&data[2], Prepared::Disk(p) if *p == files[2].0));
    }

    #[test]
    fn a_converted_archive_stays_a_file() {
        use std::io::Read;
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let mut pack = ArchiveEditor::new("dlc.rpf");
        pack.archive("x64/vehicles.rpf", true, None).unwrap().add_file("car.yft", res(162)).unwrap();
        pack.save(&dir.path().join("dlc.rpf")).unwrap();
        let (data, log) = prepare(&[(dir.path().join("dlc.rpf"), "dlc.rpf".into())], &fake_tool(dir.path()), None).unwrap();
        assert_eq!(log, ["converted dlc.rpf/x64/vehicles.rpf/car.yft"]);
        // Not read into memory: a file, with its folder gone, that is read through the handle it was opened with.
        let Some(Prepared::File(mut file, len)) = data.into_iter().next() else { panic!("the converted archive is not a file") };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len);
        let converted = dir.path().join("converted.rpf");
        std::fs::write(&converted, bytes).unwrap();
        let mut pack = ArchiveEditor::open(&converted, None).unwrap();
        assert_eq!(pack.archive("x64/vehicles.rpf", false, None).unwrap().read("car.yft", None).unwrap(), res(171));
    }

    #[test]
    fn a_stuck_converter_is_stopped() {
        let start = Instant::now();
        let err = output_within(Command::new("sleep").arg("30"), Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("was stopped"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(10));
        let out = output_within(Command::new("sh").args(["-c", "echo out; echo err >&2"]), Duration::from_secs(30)).unwrap();
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice()), (&b"out\n"[..], &b"err\n"[..]));
    }

    #[test]
    fn a_program_written_a_moment_ago_runs_while_other_threads_fork() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            // Each fork holds the files open for writing at that moment until it runs `true`.
            for _ in 0..4 {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = Command::new("true").status();
                    }
                });
            }
            for i in 0..300 {
                let path = dir.path().join(format!("tool{i}"));
                std::fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                let out = output_within(&mut Command::new(&path), Duration::from_secs(30));
                if !out.as_ref().is_ok_and(|o| o.stdout == b"ok\n") {
                    stop.store(true, Ordering::Relaxed);
                    panic!("run {i}: {out:?}");
                }
            }
            stop.store(true, Ordering::Relaxed);
        });
    }

    #[test]
    fn scratch_folders_are_new_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let first = scratch_dir().unwrap();
        // The next name already taken, as someone else could in a shared /tmp. Another test may have got it from
        // `scratch_dir` first and still be using it: only a folder made here is removed here.
        let n: u32 = first.to_string_lossy().rsplit('-').next().unwrap().parse().unwrap();
        let taken = std::env::temp_dir().join(format!("paleto-{}-{}", std::process::id(), n + 1));
        let made = std::fs::create_dir(&taken).is_ok();
        let next = scratch_dir().unwrap();
        assert_ne!(next, taken);
        assert_eq!(std::fs::metadata(&next).unwrap().permissions().mode() & 0o777, 0o700);
        for dir in [first, next].into_iter().chain(made.then_some(taken)) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
