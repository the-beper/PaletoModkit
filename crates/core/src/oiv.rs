//! OpenIV packages (`.oiv`, format 2.x): a zip with `assembly.xml`, an
//! optional `icon.png` and a `content/` folder.
//!
//! Installation always targets the mods folder for game data; the original
//! game files are never modified.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use paleto_rpf::{ArchiveEditor, Keys};
use zip::ZipArchive;

use crate::convert;
use crate::game::Game;
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
        let mut inst = Installer { game, keys, zip: &mut self.zip, converted: &self.converted, archives: BTreeMap::new(), log: Vec::new() };
        for cmd in &self.content.clone() {
            inst.run_root(cmd)?;
        }
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
                    self.zip.by_name(&content_path(source)).map_err(|_| Error::Package(format!("missing content/{source}")))?;
                }
                Command::Archive { body, .. } => self.check(body)?,
                _ => {}
            }
        }
        Ok(())
    }
}

struct Installer<'a> {
    game: &'a Game,
    keys: Option<&'a Keys>,
    zip: &'a mut ZipArchive<File>,
    converted: &'a HashMap<String, Vec<u8>>,
    /// Top-level archives being edited, by path in the mods folder.
    archives: BTreeMap<PathBuf, ArchiveEditor>,
    log: Vec<String>,
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
                let data = self.source(source)?;
                let dest = self.game.target(target);
                self.archives.remove(&dest);
                std::fs::create_dir_all(dest.parent().unwrap())?;
                std::fs::write(&dest, data)?;
                self.log.push(format!("add {}", dest.display()));
            }
            Command::Delete(target) => {
                // Game data is only ever deleted from the mods folder.
                let dest = self.game.target(target);
                if dest.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
                    self.log.push(format!("warning: refusing to delete {}", dest.display()));
                } else if dest.is_file() {
                    std::fs::remove_file(&dest)?;
                    self.log.push(format!("delete {}", dest.display()));
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
                let dest = self.loose_file(path)?;
                let text = match std::fs::read(&dest) {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(_) if *create => String::new(),
                    Err(_) => return Err(Error::Package(format!("{path} not found"))),
                };
                std::fs::create_dir_all(dest.parent().unwrap())?;
                std::fs::write(&dest, text::apply(&text, ops))?;
                self.log.push(format!("edit {}", dest.display()));
            }
            Command::Xml { path, ops } => {
                let dest = self.loose_file(path)?;
                let data = std::fs::read(&dest).map_err(|_| Error::Package(format!("{path} not found")))?;
                let (out, warnings) = xml::apply(&data, ops)?;
                self.warn(path, warnings);
                std::fs::write(&dest, out)?;
                self.log.push(format!("edit {}", dest.display()));
            }
            Command::Defragment(path) => {
                let disk = self.top_archive(path, false)?;
                self.archives.get_mut(&disk).unwrap().defragment();
            }
            Command::Pso { path, ops, source } => {
                let dest = self.loose_file(path)?;
                let data = std::fs::read(&dest).map_err(|_| Error::Package(format!("{path} not found")))?;
                let out = self.edit_meta(path, &data, ops, source)?;
                std::fs::write(&dest, out)?;
                self.log.push(format!("edit {}", dest.display()));
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
                let text = match editor.read(path, keys) {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(paleto_rpf::Error::NotFound(_)) if *create => String::new(),
                    Err(e) => return Err(e.into()),
                };
                editor.add_file(path, text::apply(&text, ops).into_bytes())?;
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
        let disk = self.game.target(path);
        if self.archives.contains_key(&disk) {
            return Ok(disk);
        }
        let disk = match self.game.mods_copy(path)? {
            Some(p) => p,
            None if create => disk,
            None => return Err(Error::Package(format!("archive {path} not found"))),
        };
        {
            let editor = if disk.is_file() {
                ArchiveEditor::open(&disk, self.keys)?
            } else {
                std::fs::create_dir_all(disk.parent().unwrap())?;
                ArchiveEditor::new(&disk.file_name().unwrap().to_string_lossy())
            };
            self.archives.insert(disk.clone(), editor);
        }
        Ok(disk)
    }

    /// A loose file to edit: game data is copied to the mods folder first.
    fn loose_file(&self, path: &str) -> Result<PathBuf, Error> {
        let dest = self.game.target(path);
        if dest.starts_with(self.game.mods_dir()) {
            self.game.mods_copy(path)?;
        }
        Ok(dest)
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
        let cmd = match n.tag_name().name() {
            "add" => Command::Add { source: attr("source")?, target: text() },
            "delete" => Command::Delete(text()),
            "archive" => {
                let kind = n.attribute("type").unwrap_or("RPF7");
                if !kind.eq_ignore_ascii_case("rpf7") {
                    return Err(Error::Package(format!("unsupported archive type {kind}")));
                }
                Command::Archive { path: attr("path")?, create: flag("createIfNotExist"), body: parse_commands(n, src)? }
            }
            "defragmentation" => Command::Defragment(attr("archive")?),
            "text" => Command::Text { path: attr("path")?, create: flag("createIfNotExist"), ops: text_ops(n)? },
            "xml" => Command::Xml { path: attr("path")?, ops: xml_ops(n, src)? },
            "pso" => Command::Pso { path: attr("path")?, ops: xml_ops(n, src)?, source: src[n.range()].to_string() },
            other => return Err(Error::Package(format!("unknown command <{other}>"))),
        };
        out.push(cmd);
    }
    Ok(out)
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

/// Reads a zip entry, matching the name case-insensitively and accepting
/// backslash separators (packages made on Windows use both).
fn read_entry(zip: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, Error> {
    let wanted = name.replace('\\', "/").to_ascii_lowercase();
    let index = (0..zip.len())
        .find(|&i| zip.name_for_index(i).is_some_and(|n| n.is_ok_and(|n| n.replace('\\', "/").to_ascii_lowercase() == wanted)))
        .ok_or_else(|| Error::Package(format!("missing {name}")))?;
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
