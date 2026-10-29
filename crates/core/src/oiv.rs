//! OpenIV packages (`.oiv`, format 2.x): a zip with `assembly.xml`, an
//! optional `icon.png` and a `content/` folder.
//!
//! Installation always targets the mods folder for game data; the original
//! game files are never modified.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use paleto_rpf::{ArchiveEditor, Keys};
use zip::ZipArchive;

use crate::budget::{self, Budget, Limits, Scratch};
use crate::convert;
use crate::fsroot::{Dir, Existing, Fingerprint, GameRoots, Resolved, RootKind};
use crate::game::Game;
use crate::relpath::RelPath;
use crate::txn::{self, TxnFailure};
use crate::text::{self, Condition, TextOp};
use crate::xml::{self, Append, XmlOp};
use crate::Error;

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub format_version: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

#[derive(Debug, Clone)]
enum Command {
    Add { source: String, target: String },
    Delete(String),
    Archive { path: String, create: bool, body: Vec<Command> },
    Text { path: String, create: bool, ops: Vec<TextOp> },
    Xml { path: String, ops: Vec<XmlOp> },
    /// XML commands on a binary metadata file (PSO or resource metadata),
    /// with the command's source text to learn names from.
    Pso { path: String, ops: Vec<XmlOp>, source: String },
    Defragment(String),
}

pub struct Package {
    entries: Entries,
    pub metadata: Metadata,
    content: Vec<Command>,
    /// Content converted to the Enhanced format, by zip entry name (lower case),
    /// installed in place of the package's own.
    converted: HashMap<String, Converted>,
}

/// Converted content: small files are held, an archive stays in a file (its folder already gone).
enum Converted {
    Bytes(Vec<u8>),
    File(File, u64),
}

/// How many nodes `assembly.xml` may have (a legitimate one has a few thousand).
const MAX_NODES: u32 = 2_000_000;

impl Package {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_with(path, Limits::PACKAGE)
    }

    /// [`Package::open`] with other limits (tests use small ones).
    pub(crate) fn open_with(path: &Path, limits: Limits) -> Result<Self, Error> {
        let mut file = File::open(path)?;
        // Before the zip reader allocates for the list of files.
        budget::zip_precheck(&mut file, &limits)?;
        let zip = budget::zip_open(file).map_err(zip_err)?;
        let mut entries = Entries::new(zip, limits)?;
        let assembly = entries.read("assembly.xml", limits.manifest)?;
        let assembly = String::from_utf8_lossy(assembly.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&assembly)).into_owned();
        let options = roxmltree::ParsingOptions { nodes_limit: MAX_NODES, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(&assembly, options).map_err(|e| Error::Package(format!("assembly.xml: {e}")))?;
        let root = doc.root_element();
        if !root.has_tag_name("package") {
            return Err(Error::Package("assembly.xml: root is not <package>".into()));
        }
        let format_version = root.attribute("version").unwrap_or("").to_string();
        if !format_version.starts_with("2.") {
            return Err(Error::Package(format!("unsupported package format {format_version}")));
        }
        if let Some(target) = root.attribute("target")
            && !target.eq_ignore_ascii_case("five") {
                return Err(Error::Package(format!("package is for another game ({target})")));
            }

        let meta = child(root, "metadata");
        let text_of = |path: &[&str]| -> String {
            let mut node = meta;
            for p in path {
                node = node.and_then(|n| child(n, p));
            }
            node.and_then(|n| n.text()).unwrap_or("").trim().to_string()
        };
        let mut version = format!("{}.{}", text_of(&["version", "major"]), text_of(&["version", "minor"]));
        let tag = text_of(&["version", "tag"]);
        if !tag.is_empty() {
            version = format!("{version} {tag}");
        }
        let metadata = Metadata {
            format_version,
            id: root.attribute("id").unwrap_or("").to_string(),
            name: text_of(&["name"]),
            version,
            author: text_of(&["author", "displayName"]),
            description: text_of(&["description"]),
        };

        let content = child(root, "content").ok_or_else(|| Error::Package("assembly.xml: missing <content>".into()))?;
        let content = parse_commands(content, assembly.as_str())?;
        Ok(Package { entries, metadata, content, converted: HashMap::new() })
    }

    /// The package's picture; none if there is none or it is over the limit (it is not read then).
    pub fn icon(&mut self) -> Option<Vec<u8>> {
        self.entries.fresh();
        self.entries.read("icon.png", self.entries.budget.limits().icon).ok()
    }

    /// The `<add>` sources, at every level.
    fn add_sources(&self) -> Vec<String> {
        fn walk(cmds: &[Command], out: &mut Vec<String>) {
            for cmd in cmds {
                match cmd {
                    Command::Add { source, .. } => out.push(source.clone()),
                    Command::Archive { body, .. } => walk(body, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.content, &mut out);
        out.sort();
        out.dedup();
        out
    }

    /// The package's files that the Enhanced game needs converted: models,
    /// textures and particles made for Legacy, loose or inside the archives it
    /// adds ("dlc.rpf/x64/vehicles.rpf/car.yft"). Paths use '/' separators.
    pub fn legacy_files(&mut self, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
        self.entries.fresh();
        let mut found = Vec::new();
        for source in self.add_sources() {
            let name = source.replace('\\', "/");
            let entry = content_path(&source);
            if name.to_ascii_lowercase().ends_with(".rpf") {
                // An archive is looked into on disk, never held.
                let dir = Scratch::new()?;
                let disk = dir.path().join("archive.rpf");
                self.entries.copy(&entry, &mut dir.create("archive.rpf")?)?;
                if let Ok(archive) = paleto_rpf::Archive::open(&disk, keys) {
                    found.extend(convert::legacy_in_archive(&archive, keys).into_iter().map(|p| format!("{name}/{p}")));
                }
            } else if convert::convertible(&name) && convert::needs_conversion(&name, &self.entries.head(&entry)?) {
                found.push(name);
            }
        }
        Ok(found)
    }

    /// Converts the package's Legacy files (see [`Package::legacy_files`])
    /// with `tool`, so that [`Package::install`] installs the Enhanced ones.
    /// Returns a log; files that cannot be converted are reported there and
    /// installed as they are.
    pub fn convert_legacy(&mut self, tool: &Path, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
        self.entries.fresh();
        let in_memory = self.entries.budget.limits().in_memory;
        let mut log = Vec::new();
        let mut loose = Vec::new();
        for source in self.add_sources() {
            let name = source.replace('\\', "/");
            let entry = content_path(&source).to_ascii_lowercase();
            if name.to_ascii_lowercase().ends_with(".rpf") {
                let dir = Scratch::new()?;
                let disk = dir.path().join("archive.rpf");
                self.entries.copy(&entry, &mut dir.create("archive.rpf")?)?;
                let result = convert::convert_whole_archive(&disk, keys, tool);
                let converted = result.and_then(|report| {
                    for r in &report {
                        log.push(match &r.error {
                            None => format!("converted {name}/{}", r.path),
                            Some(e) => format!("warning: {name}/{}: not converted: {e}", r.path),
                        });
                    }
                    // The file stays readable through its handle once the folder is gone.
                    Ok(report.iter().any(|r| r.error.is_none()).then(|| File::open(&disk).and_then(|f| f.metadata().map(|m| (f, m.len())))).transpose()?)
                });
                if let Some((file, len)) = converted? {
                    self.converted.insert(entry, Converted::File(file, len));
                }
            } else if convert::convertible(&name) && convert::needs_conversion(&name, &self.entries.head(&entry)?) {
                let data = self.entries.read(&entry, in_memory)?;
                loose.push((name, entry, data));
            }
        }
        if !loose.is_empty() {
            let inputs: Vec<(&str, &[u8])> = loose.iter().map(|(n, _, d)| (n.as_str(), d.as_slice())).collect();
            for ((name, entry, _), outcome) in loose.iter().zip(convert::convert(tool, &inputs)?) {
                match outcome {
                    convert::Outcome::Converted(new) => {
                        self.converted.insert(entry.clone(), Converted::Bytes(new));
                        log.push(format!("converted {name}"));
                    }
                    convert::Outcome::Unchanged => {}
                    convert::Outcome::Failed(e) => log.push(format!("warning: {name}: not converted: {e}")),
                }
            }
        }
        Ok(log)
    }

    /// Installs the package. Game data goes to the mods folder (copying the
    /// originals there first); other files go to the game folder. Returns a log
    /// of what was done, including warnings.
    pub fn install(&mut self, game: &Game, keys: &Keys) -> Result<Vec<String>, Error> {
        self.install_with(game, Some(keys))
    }

    /// [`Package::install`], with keys only needed for encrypted archives.
    pub(crate) fn install_with(&mut self, game: &Game, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
        self.check(&self.content.clone(), false)?;
        self.entries.fresh();
        let roots = game.roots()?;
        // One change at a time per game, for the whole install.
        let _lock = txn::guard(&roots, "installing a package")?;
        let mut inst = Installer { roots: &roots, keys, entries: &mut self.entries, converted: &self.converted, archives: BTreeMap::new(), log: Vec::new(), undo: Undo::default() };
        if let Err(e) = self.content.clone().iter().try_for_each(|cmd| inst.run_root(cmd)) {
            // Archives are only saved at the end: putting the loose files back undoes it all.
            let (failures, notes) = inst.undo.roll_back();
            inst.log.extend(notes.into_iter().map(|n| format!("warning: {n}")));
            return Err(Error::Txn(Box::new(TxnFailure { cause: e, log: inst.log, undone: failures.is_empty(), failures, journaled: false, verb: "installed" })));
        }
        let left = inst.undo.finish();
        inst.log.extend(left.into_iter().map(|l| format!("warning: {l}")));
        for (path, editor) in std::mem::take(&mut inst.archives) {
            editor.save(&path)?;
            inst.log.push(format!("saved {}", path.display()));
        }
        Ok(inst.log)
    }

    /// Fails early on anything we cannot do, before touching any file: a missing file, or one
    /// whose header already says it is more than an install may take (`in_archive`: inside one).
    fn check(&mut self, cmds: &[Command], in_archive: bool) -> Result<(), Error> {
        let limits = *self.entries.budget.limits();
        for cmd in cmds {
            match cmd {
                Command::Add { source, target } => {
                    let entry = content_path(source);
                    let (declared, stored) = self.entries.declared(&entry).map_err(|_| Error::Package(format!("missing content/{source}")))?;
                    // What will be added is the converted file if there is one.
                    let size = match self.converted.get(&entry.to_ascii_lowercase()) {
                        Some(Converted::Bytes(data)) => data.len() as u64,
                        Some(Converted::File(_, len)) => *len,
                        None => {
                            self.entries.budget.check_ratio(declared, stored, &entry)?;
                            declared
                        }
                    };
                    within(&entry, size, add_cap(&limits, in_archive, target))?;
                }
                Command::Archive { body, .. } => self.check(body, true)?,
                _ => {}
            }
        }
        Ok(())
    }
}

struct Installer<'a> {
    /// Where files may be written: every destination is resolved through it.
    roots: &'a GameRoots,
    keys: Option<&'a Keys>,
    entries: &'a mut Entries,
    converted: &'a HashMap<String, Converted>,
    /// Top-level archives being edited, by path in the mods folder.
    archives: BTreeMap<PathBuf, ArchiveEditor>,
    log: Vec<String>,
    undo: Undo,
}

/// What an install changed on disk so far, to put back if it fails halfway: an interim
/// until the install runs as a transaction (see [`crate::txn`]), and without a journal.
/// Files are only ever replaced by a rename (never rewritten in place), so a hard link
/// keeps the old content (the file itself is renamed aside on a filesystem without hard
/// links). Backups are made under names nobody else can have, in the folder of the file,
/// and only what the install made or kept is touched when undoing: a name that is not
/// the one it wrote is left alone, and whatever cannot be put back is reported with the
/// name of its backup, never hidden. A failure while the archives are saved, at the very
/// end, is not undone.
#[derive(Default)]
struct Undo {
    seen: HashSet<PathBuf>,
    steps: Vec<Step>,
    /// Folders made for the install that could not be noted, so cannot be removed again.
    lost: Vec<String>,
}

enum Step {
    /// A file that did not exist: removed again if it is still the one the install wrote.
    Made { dir: Dir, name: String, path: PathBuf, wrote: Option<Fingerprint> },
    /// A folder made for a file: removed again if it is empty.
    Folder { parent: Dir, name: String, path: PathBuf },
    /// A file replaced or deleted: its old content is under `backup` (a hard link to it, or the file itself renamed aside).
    Kept { dir: Dir, name: String, backup: String, path: PathBuf, wrote: Option<Fingerprint> },
}

/// A filesystem that cannot make hard links (exFAT, some network mounts).
fn links_unsupported(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EPERM | libc::EOPNOTSUPP | libc::EXDEV | libc::EMLINK)) || e.kind() == io::ErrorKind::Unsupported
}

/// Where a destination was before its folders existed (they are made later, by somebody else).
struct Planned {
    anchor: RootKind,
    logical: Vec<String>,
    missing: Vec<String>,
    name: String,
    path: PathBuf,
}

impl Planned {
    fn of(dest: &Resolved) -> Self {
        Planned { anchor: dest.anchor(), logical: dest.logical.clone(), missing: dest.missing.clone(), name: dest.name.clone(), path: dest.path() }
    }
}

impl Undo {
    /// Remembers a file before its first change: a hard link to it under a name of its own, or
    /// (`aside`, where links cannot be made) the file itself renamed to one; or, if there is
    /// no file, that the install made it. `Ok(true)` if the file was moved aside (it is not at
    /// its name any more). Without `aside`, a filesystem that cannot link is left alone and
    /// nothing is remembered: the caller asks again right before it writes. The folders must exist.
    fn keep(&mut self, dest: &Resolved, aside: bool) -> Result<bool, Error> {
        let path = dest.path();
        if self.seen.contains(&path) {
            return Ok(false);
        }
        let dir = &dest.parent;
        match dir.stat(&dest.name)? {
            // The write that follows refuses a folder.
            Some(Existing::Dir) => return Ok(false),
            Some(_) => {
                let (backup, moved) = match dir.link_unique(&dest.name, ".old") {
                    Ok(backup) => (backup, false),
                    Err(e) if links_unsupported(&e) => {
                        if !aside {
                            return Ok(false);
                        }
                        (dir.rename_unique(&dest.name, ".old")?, true)
                    }
                    Err(e) => return Err(e.into()),
                };
                self.steps.push(Step::Kept { dir: dir.try_clone()?, name: dest.name.clone(), backup, path: path.clone(), wrote: None });
                self.seen.insert(path);
                return Ok(moved);
            }
            None => self.steps.push(Step::Made { dir: dir.try_clone()?, name: dest.name.clone(), path: path.clone(), wrote: None }),
        }
        self.seen.insert(path);
        Ok(false)
    }

    /// The install has written `dest` (which now says what is there): that is what undoing may remove or replace.
    fn wrote(&mut self, dest: &Resolved) {
        let Some(Existing::File(fp)) = &dest.existing else { return };
        let path = dest.path();
        for step in &mut self.steps {
            if let Step::Made { path: p, wrote, .. } | Step::Kept { path: p, wrote, .. } = step
                && *p == path
            {
                *wrote = Some(*fp);
            }
        }
    }

    /// A folder the install made.
    fn folder(&mut self, parent: &Dir, name: &str, path: PathBuf) {
        match parent.try_clone() {
            Ok(parent) => self.steps.push(Step::Folder { parent, name: name.to_string(), path }),
            Err(e) => self.lost.push(format!("{}: a folder made by the install cannot be removed again: {e}", path.display())),
        }
    }

    /// A file that did not exist at `plan` was made by somebody else for the install (a copy of a
    /// game file): remembers it, and the folders made for it. Nothing if there is no such file.
    fn made_by_copy(&mut self, roots: &GameRoots, plan: &Planned) -> Result<(), Error> {
        let logical = [plan.logical.as_slice(), plan.missing.as_slice()].concat();
        let mut folders = Vec::new();
        let mut path = plan.path.clone();
        path.pop();
        for name in plan.missing.iter().rev() {
            folders.push((name.clone(), path.clone()));
            path.pop();
        }
        let dir = match roots.open_anchor(plan.anchor, &logical) {
            Ok(dir) => dir,
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(Existing::File(fp)) = dir.stat(&plan.name)? else { return Ok(()) };
        // Outermost folder first, so that undoing (newest first) removes the innermost first.
        let mut parent_logical = plan.logical.clone();
        for (name, path) in folders.into_iter().rev() {
            let parent = roots.open_anchor(plan.anchor, &parent_logical)?;
            self.folder(&parent, &name, path);
            parent_logical.push(name);
        }
        self.steps.push(Step::Made { dir, name: plan.name.clone(), path: plan.path.clone(), wrote: Some(fp) });
        self.seen.insert(plan.path.clone());
        Ok(())
    }

    /// Puts everything back, newest change first. Returns what could not be put back (each with the
    /// name of its backup: empty means nothing is left of the install) and what was left in place on purpose.
    fn roll_back(&mut self) -> (Vec<String>, Vec<String>) {
        #[cfg(test)]
        if let Some(hook) = ON_ROLLBACK.with(|h| h.borrow_mut().take()) {
            hook();
        }
        let mut failures = std::mem::take(&mut self.lost);
        let mut notes = Vec::new();
        for step in self.steps.drain(..).rev() {
            match step {
                Step::Made { dir, name, path, wrote } => match dir.stat(&name) {
                    Ok(None) => {}
                    Ok(Some(Existing::File(fp))) if wrote.is_some_and(|w| fp.same_file(&w)) => match dir.unlink(&name) {
                        Ok(()) => {
                            let _ = dir.sync();
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => failures.push(format!("{}: made by the install and cannot be removed: {e}", path.display())),
                    },
                    Ok(Some(_)) => failures.push(format!("{}: made by the install, but what is there now is not the file it wrote; left as it is", path.display())),
                    Err(e) => failures.push(format!("{}: cannot be looked at: {e}", path.display())),
                },
                Step::Folder { parent, name, path } => match parent.rmdir(&name) {
                    Ok(()) => {
                        let _ = parent.sync();
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => notes.push(format!("{}: a folder made by the install is not empty, left in place", path.display())),
                    Err(e) => failures.push(format!("{}: a folder made by the install cannot be removed: {e}", path.display())),
                },
                Step::Kept { dir, name, backup, path, wrote } => {
                    let shown = format!("{} (backup: {})", path.display(), path.with_file_name(&backup).display());
                    let (target, saved) = match (dir.stat(&name), dir.stat(&backup)) {
                        (Ok(t), Ok(Some(s))) => (t, s),
                        (Ok(_), Ok(None)) => {
                            failures.push(format!("{shown}: the backup is gone, so the original cannot be put back"));
                            continue;
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            failures.push(format!("{shown}: cannot be looked at: {e}"));
                            continue;
                        }
                    };
                    let untouched = match (&target, &saved) {
                        (Some(Existing::File(a)), Existing::File(b)) => a.same_file(b),
                        (Some(Existing::Link(a)), Existing::Link(b)) => a == b,
                        _ => false,
                    };
                    let ours = matches!((&target, &wrote), (Some(Existing::File(fp)), Some(w)) if fp.same_file(w));
                    let result = if untouched {
                        // The name was not changed: the backup is a second name of the same file.
                        dir.unlink(&backup)
                    } else if target.is_none() {
                        dir.rename(&backup, &name, false)
                    } else if ours {
                        dir.rename(&backup, &name, true)
                    } else {
                        failures.push(format!("{shown}: what is there now is not what the install left (it was changed, or is a folder now); the original was not put back"));
                        continue;
                    };
                    match result {
                        Ok(()) => {
                            let _ = dir.sync();
                        }
                        Err(e) => failures.push(format!("{shown}: the original cannot be put back: {e}")),
                    }
                }
            }
        }
        self.seen.clear();
        (failures, notes)
    }

    /// The install is done: the backups go. Returns the ones that could not be removed.
    fn finish(&mut self) -> Vec<String> {
        let mut left = Vec::new();
        for step in self.steps.drain(..) {
            if let Step::Kept { dir, backup, path, .. } = step {
                match dir.unlink(&backup) {
                    Ok(()) => {
                        let _ = dir.sync();
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => left.push(format!("the backup {} of {} could not be removed: {e}", path.with_file_name(&backup).display(), path.display())),
                }
            }
        }
        left
    }
}

// Runs once when an install is rolled back (tests damage the folder to make it fail).
#[cfg(test)]
thread_local! {
    pub(crate) static ON_ROLLBACK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

impl Installer<'_> {
    /// The data of an `<add>` source (the converted file if there is one) in memory, within the caps.
    fn source(&mut self, source: &str) -> Result<Vec<u8>, Error> {
        let entry = content_path(source);
        let cap = self.entries.budget.limits().in_memory;
        match self.converted.get(&entry.to_ascii_lowercase()) {
            Some(Converted::Bytes(data)) => self.entries.budget.read(&mut data.as_slice(), data.len() as u64, cap, &entry),
            Some(Converted::File(file, len)) => self.entries.budget.read(&mut rewound(file)?, *len, cap, &entry),
            None => self.entries.read(&entry, cap),
        }
    }

    /// An `<add>` source written to `out`, a piece at a time.
    fn write_source(&mut self, source: &str, out: &mut File) -> Result<(), Error> {
        let entry = content_path(source);
        match self.converted.get(&entry.to_ascii_lowercase()) {
            Some(Converted::Bytes(data)) => self.entries.budget.copy(&mut data.as_slice(), data.len() as u64, out, &entry)?,
            Some(Converted::File(file, len)) => self.entries.budget.copy(&mut rewound(file)?, *len, out, &entry)?,
            None => self.entries.copy(&entry, out)?,
        };
        Ok(())
    }

    /// An `<add>` source as a file, and its length: for the archives an archive takes from disk.
    fn source_file(&mut self, source: &str) -> Result<(File, u64), Error> {
        if let Some(Converted::File(file, len)) = self.converted.get(&content_path(source).to_ascii_lowercase()) {
            return Ok((file.try_clone()?, *len));
        }
        Scratch::spool(|out| {
            self.write_source(source, out)?;
            Ok(out.stream_position()?)
        })
    }

    /// A command at the game folder level.
    fn run_root(&mut self, cmd: &Command) -> Result<(), Error> {
        match cmd {
            Command::Add { source, target } => {
                let mut dest = self.roots.dest(&RelPath::parse(target)?)?;
                if let Some(why) = self.refused(&dest) {
                    self.log.push(format!("warning: refusing to replace {}{why}", dest.path().display()));
                    return Ok(());
                }
                self.ensure_dirs(&mut dest)?;
                let path = dest.path();
                self.archives.remove(&path);
                self.undo.keep(&dest, true)?;
                // A copy in the mods folder may be a link to an original: replace it, never write through it.
                // The source goes into the new file a piece at a time; a failure of its own is not an I/O error of the file.
                let (roots, mut failed) = (self.roots, None);
                let written = roots.replace(&mut dest, |f| self.write_source(source, f).map_err(|e| io::Error::other(failed.insert(e).to_string())));
                if let Some(e) = failed {
                    return Err(e);
                }
                written?;
                self.undo.wrote(&dest);
                self.log.push(format!("add {}", path.display()));
            }
            Command::Delete(target) => {
                // Game data is only ever deleted from the mods folder.
                let dest = self.roots.dest(&RelPath::parse(target)?)?;
                let path = dest.path();
                if let Some(why) = self.refused(&dest) {
                    self.log.push(format!("warning: refusing to delete {}{why}", path.display()));
                } else if self.roots.read_dest(&dest)?.is_some() {
                    // Without hard links the file itself was renamed aside as its backup: it is gone from its name already.
                    if !self.undo.keep(&dest, true)? {
                        self.roots.remove(&dest)?;
                    }
                    self.log.push(format!("delete {}", path.display()));
                }
            }
            Command::Archive { path, create, body } => {
                let disk = self.top_archive(path, *create)?;
                let mut editor = self.archives.remove(&disk).unwrap();
                let result = body.iter().try_for_each(|c| self.run_in(&mut editor, c, path));
                self.archives.insert(disk, editor);
                result?;
            }
            Command::Text { path, create, ops } => {
                let Some(mut dest) = self.loose_file(path)? else { return Ok(()) };
                let (text, latin1) = match self.read_loose(&dest)? {
                    Some(b) => decode_text(b),
                    None if *create => (String::new(), false),
                    None => return Err(Error::Package(format!("{path} not found"))),
                };
                let out = self.encode_text(path, &text::apply(&text, ops), latin1);
                self.write_loose(&mut dest, out)?;
                self.log.push(format!("edit {}", dest.path().display()));
            }
            Command::Xml { path, ops } => {
                let Some(mut dest) = self.loose_file(path)? else { return Ok(()) };
                let data = self.read_loose(&dest)?.ok_or_else(|| Error::Package(format!("{path} not found")))?;
                let (out, warnings) = xml::apply(&data, ops)?;
                self.warn(path, warnings);
                self.write_loose(&mut dest, out)?;
                self.log.push(format!("edit {}", dest.path().display()));
            }
            Command::Defragment(path) => {
                let disk = self.top_archive(path, false)?;
                self.archives.get_mut(&disk).unwrap().defragment();
            }
            Command::Pso { path, ops, source } => {
                let Some(mut dest) = self.loose_file(path)? else { return Ok(()) };
                let data = self.read_loose(&dest)?.ok_or_else(|| Error::Package(format!("{path} not found")))?;
                let out = self.edit_meta(path, &data, ops, source)?;
                self.write_loose(&mut dest, out)?;
                self.log.push(format!("edit {}", dest.path().display()));
            }
        }
        Ok(())
    }

    fn edit_meta(&mut self, path: &str, data: &[u8], ops: &[XmlOp], source: &str) -> Result<Vec<u8>, Error> {
        let (out, warnings) = edit_meta(path, data, ops, source)?;
        self.warn(path, warnings);
        Ok(out)
    }

    /// A command inside an archive; `at` is for messages.
    fn run_in(&mut self, editor: &mut ArchiveEditor, cmd: &Command, at: &str) -> Result<(), Error> {
        let keys = self.keys;
        match cmd {
            Command::Add { source, target } => {
                if spools(target) {
                    // An archive, sound or video is attached from a file, not held.
                    let (file, len) = self.source_file(source)?;
                    editor.add_file_from(target, file, len)?;
                } else {
                    editor.add_file(target, self.source(source)?)?;
                }
                self.log.push(format!("add {at}\\{target}"));
            }
            Command::Delete(target) => {
                if editor.delete(target)? {
                    self.log.push(format!("delete {at}\\{target}"));
                }
            }
            Command::Archive { path, create, body } => {
                let inner = editor.archive(path, *create, keys)?;
                let at = format!("{at}\\{path}");
                body.iter().try_for_each(|c| self.run_in(inner, c, &at))?;
            }
            Command::Text { path, create, ops } => {
                let (text, latin1) = match editor.read(path, keys) {
                    Ok(b) => decode_text(b),
                    Err(paleto_rpf::Error::NotFound(_)) if *create => (String::new(), false),
                    Err(e) => return Err(e.into()),
                };
                let out = self.encode_text(&format!("{at}\\{path}"), &text::apply(&text, ops), latin1);
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}\\{path}"));
            }
            Command::Xml { path, ops } => {
                let (out, warnings) = xml::apply(&editor.read(path, keys)?, ops)?;
                self.warn(&format!("{at}\\{path}"), warnings);
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}\\{path}"));
            }
            Command::Defragment(path) => editor.archive(path, false, keys)?.defragment(),
            Command::Pso { path, ops, source } => {
                let at = format!("{at}\\{path}");
                let out = self.edit_meta(&at, &editor.read(path, keys)?, ops, source)?;
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}"));
            }
        }
        Ok(())
    }

    /// Opens (once per install) a top-level archive in the mods folder.
    fn top_archive(&mut self, path: &str, create: bool) -> Result<PathBuf, Error> {
        let rel = RelPath::parse(path)?;
        let mut dest = self.roots.dest(&rel)?;
        let disk = dest.path();
        if self.archives.contains_key(&disk) {
            return Ok(disk);
        }
        // The archive is saved at the end: it must pass what any write passes (no programs, no
        // game data in the game folder), whether it exists or is created.
        self.roots.check_write(&dest)?;
        // A copy made from the original goes again if the install fails. An
        // archive already there is only saved at the end: nothing to keep.
        let (was_new, plan) = (dest.existing.is_none(), Planned::of(&dest));
        let disk = match self.roots.mods_copy(&rel.without_mods())? {
            Some(p) => {
                if was_new {
                    self.undo.made_by_copy(self.roots, &plan)?;
                }
                p
            }
            // Saved by a rename: a dangling link there is replaced, never written through.
            None if create => {
                self.ensure_dirs(&mut dest)?;
                self.roots.check_write(&dest)?;
                disk
            }
            None => return Err(Error::Package(format!("archive {path} not found"))),
        };
        {
            let editor = if disk.is_file() { ArchiveEditor::open(&disk, self.keys)? } else { ArchiveEditor::new(&disk.file_name().unwrap().to_string_lossy()) };
            self.archives.insert(disk.clone(), editor);
        }
        Ok(disk)
    }

    /// Why a package may not replace or delete this destination, as text to
    /// append to a warning: programs and the game's own libraries and notes in
    /// the game folder are left alone, and the rest of the package installs.
    fn refused(&self, dest: &Resolved) -> Option<&'static str> {
        if is_exe(Path::new(&dest.name)) {
            Some("")
        } else if self.roots.is_game_file(dest) {
            Some(" (one of the game's own files; mod loaders excepted)")
        } else {
            None
        }
    }

    /// A loose file to edit: game data is copied to the mods folder first.
    /// `None` (after a warning) for one the package may not change.
    fn loose_file(&mut self, path: &str) -> Result<Option<Resolved>, Error> {
        let rel = RelPath::parse(path)?;
        let dest = self.roots.dest(&rel)?;
        if self.roots.is_game_file(&dest) {
            self.log.push(format!("warning: refusing to replace {}{}", dest.path().display(), self.refused(&dest).unwrap_or_default()));
            return Ok(None);
        }
        if dest.root == RootKind::Mods && !rel.in_mods_folder() {
            // Game data is edited in a copy in the mods folder, made first: a copy that did not
            // exist goes again if the install fails, one that did (a link, say) is kept.
            let plan = Planned::of(&dest);
            if dest.existing.is_some() {
                self.undo.keep(&dest, false)?;
            }
            self.roots.mods_copy(&rel)?;
            if dest.existing.is_none() {
                self.undo.made_by_copy(self.roots, &plan)?;
            }
            return Ok(Some(self.roots.dest(&rel)?));
        }
        Ok(Some(dest))
    }

    /// Makes the folders `dest` needs, noting each so that a failed install removes it again.
    fn ensure_dirs(&mut self, dest: &mut Resolved) -> Result<(), Error> {
        let mut paths = Vec::new();
        let mut path = dest.path();
        path.pop();
        for _ in &dest.missing {
            paths.push(path.clone());
            path.pop();
        }
        paths.reverse();
        let (roots, undo, mut made) = (self.roots, &mut self.undo, 0);
        roots.ensure_parent_noting(dest, &mut |parent, name| {
            undo.folder(parent, name, paths[made].clone());
            made += 1;
        })
    }

    /// What a loose file holds now; `None` if there is no file.
    fn read_loose(&mut self, dest: &Resolved) -> Result<Option<Vec<u8>>, Error> {
        let mut data = Vec::new();
        Ok(self.roots.read_dest(dest)?.map(|mut f| f.read_to_end(&mut data).map(|_| data)).transpose()?)
    }

    /// Writes a loose file by replacing it: never through a link, and the
    /// old content stays for [`Undo`].
    fn write_loose(&mut self, dest: &mut Resolved, data: Vec<u8>) -> Result<(), Error> {
        self.ensure_dirs(dest)?;
        self.undo.keep(dest, true)?;
        self.roots.replace(dest, |f| f.write_all(&data))?;
        self.undo.wrote(dest);
        Ok(())
    }

    /// A text edited by `<text>`, in the encoding it was read in.
    fn encode_text(&mut self, path: &str, text: &str, latin1: bool) -> Vec<u8> {
        if !latin1 {
            return text.as_bytes().to_vec();
        }
        if text.chars().any(|c| c as u32 > 0xFF) {
            self.log.push(format!("warning: {path}: characters the file's encoding cannot hold were written as ?"));
        }
        text.chars().map(|c| u8::try_from(c as u32).unwrap_or(b'?')).collect()
    }

    fn warn(&mut self, path: &str, warnings: Vec<String>) {
        self.log.extend(warnings.into_iter().map(|w| format!("warning: {path}: {w}")));
    }
}

/// A `<pso>` command: converts a metadata file to XML, applies the XML
/// commands and converts it back. Names in the command's text are learned
/// first, so values it mentions by name show by name.
fn edit_meta(path: &str, data: &[u8], ops: &[XmlOp], source: &str) -> Result<(Vec<u8>, Vec<String>), Error> {
    let fail = |e: paleto_formats::Error| Error::Package(format!("{path}: {e}"));
    let mut meta = paleto_formats::MetaFile::parse(data).map_err(fail)?;
    meta.learn_names(source);
    let (xml, warnings) = xml::apply(meta.to_xml().as_bytes(), ops)?;
    let out = meta.rebuild_from_xml(&String::from_utf8_lossy(&xml)).map_err(fail)?;
    Ok((out, warnings))
}

fn parse_commands(node: roxmltree::Node, src: &str) -> Result<Vec<Command>, Error> {
    let mut out = Vec::new();
    for n in node.children().filter(|n| n.is_element()) {
        let attr = |name: &str| -> Result<String, Error> {
            n.attribute(name)
                .map(str::to_string)
                .ok_or_else(|| Error::Package(format!("<{}> without {name}", n.tag_name().name())))
        };
        let flag = |name: &str| n.attribute(name).is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let text = || n.text().unwrap_or("").trim().to_string();
        let path = |name: &str| attr(name).and_then(safe_path);
        let cmd = match n.tag_name().name() {
            "add" => Command::Add { source: attr("source")?, target: safe_path(text())? },
            "delete" => Command::Delete(safe_path(text())?),
            "archive" => {
                let kind = n.attribute("type").unwrap_or("RPF7");
                if !kind.eq_ignore_ascii_case("rpf7") {
                    return Err(Error::Package(format!("unsupported archive type {kind}")));
                }
                Command::Archive { path: path("path")?, create: flag("createIfNotExist"), body: parse_commands(n, src)? }
            }
            "defragmentation" => Command::Defragment(path("archive")?),
            "text" => Command::Text { path: path("path")?, create: flag("createIfNotExist"), ops: text_ops(n)? },
            "xml" => Command::Xml { path: path("path")?, ops: xml_ops(n, src)? },
            "pso" => Command::Pso { path: path("path")?, ops: xml_ops(n, src)?, source: src[n.range()].to_string() },
            other => return Err(Error::Package(format!("unknown command <{other}>"))),
        };
        out.push(cmd);
    }
    Ok(out)
}

/// Programs (the game's own, its launcher) are never replaced or deleted.
/// A text file's content, and whether it was not UTF-8: then it is read as
/// Latin-1, which keeps every byte, so the lines a `<text>` command leaves
/// alone are written back unchanged.
fn decode_text(bytes: Vec<u8>) -> (String, bool) {
    match String::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(e) => (e.into_bytes().iter().map(|&b| char::from(b)).collect(), true),
    }
}

fn is_exe(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// A target path from the package, refused if it could leave the game folder
/// or the archive, or names something Windows cannot (see [`RelPath`]).
fn safe_path(path: String) -> Result<String, Error> {
    RelPath::parse(&path)?;
    Ok(path)
}

fn text_ops(node: roxmltree::Node) -> Result<Vec<TextOp>, Error> {
    let mut ops = Vec::new();
    for n in node.children().filter(|n| n.is_element()) {
        let body = n.text().unwrap_or("").to_string();
        let line = || n.attribute("line").unwrap_or("").to_string();
        let condition = || {
            let c = n.attribute("condition").unwrap_or("Equal");
            Condition::parse(c).ok_or_else(|| Error::Package(format!("unknown text condition {c}")))
        };
        ops.push(match n.tag_name().name() {
            "add" => TextOp::Add(body),
            "insert" => TextOp::Insert {
                before: n.attribute("where").is_some_and(|w| w.eq_ignore_ascii_case("before")),
                line: line(),
                condition: condition()?,
                text: body,
            },
            "replace" => TextOp::Replace { line: line(), condition: condition()?, text: body },
            "delete" => TextOp::Delete { condition: condition()?, line: body },
            other => return Err(Error::Package(format!("unknown text command <{other}>"))),
        });
    }
    Ok(ops)
}

fn xml_ops(node: roxmltree::Node, src: &str) -> Result<Vec<XmlOp>, Error> {
    let mut ops = Vec::new();
    for n in node.children().filter(|n| n.is_element()) {
        let xpath = n
            .attribute("xpath")
            .ok_or_else(|| Error::Package("xml command without xpath".into()))?
            .to_string();
        // The new nodes, verbatim from assembly.xml.
        let content = match (n.first_child(), n.last_child()) {
            (Some(first), Some(last)) => src[first.range().start..last.range().end].trim().to_string(),
            _ => String::new(),
        };
        ops.push(match n.tag_name().name() {
            "add" => {
                let a = n.attribute("append").unwrap_or("Last");
                let append = Append::parse(a).ok_or_else(|| Error::Package(format!("unknown append mode {a}")))?;
                XmlOp::Add { xpath, append, content }
            }
            "replace" => XmlOp::Replace { xpath, content },
            "remove" => XmlOp::Remove { xpath },
            other => return Err(Error::Package(format!("unknown xml command <{other}>"))),
        });
    }
    Ok(ops)
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
}

fn content_path(source: &str) -> String {
    format!("content/{}", source.replace('\\', "/").trim_start_matches('/'))
}

/// The most one `<add>` may bring: into the game or mods folder it is streamed; into an archive
/// it is held (so less) unless it is spooled, and then the archive's own size field (32 bits) is the limit.
fn add_cap(limits: &Limits, in_archive: bool, target: &str) -> u64 {
    match (in_archive, spools(target)) {
        (false, _) => limits.streamed,
        (true, false) => limits.in_memory,
        (true, true) => limits.streamed.min(u64::from(u32::MAX)),
    }
}

/// Fails if a file of `size` bytes is over `cap`; says 4 GiB for what a 32-bit size field cannot hold.
fn within(what: &str, size: u64, cap: u64) -> Result<(), Error> {
    let shown = if cap == u64::from(u32::MAX) { 1 << 32 } else { cap };
    if size > cap { Err(Error::Limit { what: what.to_string(), limit: shown }) } else { Ok(()) }
}

/// What an `<add>` into an archive takes from a file instead of memory.
fn spools(target: &str) -> bool {
    target.rsplit(['/', '\\']).next().and_then(|n| n.rsplit_once('.')).is_some_and(|(_, e)| ["rpf", "awc", "bik"].iter().any(|k| e.eq_ignore_ascii_case(k)))
}

/// A converted file from its start.
fn rewound(file: &File) -> io::Result<&File> {
    let mut f = file;
    f.seek(SeekFrom::Start(0))?;
    Ok(f)
}

/// A zip entry name as entries are looked up: case and separators do not matter
/// (packages made on Windows use both).
fn normal(name: &str) -> String {
    name.replace('\\', "/").to_ascii_lowercase()
}

/// The package's zip, read within its limits: every read goes through the budget.
struct Entries {
    zip: ZipArchive<File>,
    /// Entry numbers by [`normal`] name; of two entries of one name, the first.
    index: HashMap<String, usize>,
    budget: Budget,
}

impl Entries {
    fn new(mut zip: ZipArchive<File>, limits: Limits) -> Result<Self, Error> {
        let count = zip.len();
        if count as u64 > limits.entries {
            return Err(Error::Package(format!("the package has {count} files; more than {} are refused", limits.entries)));
        }
        // What a file says it takes up must be room it has, or its ratio proves nothing.
        budget::zip_extents(&mut zip)?;
        let mut index = HashMap::with_capacity(count);
        for i in 0..count {
            if let Some(Ok(name)) = zip.name_for_index(i) {
                index.entry(normal(&name)).or_insert(i);
            }
        }
        Ok(Entries { zip, index, budget: Budget::new(limits) })
    }

    /// A new budget, for a new operation.
    fn fresh(&mut self) {
        self.budget = Budget::new(*self.budget.limits());
    }

    fn find(&self, name: &str) -> Result<usize, Error> {
        self.index.get(&normal(name)).copied().ok_or_else(|| Error::Package(format!("missing {name}")))
    }

    /// What the zip says an entry holds and takes (unpacked, packed), which is not yet what it holds.
    fn declared(&mut self, name: &str) -> Result<(u64, u64), Error> {
        let index = self.find(name)?;
        let entry = self.zip.by_index_raw(index).map_err(zip_err)?;
        Ok((entry.size(), entry.compressed_size()))
    }

    /// The first bytes of an entry.
    fn head(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        let index = self.find(name)?;
        let mut head = Vec::new();
        self.zip.by_index(index).map_err(zip_err)?.take(16).read_to_end(&mut head)?;
        Ok(head)
    }

    /// An entry into memory, at most `cap` bytes.
    fn read(&mut self, name: &str, cap: u64) -> Result<Vec<u8>, Error> {
        let index = self.find(name)?;
        let mut entry = self.zip.by_index(index).map_err(zip_err)?;
        let (declared, stored) = (entry.size(), entry.compressed_size());
        self.budget.read_packed(&mut entry, declared, stored, cap, name)
    }

    /// An entry written to `out`, a piece at a time.
    fn copy(&mut self, name: &str, out: &mut impl Write) -> Result<u64, Error> {
        let index = self.find(name)?;
        let mut entry = self.zip.by_index(index).map_err(zip_err)?;
        let (declared, stored) = (entry.size(), entry.compressed_size());
        self.budget.copy_packed(&mut entry, declared, stored, out, name)
    }
}

fn zip_err(e: zip::result::ZipError) -> Error {
    Error::Package(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_formats::hash::{joaat, joaat_lower};

    #[test]
    fn what_an_add_may_bring() {
        let limits = Limits::PACKAGE;
        // Streamed to a file: 8 GiB. Held inside an archive: 1 GiB. Spooled inside one: what its 32-bit size can say.
        assert_eq!(add_cap(&limits, false, "scripts/a.asi"), 8 << 30);
        assert_eq!(add_cap(&limits, false, "x64/a.rpf"), 8 << 30);
        assert_eq!(add_cap(&limits, true, "b.txt"), 1 << 30);
        for target in ["a.rpf", "dir\\a.RPF", "a.awc", "a.bik"] {
            assert_eq!(add_cap(&limits, true, target), u64::from(u32::MAX));
        }
        assert_eq!(add_cap(&Limits { streamed: 100, ..limits }, true, "a.rpf"), 100);
        // And the way it is told.
        assert_eq!(within("content/x", 5 << 30, add_cap(&limits, true, "a.rpf")).unwrap_err().to_string(), "content/x is larger than 4 GiB");
        assert_eq!(within("content/x", 2 << 30, add_cap(&limits, true, "b.txt")).unwrap_err().to_string(), "content/x is larger than 1 GiB");
        assert!(within("content/x", u64::from(u32::MAX), add_cap(&limits, true, "a.rpf")).is_ok());
    }

    /// A PSO file with `CPaletoTest { value: i32, model: hash }`.
    fn pso(value: i32, model: &str) -> Vec<u8> {
        let be = |v: u32| v.to_be_bytes();
        let ty = joaat(b"CPaletoTest");
        let mut psin = b"PSIN".to_vec();
        psin.extend_from_slice(&be(32));
        psin.resize(16, 0);
        psin.extend_from_slice(&be(value as u32));
        psin.extend_from_slice(&be(joaat_lower(model)));
        psin.resize(32, 0);
        let mut pmap = b"PMAP".to_vec();
        for w in [32, 1] {
            pmap.extend_from_slice(&be(w));
        }
        pmap.extend_from_slice(&[0, 1, 0x70, 0x70]);
        for w in [ty, 16, 0, 16] {
            pmap.extend_from_slice(&be(w));
        }
        let mut psch = b"PSCH".to_vec();
        for w in [12 + 8 + 12 + 24, 1, ty, 20, 2, 16, 0] {
            psch.extend_from_slice(&be(w));
        }
        for (name, ty, sub, offset) in [(joaat(b"value"), 5u8, 0u8, 0u16), (joaat(b"model"), 0x0B, 7, 4)] {
            psch.extend_from_slice(&be(name));
            psch.extend_from_slice(&[ty, sub]);
            psch.extend_from_slice(&offset.to_be_bytes());
            psch.extend_from_slice(&be(0));
        }
        [psin, pmap, psch].concat()
    }

    #[test]
    fn pso_command() {
        let src = r#"<content><pso path="data\test.pso">
            <replace xpath="/CPaletoTest/value"><value value="42" /></replace>
            <replace xpath="/CPaletoTest/model[text()='adder']"><model>zentorno</model></replace>
        </pso></content>"#;
        let doc = roxmltree::Document::parse(src).unwrap();
        let [Command::Pso { path, ops, source }] = &parse_commands(doc.root_element(), src).unwrap()[..] else { panic!() };
        assert_eq!(path, "data\\test.pso");
        let (out, warnings) = edit_meta(path, &pso(5, "adder"), ops, source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut expected = paleto_formats::MetaFile::parse(&pso(42, "zentorno")).unwrap();
        let mut got = paleto_formats::MetaFile::parse(&out).unwrap();
        for m in [&mut expected, &mut got] {
            m.learn_names("CPaletoTest value model zentorno");
        }
        assert_eq!(got.to_xml(), expected.to_xml());
    }

    #[test]
    fn refuses_paths_outside_the_game() {
        for cmd in [
            r#"<add source="x">..\..\evil.asi</add>"#,
            r#"<add source="x">C:\Windows\evil.dll</add>"#,
            r#"<delete>scripts/../../evil</delete>"#,
            r#"<archive path=".."><add source="x">a</add></archive>"#,
            r#"<archive path="x64e.rpf"><add source="x">..\a</add></archive>"#,
            r#"<text path="./"><add>x</add></text>"#,
            r#"<defragmentation archive="" />"#,
        ] {
            let src = format!("<content>{cmd}</content>");
            let doc = roxmltree::Document::parse(&src).unwrap();
            let err = parse_commands(doc.root_element(), &src).unwrap_err();
            assert!(err.to_string().contains("unsafe path"), "{cmd}: {err}");
        }
        let src = r#"<content><add source="x">\scripts\.\a..b.asi</add></content>"#;
        let doc = roxmltree::Document::parse(src).unwrap();
        assert!(parse_commands(doc.root_element(), src).is_ok());
    }

    #[test]
    fn programs_are_left_alone() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content>
            <add source="x">gta5.EXE</add>
            <add source="x">PlayGTAV.exe</add>
            <add source="x">dinput8.dll</add>
            <delete>GTA5.exe</delete>
        </content></package>"#;
        let oiv = dir.path().join("x.oiv");
        let mut zip = zip::ZipWriter::new(File::create(&oiv).unwrap());
        for (name, data) in [("assembly.xml", assembly.as_bytes()), ("content/x", b"new")] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();

        let log = Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap();
        assert_eq!(std::fs::read(root.join("GTA5.exe")).unwrap(), b"game");
        assert!(!root.join("PlayGTAV.exe").exists());
        assert_eq!(std::fs::read(root.join("dinput8.dll")).unwrap(), b"new");
        assert_eq!(log.iter().filter(|l| l.starts_with("warning: refusing")).count(), 3, "{log:?}");
    }

    /// A package with these commands and `content/` files.
    fn package(dir: &Path, commands: &str, files: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;
        let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content>{commands}</content></package>"#);
        let oiv = dir.join("x.oiv");
        let mut zip = zip::ZipWriter::new(File::create(&oiv).unwrap());
        for (name, data) in [("assembly.xml", assembly.as_bytes())].into_iter().chain(files.iter().copied()) {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        oiv
    }

    #[test]
    fn a_failed_install_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::create_dir_all(root.join("update")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        std::fs::write(root.join("scripts/old.asi"), b"old").unwrap();
        std::fs::write(root.join("scripts/gone.asi"), b"gone").unwrap();
        std::fs::write(root.join("update/list.txt"), b"a\n").unwrap();
        let mut original = ArchiveEditor::new("update.rpf");
        original.add_file("a.txt", b"a".to_vec()).unwrap();
        original.save(&root.join("update/update.rpf")).unwrap();
        // Everything works until the last command, whose file is missing.
        let oiv = package(dir.path(), r#"
            <archive path="update\update.rpf"><add source="x">b.txt</add></archive>
            <add source="x">scripts\old.asi</add>
            <add source="x">scripts\new.asi</add>
            <delete>scripts\gone.asi</delete>
            <text path="update\list.txt"><add>b</add></text>
            <xml path="update\missing.xml"><remove xpath="/a" /></xml>"#, &[("content/x", b"new")]);

        let err = Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap_err();
        assert!(err.to_string().contains("nothing was installed"), "{err}");
        assert_eq!(std::fs::read(root.join("scripts/old.asi")).unwrap(), b"old");
        assert_eq!(std::fs::read(root.join("scripts/gone.asi")).unwrap(), b"gone");
        assert!(!root.join("scripts/new.asi").exists());
        assert!(!root.join("mods/update/list.txt").exists() && !root.join("mods/update/update.rpf").exists());
        assert_eq!(std::fs::read(root.join("update/list.txt")).unwrap(), b"a\n");
        let left: Vec<_> = std::fs::read_dir(root.join("scripts")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn text_edits_keep_the_files_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        // Latin-1, as Windows tools often write it: not UTF-8.
        std::fs::write(root.join("scripts/settings.ini"), b"name=caf\xE9\r\n").unwrap();
        let oiv = package(dir.path(), r#"<text path="scripts\settings.ini"><add>speed=1 €</add></text>"#, &[]);
        let log = Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap();
        assert_eq!(std::fs::read(root.join("scripts/settings.ini")).unwrap(), b"name=caf\xE9\r\nspeed=1 ?\r\n");
        assert!(log.iter().any(|l| l.starts_with("warning:") && l.contains("written as ?")), "{log:?}");
    }

    #[test]
    fn a_new_archive_never_goes_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/x64")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        // A link left in the mods folder, pointing at nothing (yet).
        let outside = dir.path().join("outside.rpf");
        std::os::unix::fs::symlink(&outside, root.join("mods/x64/new.rpf")).unwrap();
        let oiv = package(dir.path(), r#"<archive path="x64\new.rpf" createIfNotExist="True"><add source="x">a.txt</add></archive>"#, &[("content/x", b"a")]);
        Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap();
        assert!(!outside.exists());
        let made = root.join("mods/x64/new.rpf");
        assert!(std::fs::symlink_metadata(&made).unwrap().is_file());
        assert_eq!(ArchiveEditor::open(&made, None).unwrap().read("a.txt", None).unwrap(), b"a");
    }

    #[test]
    fn content_names_ignore_case_and_separators() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        let assembly = r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content>
            <add source="scripts/a.asi">scripts\a.asi</add>
        </content></package>"#;
        let oiv = dir.path().join("x.oiv");
        let mut zip = zip::ZipWriter::new(File::create(&oiv).unwrap());
        for (name, data) in [("assembly.xml", assembly.as_bytes()), ("Content\\Scripts\\A.asi", b"asi")] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();

        Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap();
        assert_eq!(std::fs::read(root.join("scripts/a.asi")).unwrap(), b"asi");
    }

    #[test]
    fn converts_legacy_content() {
        use std::io::Write;
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let mut original = ArchiveEditor::new("x64e.rpf");
        original.add_file("props/x.txt", b"x".to_vec()).unwrap();
        original.save(&root.join("x64e.rpf")).unwrap();

        // A vehicle pack with a Legacy model inside a nested archive, and a loose Legacy texture.
        let mut pack = ArchiveEditor::new("dlc.rpf");
        let vehicles = pack.archive("x64/vehicles.rpf", true, None).unwrap();
        vehicles.add_file("car.yft", res(162)).unwrap();
        vehicles.add_file("car_hi.yft", res(171)).unwrap();
        pack.save(&dir.path().join("dlc.rpf")).unwrap();
        let assembly = r#"<package version="2.2" target="Five"><metadata><name>Car</name></metadata><content>
            <add source="dlc.rpf">update\x64\dlcpacks\car\dlc.rpf</add>
            <archive path="x64e.rpf" createIfNotExist="False" type="RPF7"><add source="props\car.yft">props\car.yft</add></archive>
        </content></package>"#;
        let oiv = dir.path().join("car.oiv");
        let mut zip = zip::ZipWriter::new(File::create(&oiv).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in [
            ("assembly.xml", assembly.as_bytes().to_vec()),
            ("content/dlc.rpf", std::fs::read(dir.path().join("dlc.rpf")).unwrap()),
            ("content/props/car.yft", res(162)),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&data).unwrap();
        }
        zip.finish().unwrap();

        let game = Game::open(&root).unwrap();
        let mut pkg = Package::open(&oiv).unwrap();
        assert_eq!(pkg.legacy_files(None).unwrap(), ["dlc.rpf/x64/vehicles.rpf/car.yft", "props/car.yft"]);
        let tool = crate::convert::tests::fake_tool(dir.path());
        let log = pkg.convert_legacy(&tool, None).unwrap();
        assert_eq!(log, ["converted dlc.rpf/x64/vehicles.rpf/car.yft", "converted props/car.yft"]);
        pkg.install_with(&game, None).unwrap();

        let mut installed = ArchiveEditor::open(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), None).unwrap();
        let vehicles = installed.archive("x64/vehicles.rpf", false, None).unwrap();
        assert_eq!(vehicles.read("car.yft", None).unwrap(), res(171));
        assert_eq!(vehicles.read("car_hi.yft", None).unwrap(), res(171));
        let x64e = ArchiveEditor::open(&root.join("mods/x64e.rpf"), None).unwrap();
        assert_eq!(x64e.read("props/car.yft", None).unwrap(), res(171));
    }
}
