//! Legacy → Enhanced conversion of models, textures and particles.
//!
//! The conversion itself is CodeWalker's, run by `paleto-cwconv`: a small .NET
//! program shipped next to Paleto (see tools/cwconv). This module finds it,
//! hands it the files and puts the results back into the mods folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::loader::enhanced_version;
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
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
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
/// back into the mods folder's archives, then removes that folder (when every
/// file went back). Report paths are relative to the mods folder.
pub fn undo(game: &Game, keys: Option<&Keys>, time: &str) -> Result<Vec<Converted>, Error> {
    let dir = game.root.join("mods_legacy").join(time);
    if time.parse::<u64>().is_err() || !dir.is_dir() {
        return Err(Error::Convert(format!("no conversion {time} to undo in {}", game.root.join("mods_legacy").display())));
    }
    let mut by_archive: BTreeMap<String, Vec<(Placed, PathBuf)>> = BTreeMap::new();
    let mut report = Vec::new();
    for (local, _) in crate::vfs::local_files(&dir)? {
        let path = local.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
        match split_archives(&path) {
            Some((top, nested, file)) => by_archive.entry(top).or_default().push(((nested, file, path), local)),
            None => report.push(Converted { path, error: Some("not inside an archive".into()) }),
        }
    }
    for (top, files) in by_archive {
        let disk = game.mods_dir().join(&top);
        let result = (|| -> Result<(), Error> {
            let mut editor = ArchiveEditor::open(&disk, keys)?;
            for ((nested, file, _), local) in &files {
                inner(&mut editor, nested, keys)?.add_file(file, std::fs::read(local)?)?;
            }
            Ok(editor.save(&disk)?)
        })();
        let error = result.err().map(|e| e.to_string());
        report.extend(files.into_iter().map(|((_, _, path), _)| Converted { path, error: error.clone() }));
    }
    if report.iter().all(|r| r.error.is_none()) {
        std::fs::remove_dir_all(&dir)?;
    }
    report.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// What happened to one file of the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    /// Relative to the mods folder, as in [`crate::loader::ForeignFile`].
    pub path: String,
    /// `None` when converted, otherwise why not.
    pub error: Option<String>,
}

/// Converts files inside the mods folder's archives; `paths` are relative to
/// the mods folder with '/' separators, as [`crate::loader::foreign_files`]
/// gives them. The Legacy originals are kept under `backup` (same relative
/// paths) when given. Each archive is saved once, after all its files.
pub fn convert_in_mods(game: &Game, keys: Option<&Keys>, tool: &Path, paths: &[String], backup: Option<&Path>) -> Result<Vec<Converted>, Error> {
    // Top-level archive -> [(nested archives, file inside the innermost), full path].
    let mut by_archive: BTreeMap<String, Vec<Placed>> = BTreeMap::new();
    let mut report = Vec::new();
    for path in paths {
        match split_archives(path) {
            Some((top, nested, file)) => by_archive.entry(top).or_default().push((nested, file, path.clone())),
            None => report.push(Converted { path: path.clone(), error: Some("not inside an archive".into()) }),
        }
    }
    for (top, files) in by_archive {
        report.extend(convert_archive(&game.mods_dir().join(&top), keys, tool, &files, backup)?);
    }
    report.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// Converts files of one archive on disk; `files` are (nested archives, file
/// inside the innermost, path for the report and the backup).
fn convert_archive(
    disk: &Path,
    keys: Option<&Keys>,
    tool: &Path,
    files: &[Placed],
    backup: Option<&Path>,
) -> Result<Vec<Converted>, Error> {
    let mut editor = ArchiveEditor::open(disk, keys)?;
    let mut data = Vec::new();
    for (nested, file, _) in files {
        data.push(inner(&mut editor, nested, keys)?.read(file, keys)?);
    }
    let inputs: Vec<(&str, &[u8])> = files.iter().zip(&data).map(|((_, f, _), d)| (f.as_str(), d.as_slice())).collect();
    let outcomes = convert(tool, &inputs)?;
    let mut changed = false;
    let mut report = Vec::new();
    for (((nested, file, path), original), outcome) in files.iter().zip(&data).zip(outcomes) {
        let error = match outcome {
            Outcome::Converted(new) => {
                if let Some(dir) = backup {
                    let dest = dir.join(path);
                    std::fs::create_dir_all(dest.parent().unwrap())?;
                    std::fs::write(dest, original)?;
                }
                inner(&mut editor, nested, keys)?.add_file(file, new)?;
                changed = true;
                None
            }
            Outcome::Unchanged => Some("already in the Enhanced format".into()),
            Outcome::Failed(e) => Some(e),
        };
        report.push(Converted { path: path.clone(), error });
    }
    if changed {
        editor.save(disk)?;
    }
    Ok(report)
}

/// The files of an archive (nested archives too) that the Enhanced game needs
/// converted, as paths inside it with '/' separators.
pub fn legacy_in_archive(archive: &Archive, keys: Option<&Keys>) -> Vec<String> {
    let mut found = Vec::new();
    let mut queue = vec![(String::new(), None::<Archive>)];
    while let Some((prefix, child)) = queue.pop() {
        let archive = child.as_ref().unwrap_or(archive);
        for (path, i) in archive.walk(0) {
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
    convert_archive(disk, keys, tool, &files, None)
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

/// Reads files on disk (as [`legacy_in_local`] takes them), converting the
/// Legacy ones with `tool`: loose resources, and the resources inside archives
/// (the archive on disk is left alone). One data per file, in order, and a
/// log of what was converted or could not be.
pub fn read_converting(files: &[(PathBuf, String)], tool: &Path, keys: Option<&Keys>) -> Result<(Vec<Vec<u8>>, Vec<String>), Error> {
    let mut data = Vec::new();
    let mut log = Vec::new();
    let mut loose = Vec::new();
    for (i, (path, rel)) in files.iter().enumerate() {
        let rel = rel.replace('\\', "/");
        let bytes = std::fs::read(path)?;
        if needs_conversion(&rel, &bytes) {
            loose.push((i, rel));
        } else if rel.to_ascii_lowercase().ends_with(".rpf")
            && Archive::open(path, keys).is_ok_and(|a| !legacy_in_archive(&a, keys).is_empty())
        {
            let dir = scratch_dir()?;
            let copy = dir.join("archive.rpf");
            std::fs::write(&copy, &bytes)?;
            let result = convert_whole_archive(&copy, keys, tool).and_then(|report| {
                for r in &report {
                    log.push(match &r.error {
                        None => format!("converted {rel}/{}", r.path),
                        Some(e) => format!("{rel}/{}: not converted: {e}", r.path),
                    });
                }
                Ok(std::fs::read(&copy)?)
            });
            let _ = std::fs::remove_dir_all(&dir);
            data.push(result?);
            continue;
        }
        data.push(bytes);
    }
    if !loose.is_empty() {
        let inputs: Vec<(&str, &[u8])> = loose.iter().map(|(i, rel)| (rel.as_str(), data[*i].as_slice())).collect();
        let outcomes = convert(tool, &inputs)?;
        for ((i, rel), outcome) in loose.iter().zip(outcomes) {
            match outcome {
                Outcome::Converted(new) => {
                    data[*i] = new;
                    log.push(format!("converted {rel}"));
                }
                Outcome::Unchanged => {}
                Outcome::Failed(e) => log.push(format!("{rel}: not converted: {e}")),
            }
        }
    }
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
        let backup = dir.path().join("backup");
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
        let kept = root.join("mods_legacy/1700000000");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::rename(&backup, &kept).unwrap();
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
    fn a_stuck_converter_is_stopped() {
        let start = Instant::now();
        let err = output_within(Command::new("sleep").arg("30"), Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("was stopped"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(10));
        let out = output_within(Command::new("sh").args(["-c", "echo out; echo err >&2"]), Duration::from_secs(30)).unwrap();
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice()), (&b"out\n"[..], &b"err\n"[..]));
    }

    #[test]
    fn scratch_folders_are_new_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let first = scratch_dir().unwrap();
        // The next name already taken, as someone else could in a shared /tmp.
        let n: u32 = first.to_string_lossy().rsplit('-').next().unwrap().parse().unwrap();
        let taken = std::env::temp_dir().join(format!("paleto-{}-{}", std::process::id(), n + 1));
        let _ = std::fs::create_dir(&taken);
        let next = scratch_dir().unwrap();
        assert_ne!(next, taken);
        assert_eq!(std::fs::metadata(&next).unwrap().permissions().mode() & 0o777, 0o700);
        for dir in [first, taken, next] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
