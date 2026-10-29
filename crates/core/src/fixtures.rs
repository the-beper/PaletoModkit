//! Helpers shared by the write-safety tests: synthetic game folders and
//! packages, folder snapshots and a leftover-name scan.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use paleto_rpf::ArchiveEditor;

use crate::game::{Edition, Game};

/// An archive at `path` holding `files`.
pub(crate) fn rpf(path: &Path, name: &str, files: &[(&str, &[u8])]) {
    let mut ed = ArchiveEditor::new(name);
    for (inner, data) in files {
        ed.add_file(inner, data.to_vec()).unwrap();
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    ed.save(path).unwrap();
}

/// An `update.rpf` whose DLC list names `packs`.
pub(crate) fn update_rpf(path: &Path, packs: &[&str]) {
    let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
    let list = format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n");
    rpf(path, "update.rpf", &[("common/data/dlclist.xml", list.as_bytes())]);
}

/// A `dlc.rpf` with a setup2.xml naming it `name_hash`.
pub(crate) fn dlc_pack(path: &Path, name_hash: &str) {
    let setup = format!("<SSetupData><nameHash>{name_hash}</nameHash></SSetupData>");
    rpf(path, "dlc.rpf", &[("setup2.xml", setup.as_bytes())]);
}

/// A game folder `dir/game`: the edition's executable, `update/update.rpf`
/// (DLC list with `mpheist`), `x64/a.rpf` (with `a.txt`) and `scripts/orig.asi`.
pub(crate) fn game_folder(dir: &Path, edition: Edition) -> Game {
    let root = dir.join("game");
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(root.join(edition.exe_name()), b"exe").unwrap();
    update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
    rpf(&root.join("x64/a.rpf"), "a.rpf", &[("a.txt", b"a")]);
    std::fs::write(root.join("scripts/orig.asi"), b"orig").unwrap();
    Game::open(&root).unwrap()
}

/// A package `dir/name` with these commands and `content/` files.
pub(crate) fn oiv(dir: &Path, name: &str, commands: &str, files: &[(&str, &[u8])], deflate: bool) -> PathBuf {
    let assembly = format!(r#"<package version="2.2" target="Five"><metadata><name>X</name></metadata><content>{commands}</content></package>"#);
    let entries = std::iter::once(("assembly.xml".to_string(), assembly.into_bytes())).chain(files.iter().map(|(n, d)| (n.to_string(), d.to_vec())));
    oiv_raw(dir, name, entries, deflate)
}

/// A zip `dir/name` of exactly these entries.
pub(crate) fn oiv_raw(dir: &Path, name: &str, entries: impl IntoIterator<Item = (String, Vec<u8>)>, deflate: bool) -> PathBuf {
    let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
    let opts = zip::write::SimpleFileOptions::default().compression_method(method);
    let path = dir.join(name);
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    for (entry, data) in entries {
        zip.start_file(entry, opts).unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap();
    path
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Dir,
    File(Vec<u8>),
    /// A symbolic link, by its target.
    Link(PathBuf),
}

/// Every name under `dir` (relative, '/' separated), links not followed.
pub(crate) fn snapshot(dir: &Path) -> BTreeMap<String, Entry> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_symlink() {
                out.insert(rel, Entry::Link(std::fs::read_link(&path).unwrap()));
            } else if kind.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(base, &path, out);
            } else {
                out.insert(rel, Entry::File(std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

pub(crate) fn assert_unchanged(before: &BTreeMap<String, Entry>, after: &BTreeMap<String, Entry>) {
    let mut diffs = Vec::new();
    for (name, old) in before {
        match after.get(name) {
            None => diffs.push(format!("removed {name}")),
            Some(new) if new != old => diffs.push(format!("changed {name}")),
            Some(_) => {}
        }
    }
    diffs.extend(after.keys().filter(|n| !before.contains_key(*n)).map(|n| format!("added {n}")));
    assert!(diffs.is_empty(), "the folder changed: {diffs:?}");
}

/// Temporary names (`.paleto-tmp`, `.paleto-undo`, `.new`, `.partial`) under `dir`.
pub(crate) fn leftovers(dir: &Path) -> Vec<String> {
    snapshot(dir)
        .into_keys()
        .filter(|rel| {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            name.contains(".paleto-tmp") || name.contains(".paleto-undo") || name.ends_with(".new") || name.ends_with(".partial")
        })
        .collect()
}

/// Makes a folder read-only; writable again when dropped, so the temporary
/// folder can be removed.
pub(crate) struct ReadOnly(PathBuf);

impl ReadOnly {
    pub(crate) fn new(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
        ReadOnly(path.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

pub(crate) fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
