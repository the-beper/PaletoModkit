//! OpenIV packages (`.oiv`, format 2.x): a zip with `assembly.xml`, an
//! optional `icon.png` and a `content/` folder.
//!
//! Installation always targets the mods folder for game data; the original
//! game files are never modified.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use paleto_rpf::{ArchiveEditor, Keys};
use zip::ZipArchive;

use crate::convert;
use crate::fsroot::{GameRoots, Resolved, RootKind};
use crate::game::Game;
use crate::relpath::RelPath;
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
    zip: ZipArchive<File>,
    pub metadata: Metadata,
    content: Vec<Command>,
    /// Content converted to the Enhanced format, by zip entry name (lower case),
    /// installed in place of the package's own.
    converted: HashMap<String, Vec<u8>>,
}

impl Package {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut zip = ZipArchive::new(File::open(path)?).map_err(zip_err)?;
        let assembly = read_entry(&mut zip, "assembly.xml")?;
        let assembly = String::from_utf8_lossy(assembly.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&assembly)).into_owned();
        let doc = roxmltree::Document::parse(&assembly).map_err(|e| Error::Package(format!("assembly.xml: {e}")))?;
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
        Ok(Package { zip, metadata, content, converted: HashMap::new() })
    }

    pub fn icon(&mut self) -> Option<Vec<u8>> {
        read_entry(&mut self.zip, "icon.png").ok()
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
        let mut found = Vec::new();
        for source in self.add_sources() {
            let name = source.replace('\\', "/");
            let data = read_entry(&mut self.zip, &content_path(&source))?;
            if convert::needs_conversion(&name, &data) {
                found.push(name);
            } else if name.to_ascii_lowercase().ends_with(".rpf") {
                let dir = convert::scratch_dir()?;
                let disk = dir.join("archive.rpf");
                let result = std::fs::write(&disk, &data).map_err(Error::from).and_then(|_| Ok(paleto_rpf::Archive::open(&disk, keys)?));
                if let Ok(archive) = result {
                    found.extend(convert::legacy_in_archive(&archive, keys).into_iter().map(|p| format!("{name}/{p}")));
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        Ok(found)
    }

    /// Converts the package's Legacy files (see [`Package::legacy_files`])
    /// with `tool`, so that [`Package::install`] installs the Enhanced ones.
    /// Returns a log; files that cannot be converted are reported there and
    /// installed as they are.
    pub fn convert_legacy(&mut self, tool: &Path, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
        let mut log = Vec::new();
        let mut loose = Vec::new();
        for source in self.add_sources() {
            let name = source.replace('\\', "/");
            let entry = content_path(&source).to_ascii_lowercase();
            let data = read_entry(&mut self.zip, &entry)?;
            if convert::needs_conversion(&name, &data) {
                loose.push((name, entry, data));
            } else if name.to_ascii_lowercase().ends_with(".rpf") {
                let dir = convert::scratch_dir()?;
                let disk = dir.join("archive.rpf");
                std::fs::write(&disk, &data)?;
                let result = convert::convert_whole_archive(&disk, keys, tool);
                let converted = result.and_then(|report| {
                    for r in &report {
                        log.push(match &r.error {
                            None => format!("converted {name}/{}", r.path),
                            Some(e) => format!("warning: {name}/{}: not converted: {e}", r.path),
                        });
                    }
                    Ok(report.iter().any(|r| r.error.is_none()).then(|| std::fs::read(&disk)).transpose()?)
                });
                let _ = std::fs::remove_dir_all(&dir);
                if let Some(new) = converted? {
                    self.converted.insert(entry, new);
                }
            }
        }
        if !loose.is_empty() {
            let inputs: Vec<(&str, &[u8])> = loose.iter().map(|(n, _, d)| (n.as_str(), d.as_slice())).collect();
            for ((name, entry, _), outcome) in loose.iter().zip(convert::convert(tool, &inputs)?) {
                match outcome {
                    convert::Outcome::Converted(new) => {
                        self.converted.insert(entry.clone(), new);
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
        self.check(&self.content.clone())?;
        let roots = game.roots()?;
        let mut inst = Installer { roots, keys, zip: &mut self.zip, converted: &self.converted, archives: BTreeMap::new(), log: Vec::new(), undo: Undo::default() };
        if let Err(e) = self.content.clone().iter().try_for_each(|cmd| inst.run_root(cmd)) {
            // Archives are only saved at the end: putting the loose files back undoes it all.
            inst.undo.roll_back();
            return Err(Error::Package(format!("{e} (nothing was installed)")));
        }
        inst.undo.finish();
        for (path, editor) in std::mem::take(&mut inst.archives) {
            editor.save(&path)?;
            inst.log.push(format!("saved {}", path.display()));
        }
        Ok(inst.log)
    }

    /// Fails early on anything we cannot do, before touching any file.
    fn check(&mut self, cmds: &[Command]) -> Result<(), Error> {
        for cmd in cmds {
            match cmd {
                Command::Add { source, .. } => {
                    find_entry(&self.zip, &content_path(source)).ok_or_else(|| Error::Package(format!("missing content/{source}")))?;
                }
                Command::Archive { body, .. } => self.check(body)?,
                _ => {}
            }
        }
        Ok(())
    }
}

struct Installer<'a> {
    /// Where files may be written: every destination is resolved through it.
    roots: GameRoots,
    keys: Option<&'a Keys>,
    zip: &'a mut ZipArchive<File>,
    converted: &'a HashMap<String, Vec<u8>>,
    /// Top-level archives being edited, by path in the mods folder.
    archives: BTreeMap<PathBuf, ArchiveEditor>,
    log: Vec<String>,
    undo: Undo,
}

/// What an install changed on disk so far, to put back if it fails halfway.
/// Files are only ever replaced by a rename (never rewritten in place), so a
/// hard link keeps the old content. A failure while the archives are saved, at
/// the very end, is not undone.
#[derive(Default)]
struct Undo {
    seen: std::collections::HashSet<PathBuf>,
    /// Files that did not exist.
    made: Vec<PathBuf>,
    /// Files replaced or deleted, and the link that keeps their old content.
    kept: Vec<(PathBuf, PathBuf)>,
}

impl Undo {
    /// Remembers a file before its first change.
    fn keep(&mut self, path: &Path) -> Result<(), Error> {
        if !self.seen.insert(path.to_path_buf()) {
            return Ok(());
        }
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let saved = path.with_file_name(format!(".{name}.paleto-undo"));
                let _ = std::fs::remove_file(&saved);
                // Some filesystems (exFAT) have no hard links: a copy then.
                if std::fs::hard_link(path, &saved).is_err() {
                    std::fs::copy(path, &saved)?;
                }
                self.kept.push((path.to_path_buf(), saved));
            }
            Err(_) => self.made.push(path.to_path_buf()),
        }
        Ok(())
    }

    fn roll_back(&mut self) {
        for path in self.made.drain(..) {
            let _ = std::fs::remove_file(path);
        }
        for (path, saved) in self.kept.drain(..) {
            let _ = std::fs::rename(saved, path);
        }
    }

    fn finish(&mut self) {
        self.made.clear();
        for (_, saved) in self.kept.drain(..) {
            let _ = std::fs::remove_file(saved);
        }
    }
}

impl Installer<'_> {
    /// The data of an `<add>` source: the converted file if there is one.
    fn source(&mut self, source: &str) -> Result<Vec<u8>, Error> {
        let entry = content_path(source);
        match self.converted.get(&entry.to_ascii_lowercase()) {
            Some(data) => Ok(data.clone()),
            None => read_entry(self.zip, &entry),
        }
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
                let data = self.source(source)?;
                self.roots.ensure_parent(&mut dest)?;
                let path = dest.path();
                self.archives.remove(&path);
                self.undo.keep(&path)?;
                // A copy in the mods folder may be a link to an original: replace it, never write through it.
                self.roots.replace(&mut dest, |f| f.write_all(&data))?;
                self.log.push(format!("add {}", path.display()));
            }
            Command::Delete(target) => {
                // Game data is only ever deleted from the mods folder.
                let dest = self.roots.dest(&RelPath::parse(target)?)?;
                let path = dest.path();
                if let Some(why) = self.refused(&dest) {
                    self.log.push(format!("warning: refusing to delete {}{why}", path.display()));
                } else if self.roots.read_dest(&dest)?.is_some() {
                    self.undo.keep(&path)?;
                    self.roots.remove(&dest)?;
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
                editor.add_file(target, self.source(source)?)?;
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
        if std::fs::symlink_metadata(&disk).is_err() {
            self.undo.keep(&disk)?;
        }
        let disk = match self.roots.mods_copy(&rel.without_mods())? {
            Some(p) => p,
            // Saved by a rename: a dangling link there is replaced, never written through.
            None if create => {
                self.roots.ensure_parent(&mut dest)?;
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
        self.undo.keep(&dest.path())?;
        if dest.root == RootKind::Mods && !rel.in_mods_folder() {
            self.roots.mods_copy(&rel)?;
            return Ok(Some(self.roots.dest(&rel)?));
        }
        Ok(Some(dest))
    }

    /// What a loose file holds now; `None` if there is no file.
    fn read_loose(&mut self, dest: &Resolved) -> Result<Option<Vec<u8>>, Error> {
        let mut data = Vec::new();
        Ok(self.roots.read_dest(dest)?.map(|mut f| f.read_to_end(&mut data).map(|_| data)).transpose()?)
    }

    /// Writes a loose file by replacing it: never through a link, and the
    /// old content stays for [`Undo`].
    fn write_loose(&mut self, dest: &mut Resolved, data: Vec<u8>) -> Result<(), Error> {
        self.roots.ensure_parent(dest)?;
        self.undo.keep(&dest.path())?;
        self.roots.replace(dest, |f| f.write_all(&data))?;
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

/// The index of a zip entry, matching the name case-insensitively and
/// accepting backslash separators (packages made on Windows use both).
fn find_entry(zip: &ZipArchive<File>, name: &str) -> Option<usize> {
    let wanted = name.replace('\\', "/").to_ascii_lowercase();
    (0..zip.len()).find(|&i| zip.name_for_index(i).is_some_and(|n| n.is_ok_and(|n| n.replace('\\', "/").to_ascii_lowercase() == wanted)))
}

/// Reads a zip entry found by [`find_entry`].
fn read_entry(zip: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, Error> {
    let index = find_entry(zip, name).ok_or_else(|| Error::Package(format!("missing {name}")))?;
    let mut entry = zip.by_index(index).map_err(zip_err)?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data)?;
    Ok(data)
}

fn zip_err(e: zip::result::ZipError) -> Error {
    Error::Package(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_formats::hash::{joaat, joaat_lower};

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
