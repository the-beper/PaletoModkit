//! OpenIV packages (`.oiv`, format 2.x): a zip with `assembly.xml`, an
//! optional `icon.png` and a `content/` folder.
//!
//! Installation always targets the mods folder for game data; the original
//! game files are never modified.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use paleto_rpf::{ArchiveEditor, Keys};
use zip::ZipArchive;

use crate::budget::{self, Budget, Limits, Scratch};
use crate::convert;
use crate::fsroot::{Dir, GameRoots, Resolved, RootKind};
use crate::game::Game;
use crate::relpath::RelPath;
use crate::txn::{self, Staged, Txn, TxnFailure};
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

    /// Installs the package as one transaction: every command is planned first (later commands see what
    /// earlier ones did), then each changed file is staged next to its name and the lot is committed and
    /// journaled. Game data goes to the mods folder (a changed original is staged there as a new file, the
    /// original is only read); other files go to the game folder. Returns a log of what was done, including
    /// warnings. Not atomic: several files are put in place one after the other, and an error or a crash
    /// in between is undone (at once, or by recovery at the next change), see [`crate::txn`].
    pub fn install(&mut self, game: &Game, keys: &Keys) -> Result<Vec<String>, Error> {
        self.install_with(game, Some(keys))
    }

    /// [`Package::install`], with keys only needed for encrypted archives.
    pub(crate) fn install_with(&mut self, game: &Game, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
        self.check(&self.content.clone(), false)?;
        self.entries.fresh();
        let roots = game.roots()?;
        // One change at a time per game, for the whole install; an interrupted one is dealt with first.
        let mut txn = Txn::begin(&roots, "installing a package")?;
        let strays = Strays::default();
        let mut inst = Installer { roots: &roots, keys, entries: &mut self.entries, converted: &self.converted, plan: Plan::default(), log: txn.recovered().to_vec(), strays: strays.clone() };
        if let Err(e) = self.content.clone().iter().try_for_each(|cmd| inst.run_root(cmd)) {
            // Nothing is on disk yet but the journal: the abort ends it and says so.
            let failure = txn.abort(e, inst.log);
            return Err(with_strays(failure, &strays.borrow()));
        }
        let saved = match inst.stage_all(&mut txn) {
            Ok(saved) => saved,
            Err(e) => return Err(with_strays(txn::failure(e, inst.log, "installed"), &strays.borrow())),
        };
        let mut log = std::mem::take(&mut inst.log);
        // Every archive, and every nested archive the install edited, must still open from its new file before
        // anything is put in place (their tables of contents are read, not the files).
        let committed = txn.commit_validated(Vec::new(), |staged, file| {
            if let Some(archive) = saved.iter().find(|s| s.staged == staged) {
                #[cfg(test)]
                if let Some(hook) = BEFORE_OPENING.with(|h| h.borrow_mut().take()) {
                    hook();
                }
                let mut top = ArchiveEditor::open_file(file.try_clone()?, &archive.name, keys)?;
                for chain in &archive.nested {
                    chain.iter().try_fold(&mut top, |editor, name| editor.archive(name, false, keys))?;
                }
            }
            Ok(())
        });
        match committed {
            Ok(notes) => {
                log.extend(notes);
                log.extend(saved.iter().map(|s| format!("saved {}", s.shown)));
                log.extend(strays.borrow().iter().map(|s| format!("warning: {s}")));
                Ok(log)
            }
            Err(e) => Err(with_strays(txn::failure(e, log, "installed"), &strays.borrow())),
        }
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

/// A file the install changes: the root it lands in and its lower-cased logical path (links resolved), so that
/// every spelling of one name is one key.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key(bool, String);

impl Key {
    fn of(dest: &Resolved) -> Self {
        Key(dest.root == RootKind::Mods, dest.relative().join("/").to_ascii_lowercase())
    }
}

/// What the install will leave at a name; nothing is written until every command has run.
enum Pending {
    /// An `<add>`: the package's file, and its size, written into the new file when it is staged.
    Source(String, u64),
    Bytes(Vec<u8>),
    /// A top-level archive being edited.
    Archive(Box<Open>),
    Deleted,
}

/// A top-level archive in memory (its nested archives are inside it), saved into one new file at the end.
struct Open {
    editor: ArchiveEditor,
    work: Work,
}

/// What the commands on an archive need besides the archive itself.
struct Work {
    spool: Spool,
    /// The size of the archive it was opened from, and what the commands have added since (what they bring, and
    /// twice each nested archive they edit: it is written whole to a spool and then appended): what its new file may take.
    base: u64,
    added: u64,
    /// The file it was opened from, to read the size of its nested archives from (none for a new archive).
    file: Option<File>,
    /// The nested archives the commands edit, as the names to open them by from the top one (case folded).
    nested: Vec<Vec<String>>,
}

impl Work {
    fn new(spool: Spool, base: u64, file: Option<File>) -> Self {
        Work { spool, base, added: 0, file, nested: Vec::new() }
    }

    /// A command edits the nested archive `chain` (names from the top archive down): its new file takes up to
    /// twice its size more, a whole copy in a spool and the same again appended. One that is not in the archive the
    /// install opened (made by the package) is only what was added; one whose size cannot be read counts as the top archive.
    fn nested(&mut self, chain: &[String], keys: Option<&Keys>) {
        let folded: Vec<String> = chain.iter().map(|p| p.replace('\\', "/").to_ascii_lowercase()).collect();
        if self.nested.contains(&folded) {
            return;
        }
        let size = self.file.as_ref().map_or(0, |f| nested_len(f, &folded, keys).unwrap_or(self.base));
        self.added = self.added.saturating_add(size.saturating_mul(2));
        self.nested.push(folded);
    }
}

/// The stored size of the nested archive at `chain` in the archive `file`: 0 if there is none. The table of
/// contents is read again through `/proc` (the file is only opened, not changed); `None` if that cannot be done.
fn nested_len(file: &File, chain: &[String], keys: Option<&Keys>) -> Option<u64> {
    use std::os::fd::AsRawFd;
    let mut archive = paleto_rpf::Archive::open(Path::new(&format!("/proc/self/fd/{}", file.as_raw_fd())), keys).ok()?;
    let mut size = 0;
    for (i, part) in chain.iter().enumerate() {
        let Some(idx) = archive.find(part) else { return Some(0) };
        size = archive.entries[idx].stored_len();
        if i + 1 < chain.len() {
            archive = archive.open_child(idx, keys).ok()?;
        }
    }
    Some(size)
}

struct Slot {
    dest: Resolved,
    rel: RelPath,
    pending: Pending,
}

/// The install as a plan: what each name will hold, in the order the names were first touched. Later commands
/// see what earlier ones planned; the engine changes each file once.
#[derive(Default)]
struct Plan {
    order: Vec<Key>,
    slots: HashMap<Key, Slot>,
}

impl Plan {
    /// Fails if `key` would be a file and a folder at once: another name of the plan is a file at a folder of it,
    /// or the other way round (a path below a file the package deletes or writes).
    fn check_room(&self, key: &Key) -> Result<(), Error> {
        for (other, slot) in self.slots.iter().filter(|(o, _)| o.0 == key.0) {
            let (file, below) = if key.1.starts_with(&format!("{}/", other.1)) {
                (&other.1, &key.1)
            } else if other.1.starts_with(&format!("{}/", key.1)) {
                (&key.1, &other.1)
            } else {
                continue;
            };
            let verb = if matches!(slot.pending, Pending::Deleted) && file == &other.1 { "deletes the file" } else { "writes the file" };
            return Err(Error::Package(format!("the package {verb} {file} and also changes {below}, which would be below it")));
        }
        Ok(())
    }
}

/// Where the spools of an archive go: its destination's volume, not the temporary folder (often memory).
struct Spool {
    /// The deepest folder that exists on the way to the destination (held: it cannot be redirected).
    dir: Dir,
    /// Where it is, for messages.
    path: PathBuf,
    /// Spool names that could not be removed (see [`Spool::file`]), shared by every spool of the install.
    strays: Strays,
}

type Strays = Rc<RefCell<Vec<String>>>;

// Runs once, in the check of the first staged archive (tests damage the staged file there).
#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_OPENING: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Tests make the filesystem look as if it had no unnamed files.
    static NO_UNNAMED_FILES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Spool {
    fn of(dest: &Resolved, strays: &Strays) -> Result<Self, Error> {
        let mut path = dest.path();
        for _ in 0..=dest.missing.len() {
            path.pop();
        }
        Ok(Spool { dir: dest.parent.try_clone()?, path, strays: strays.clone() })
    }

    /// A file that has no name at all (so a crash leaves nothing): `O_TMPFILE` in the folder, opened through the
    /// held handle. Where the filesystem has none, a file made exclusively under a private name and unlinked at once;
    /// if that name cannot be removed it stays, which is noted in `strays` (it is no part of any journal) and is an error.
    fn file(&self) -> io::Result<File> {
        #[cfg(test)]
        let unnamed = if NO_UNNAMED_FILES.with(std::cell::Cell::get) { Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)) } else { self.dir.tmpfile() };
        #[cfg(not(test))]
        let unnamed = self.dir.tmpfile();
        match unnamed {
            Err(e) if Dir::unnamed_unsupported(&e) => {
                let (file, name) = self.dir.create_unique()?;
                if let Err(e) = self.dir.unlink(&name) {
                    let shown = self.path.join(&name).display().to_string();
                    self.strays.borrow_mut().push(format!("{shown}: a temporary file of the install that could not be removed ({e}); it can be deleted"));
                    return Err(e);
                }
                Ok(file)
            }
            result => result,
        }
    }
}

/// [`Txn::stage_sized`] for a `write` that fails with an [`Error`] of its own: that error, not the I/O error it
/// had to be turned into for the engine, is the cause of what comes back.
fn stage_with(txn: &mut Txn, dest: &mut Resolved, need: u64, write: impl FnOnce(&mut File) -> Result<(), Error>) -> Result<Staged, Error> {
    let mut failed = None;
    let staged = txn.stage_sized(dest, need, |f| write(f).map_err(|e| io::Error::other(failed.insert(e).to_string())));
    staged.map_err(|e| match (failed, e) {
        (Some(cause), Error::Txn(mut f)) => {
            f.cause = cause;
            Error::Txn(f)
        }
        (Some(cause), _) => cause,
        (None, e) => e,
    })
}

/// `failure` with the spool files the install could not remove: they are no part of its journal, so what is
/// left is not clean and the message must not say "nothing was installed".
fn with_strays(failure: Error, strays: &[String]) -> Error {
    if strays.is_empty() {
        return failure;
    }
    match failure {
        Error::Txn(mut f) => {
            f.undone = false;
            f.journaled = false;
            f.failures.extend(strays.iter().cloned());
            Error::Txn(f)
        }
        cause => Error::Txn(Box::new(TxnFailure { cause, log: Vec::new(), undone: false, failures: strays.to_vec(), journaled: false, verb: "installed" })),
    }
}

/// A top-level archive staged by [`Installer::stage_all`], to be opened again before the commit.
struct Saved {
    staged: Staged,
    name: String,
    shown: String,
    /// The nested archives the install edited in it.
    nested: Vec<Vec<String>>,
}

struct Installer<'a> {
    /// Where files may be written: every destination is resolved through it.
    roots: &'a GameRoots,
    keys: Option<&'a Keys>,
    entries: &'a mut Entries,
    converted: &'a HashMap<String, Converted>,
    plan: Plan,
    log: Vec<String>,
    /// Spool files that could not be removed.
    strays: Strays,
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

    /// How many bytes an `<add>` source will be (the converted file if there is one).
    fn source_len(&mut self, source: &str) -> Result<u64, Error> {
        let entry = content_path(source);
        match self.converted.get(&entry.to_ascii_lowercase()) {
            Some(Converted::Bytes(data)) => Ok(data.len() as u64),
            Some(Converted::File(_, len)) => Ok(*len),
            None => Ok(self.entries.declared(&entry)?.0),
        }
    }

    /// An `<add>` source written to `out`, a piece at a time.
    fn write_source(&mut self, source: &str, out: &mut impl Write) -> Result<(), Error> {
        let entry = content_path(source);
        match self.converted.get(&entry.to_ascii_lowercase()) {
            Some(Converted::Bytes(data)) => self.entries.budget.copy(&mut data.as_slice(), data.len() as u64, out, &entry)?,
            Some(Converted::File(file, len)) => self.entries.budget.copy(&mut rewound(file)?, *len, out, &entry)?,
            None => self.entries.copy(&entry, out)?,
        };
        Ok(())
    }

    /// An `<add>` source as a file, and its length: for the archives an archive takes from disk. It is spooled
    /// beside the archive being edited (counted by the budget as any read is), never held.
    fn source_file(&mut self, source: &str, spool: &Spool) -> Result<(File, u64), Error> {
        if let Some(Converted::File(file, len)) = self.converted.get(&content_path(source).to_ascii_lowercase()) {
            return Ok((file.try_clone()?, *len));
        }
        let mut file = spool.file()?;
        self.write_source(source, &mut file)?;
        let len = file.stream_position()?;
        Ok((file, len))
    }

    /// Plans a new content for a name: the slot it has (which keeps its destination) or a new one, after the
    /// checks no write may fail.
    fn put(&mut self, dest: Resolved, rel: RelPath, pending: Pending) -> Result<(), Error> {
        let key = Key::of(&dest);
        if let Some(slot) = self.plan.slots.get_mut(&key) {
            slot.pending = pending;
            return Ok(());
        }
        self.register(&key, &dest)?;
        self.plan.slots.insert(key, Slot { dest, rel, pending });
        Ok(())
    }

    /// A name the plan did not have before: it must pass what any write passes (no programs, no game data in the
    /// game folder), and not clash with a file or folder of another name.
    fn register(&mut self, key: &Key, dest: &Resolved) -> Result<(), Error> {
        self.roots.check_write(dest)?;
        self.plan.check_room(key)?;
        self.plan.order.push(key.clone());
        Ok(())
    }

    /// What there is on disk for a name: the copy in the mods folder, else (for game data routed there, `original`
    /// being its game-relative path) the original, read-only. Never a copy made on the way: the transaction, which
    /// holds the game lock, makes the new file when it commits.
    fn base(&self, dest: &Resolved, original: Option<&RelPath>) -> Result<Option<File>, Error> {
        if let Some(file) = self.roots.read_dest(dest)? {
            return Ok(Some(file));
        }
        match original {
            Some(rel) if dest.root == RootKind::Mods => self.roots.original(rel),
            _ => Ok(None),
        }
    }

    /// A command at the game folder level.
    fn run_root(&mut self, cmd: &Command) -> Result<(), Error> {
        match cmd {
            Command::Add { source, target } => {
                let rel = RelPath::parse(target)?;
                let dest = self.roots.dest(&rel)?;
                if let Some(why) = self.refused(&dest) {
                    self.log.push(format!("warning: refusing to replace {}{why}", dest.path().display()));
                    return Ok(());
                }
                let (path, len) = (dest.path(), self.source_len(source)?);
                // Whatever the plan held for the name (an archive being edited, say) is replaced.
                self.put(dest, rel, Pending::Source(source.clone(), len))?;
                self.log.push(format!("add {}", path.display()));
            }
            Command::Delete(target) => {
                // Game data is only ever deleted from the mods folder.
                let rel = RelPath::parse(target)?;
                let dest = self.roots.dest(&rel)?;
                let path = dest.path();
                if let Some(why) = self.refused(&dest) {
                    self.log.push(format!("warning: refusing to delete {}{why}", path.display()));
                    return Ok(());
                }
                let key = Key::of(&dest);
                // (deleted already, a file there before the plan)
                match self.plan.slots.get(&key).map(|s| (matches!(s.pending, Pending::Deleted), s.dest.existing.is_some())) {
                    Some((true, _)) => return Ok(()),
                    // What the plan made is gone again, and there was nothing before it.
                    Some((false, false)) => {
                        self.plan.slots.remove(&key);
                        self.plan.order.retain(|k| *k != key);
                    }
                    Some((false, true)) => self.put(dest, rel, Pending::Deleted)?,
                    None if self.roots.read_dest(&dest)?.is_some() => self.put(dest, rel, Pending::Deleted)?,
                    None => return Ok(()),
                }
                self.log.push(format!("delete {}", path.display()));
            }
            Command::Archive { path, create, body } => {
                let (key, dest, rel, mut open) = self.take_archive(path, *create)?;
                body.iter().try_for_each(|c| self.run_in(&mut open.editor, &mut open.work, &[], c, path))?;
                self.plan.slots.insert(key, Slot { dest, rel, pending: Pending::Archive(Box::new(open)) });
            }
            Command::Text { path, create, ops } => {
                let Some((dest, rel)) = self.loose_file(path)? else { return Ok(()) };
                let (text, latin1) = match self.read_loose(&dest, &rel, path)? {
                    Some(b) => decode_text(b),
                    None if *create => (String::new(), false),
                    None => return Err(Error::Package(format!("{path} not found"))),
                };
                let out = self.encode_text(path, &text::apply(&text, ops), latin1);
                self.write_loose(dest, rel, out)?;
            }
            Command::Xml { path, ops } => {
                let Some((dest, rel)) = self.loose_file(path)? else { return Ok(()) };
                let data = self.read_loose(&dest, &rel, path)?.ok_or_else(|| Error::Package(format!("{path} not found")))?;
                let (out, warnings) = xml::apply(&data, ops)?;
                self.warn(path, warnings);
                self.write_loose(dest, rel, out)?;
            }
            Command::Defragment(path) => {
                let (key, dest, rel, mut open) = self.take_archive(path, false)?;
                open.editor.defragment();
                self.plan.slots.insert(key, Slot { dest, rel, pending: Pending::Archive(Box::new(open)) });
            }
            Command::Pso { path, ops, source } => {
                let Some((dest, rel)) = self.loose_file(path)? else { return Ok(()) };
                let data = self.read_loose(&dest, &rel, path)?.ok_or_else(|| Error::Package(format!("{path} not found")))?;
                let out = self.edit_meta(path, &data, ops, source)?;
                self.write_loose(dest, rel, out)?;
            }
        }
        Ok(())
    }

    fn edit_meta(&mut self, path: &str, data: &[u8], ops: &[XmlOp], source: &str) -> Result<Vec<u8>, Error> {
        let (out, warnings) = edit_meta(path, data, ops, source)?;
        self.warn(path, warnings);
        Ok(out)
    }

    /// A command inside an archive; `at` is for messages. `work` has the spool where files taken from disk wait
    /// and counts the bytes the commands bring; `chain` is the names of the nested archives `editor` is inside.
    fn run_in(&mut self, editor: &mut ArchiveEditor, work: &mut Work, chain: &[String], cmd: &Command, at: &str) -> Result<(), Error> {
        let keys = self.keys;
        match cmd {
            Command::Add { source, target } => {
                if spools(target) {
                    // An archive, sound or video is attached from a file, not held.
                    let (file, len) = self.source_file(source, &work.spool)?;
                    editor.add_file_from(target, file, len)?;
                    work.added = work.added.saturating_add(len);
                } else {
                    let data = self.source(source)?;
                    work.added = work.added.saturating_add(data.len() as u64);
                    editor.add_file(target, data)?;
                }
                self.log.push(format!("add {at}\\{target}"));
            }
            Command::Delete(target) => {
                if editor.delete(target)? {
                    self.log.push(format!("delete {at}\\{target}"));
                }
            }
            Command::Archive { path, create, body } => {
                let chain = [chain, std::slice::from_ref(path)].concat();
                work.nested(&chain, keys);
                let inner = editor.archive(path, *create, keys)?;
                let at = format!("{at}\\{path}");
                body.iter().try_for_each(|c| self.run_in(inner, work, &chain, c, &at))?;
            }
            Command::Text { path, create, ops } => {
                let (text, latin1) = match editor.read(path, keys) {
                    Ok(b) => decode_text(b),
                    Err(paleto_rpf::Error::NotFound(_)) if *create => (String::new(), false),
                    Err(e) => return Err(e.into()),
                };
                let out = self.encode_text(&format!("{at}\\{path}"), &text::apply(&text, ops), latin1);
                work.added = work.added.saturating_add(out.len() as u64);
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}\\{path}"));
            }
            Command::Xml { path, ops } => {
                let (out, warnings) = xml::apply(&editor.read(path, keys)?, ops)?;
                self.warn(&format!("{at}\\{path}"), warnings);
                work.added = work.added.saturating_add(out.len() as u64);
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}\\{path}"));
            }
            Command::Defragment(path) => {
                work.nested(&[chain, std::slice::from_ref(path)].concat(), keys);
                editor.archive(path, false, keys)?.defragment();
            }
            Command::Pso { path, ops, source } => {
                let at = format!("{at}\\{path}");
                let out = self.edit_meta(&at, &editor.read(path, keys)?, ops, source)?;
                work.added = work.added.saturating_add(out.len() as u64);
                editor.add_file(path, out)?;
                self.log.push(format!("edit {at}"));
            }
        }
        Ok(())
    }

    /// Takes a top-level archive out of the plan to edit it (put it back under the same key): the one an earlier
    /// command planned, an added `.rpf` opened from its source, else the mods copy, else the original, read
    /// through its handle and never written; with `create`, a new one.
    fn take_archive(&mut self, path: &str, create: bool) -> Result<(Key, Resolved, RelPath, Open), Error> {
        let rel = RelPath::parse(path)?;
        let dest = self.roots.dest(&rel)?;
        let key = Key::of(&dest);
        if let Some(Slot { dest, rel, pending }) = self.plan.slots.remove(&key) {
            let spool = Spool::of(&dest, &self.strays)?;
            let open = match pending {
                Pending::Archive(open) => *open,
                Pending::Source(source, _) => {
                    let (file, base) = self.source_file(&source, &spool)?;
                    let work = Work::new(spool, base, Some(file.try_clone()?));
                    Open { editor: ArchiveEditor::open_file(file, &dest.name, self.keys)?, work }
                }
                Pending::Bytes(data) => {
                    let mut file = spool.file()?;
                    file.write_all(&data)?;
                    let work = Work::new(spool, data.len() as u64, Some(file.try_clone()?));
                    Open { editor: ArchiveEditor::open_file(file, &dest.name, self.keys)?, work }
                }
                Pending::Deleted if create => Open { editor: ArchiveEditor::new(&dest.name), work: Work::new(spool, 0, None) },
                Pending::Deleted => return Err(Error::Package(format!("archive {path} not found"))),
            };
            return Ok((key, dest, rel, open));
        }
        self.register(&key, &dest)?;
        let spool = Spool::of(&dest, &self.strays)?;
        let open = match self.base(&dest, Some(&rel.without_mods()))? {
            Some(file) => {
                let work = Work::new(spool, file.metadata()?.len(), Some(file.try_clone()?));
                Open { editor: ArchiveEditor::open_file(file, &dest.name, self.keys)?, work }
            }
            None if create => Open { editor: ArchiveEditor::new(&dest.name), work: Work::new(spool, 0, None) },
            None => return Err(Error::Package(format!("archive {path} not found"))),
        };
        Ok((key, dest, rel, open))
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

    /// A loose file to edit. `None` (after a warning) for one the package may not change.
    fn loose_file(&mut self, path: &str) -> Result<Option<(Resolved, RelPath)>, Error> {
        let rel = RelPath::parse(path)?;
        let dest = self.roots.dest(&rel)?;
        if self.roots.is_game_file(&dest) {
            self.log.push(format!("warning: refusing to replace {}{}", dest.path().display(), self.refused(&dest).unwrap_or_default()));
            return Ok(None);
        }
        Ok(Some((dest, rel)))
    }

    /// What a loose file holds at this point of the plan; `None` if there is no file. An earlier command's
    /// result comes first, then the mods copy, then (game data is edited in a copy in the mods folder) the original.
    fn read_loose(&mut self, dest: &Resolved, rel: &RelPath, path: &str) -> Result<Option<Vec<u8>>, Error> {
        match self.plan.slots.get(&Key::of(dest)).map(|s| &s.pending) {
            Some(Pending::Bytes(data)) => return Ok(Some(data.clone())),
            Some(Pending::Source(source, _)) => {
                let source = source.clone();
                return Ok(Some(self.source(&source)?));
            }
            Some(Pending::Deleted) => return Ok(None),
            Some(Pending::Archive(_)) => return Err(Error::Package(format!("{path} is an archive the package edits, not a text file"))),
            None => {}
        }
        let mut data = Vec::new();
        match self.base(dest, (!rel.in_mods_folder()).then_some(rel))? {
            Some(mut file) => {
                file.read_to_end(&mut data)?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// Plans a loose file's new content: the engine writes it as a new file that replaces the old one.
    fn write_loose(&mut self, dest: Resolved, rel: RelPath, data: Vec<u8>) -> Result<(), Error> {
        let path = dest.path();
        self.put(dest, rel, Pending::Bytes(data))?;
        self.log.push(format!("edit {}", path.display()));
        Ok(())
    }

    /// Stages every planned file in the order the names were first touched (which is the order of the commit):
    /// loose files and archives into new files next to their names, deletions as planned removals. Returns the
    /// archives, which the commit opens again first. Every error is the engine's, from a call that ended the transaction.
    fn stage_all(&mut self, txn: &mut Txn) -> Result<Vec<Saved>, Error> {
        let mut saved = Vec::new();
        for key in std::mem::take(&mut self.plan.order) {
            // A name that was planned and then deleted again is not there any more; one that came twice is done.
            let Some(Slot { mut dest, pending, .. }) = self.plan.slots.remove(&key) else { continue };
            match pending {
                Pending::Deleted => txn.delete(&dest)?,
                Pending::Bytes(data) => {
                    let staged = stage_with(txn, &mut dest, data.len() as u64, |f| Ok(f.write_all(&data)?))?;
                    txn.release(staged);
                }
                Pending::Source(source, len) => {
                    let staged = stage_with(txn, &mut dest, len, |f| self.write_source(&source, f))?;
                    txn.release(staged);
                }
                Pending::Archive(open) => {
                    let Open { editor, work } = *open;
                    let Work { spool, base, added, nested, .. } = work;
                    let staged = stage_with(txn, &mut dest, base.saturating_add(added), |f| editor.save_to(f, &mut || spool.file()).map(drop).map_err(Error::from))?;
                    txn.release(staged);
                    saved.push(Saved { staged, name: dest.name.clone(), shown: dest.path().display().to_string(), nested });
                }
            }
        }
        Ok(saved)
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

    #[test]
    fn a_spool_has_no_name_and_lives_in_the_folder_of_its_destination() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("mods/x64")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        let roots = Game::open(&root).unwrap().roots().unwrap();
        // The folders `new` and `deeper` are still to be made: the spool goes where the folders end.
        let dest = roots.dest(&RelPath::parse(r"x64\new\deeper\f.rpf").unwrap()).unwrap();
        let spool = Spool::of(&dest, &Strays::default()).unwrap();
        assert_eq!(spool.path, root.join("mods/x64"));
        for _ in 0..2 {
            let mut file = spool.file().unwrap();
            file.write_all(b"spooled").unwrap();
            assert_eq!(file.metadata().unwrap().nlink(), 0, "the spool has a name");
            let shown = std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&file))).unwrap();
            assert!(shown.starts_with(root.join("mods/x64")), "{shown:?}: not in the folder of the destination");
        }
        assert_eq!(std::fs::read_dir(root.join("mods/x64")).unwrap().count(), 0);
    }

    #[test]
    fn the_room_for_an_archive_counts_twice_each_nested_archive_the_commands_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mut outer = ArchiveEditor::new("o.rpf");
        outer.add_file("o.txt", vec![1; 3000]).unwrap();
        let inner = outer.archive("inner.rpf", true, None).unwrap();
        inner.add_file("i.txt", vec![2; 5000]).unwrap();
        inner.archive("deep.rpf", true, None).unwrap().add_file("d.txt", vec![3; 7000]).unwrap();
        let path = dir.path().join("o.rpf");
        outer.save(&path).unwrap();
        let top = paleto_rpf::Archive::open(&path, None).unwrap();
        let inner_len = top.entries[top.find("inner.rpf").unwrap()].stored_len();
        let deep_len = paleto_rpf::Archive::open(&path, None).unwrap().open_child(top.find("inner.rpf").unwrap(), None).unwrap().entries.iter().find(|e| e.name == "deep.rpf").unwrap().stored_len();
        assert!(inner_len > deep_len && deep_len > 0);
        let strays = Strays::default();
        let spool = |dir: &Path| {
            std::fs::create_dir_all(dir.join("game")).unwrap();
            std::fs::write(dir.join("game/GTA5.exe"), b"game").unwrap();
            Spool::of(&Game::open(&dir.join("game")).unwrap().roots().unwrap().dest(&RelPath::parse("scripts/x.rpf").unwrap()).unwrap(), &strays).unwrap()
        };
        let mut work = Work::new(spool(dir.path()), std::fs::metadata(&path).unwrap().len(), Some(File::open(&path).unwrap()));
        work.nested(&["inner.rpf".to_string()], None);
        assert_eq!(work.added, 2 * inner_len);
        // The same archive again, in other letters and separators, counts once; one inside it counts its own size.
        work.nested(&["INNER.rpf".to_string()], None);
        work.nested(&["inner.rpf".to_string(), "deep.rpf".to_string()], None);
        assert_eq!(work.added, 2 * (inner_len + deep_len));
        // One the package makes is not in the file: only what is added to it counts.
        work.nested(&["new.rpf".to_string()], None);
        assert_eq!(work.added, 2 * (inner_len + deep_len));
        let mut fresh = Work::new(spool(dir.path()), 0, None);
        fresh.nested(&["new.rpf".to_string()], None);
        assert_eq!(fresh.added, 0);
    }

    #[test]
    fn a_spool_name_that_cannot_be_removed_is_reported_and_the_undo_is_not_called_clean() {
        use crate::fsroot::faults::{self, Mode, Point};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("x64")).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        let mut original = ArchiveEditor::new("a.rpf");
        original.add_file("a.txt", b"a".to_vec()).unwrap();
        original.save(&root.join("x64/a.rpf")).unwrap();
        let mut side = ArchiveEditor::new("side.rpf");
        side.add_file("s.txt", b"s".to_vec()).unwrap();
        side.save(&dir.path().join("side.rpf")).unwrap();
        let content = std::fs::read(dir.path().join("side.rpf")).unwrap();
        let oiv = package(dir.path(), r#"<archive path="x64\a.rpf"><add source="p">side.rpf</add></archive>"#, &[("content/p", &content)]);
        // A filesystem without unnamed files: the spool is a name that is removed at once, and the removal fails.
        NO_UNNAMED_FILES.with(|f| f.set(true));
        faults::arm_at(Point::Unlink, 1, Mode::Fail(io::ErrorKind::Other));
        let err = Package::open(&oiv).unwrap().install_with(&Game::open(&root).unwrap(), None).unwrap_err();
        assert!(faults::disarm());
        NO_UNNAMED_FILES.with(|f| f.set(false));
        let text = err.to_string();
        assert!(!text.contains("nothing was installed") && text.contains("could not be removed") && text.contains(".paleto-"), "{text}");
        let Error::Txn(failure) = &err else { panic!("{err:?}") };
        assert!(!failure.undone && failure.failures.len() == 1);
        let strays: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".paleto-")).collect();
        assert_eq!(strays.len(), 1, "the name the message gives is there");
        assert!(text.contains(&strays[0].file_name().to_string_lossy().into_owned()), "{text}");
        // Nothing else was installed.
        assert!(!root.join("mods").exists());
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
