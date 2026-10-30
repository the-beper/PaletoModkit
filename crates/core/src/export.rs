//! Making an OIV package from the changes in the mods folder: the files that
//! differ from the game's (inside archives too, nested ones included), the
//! files deleted from archives, and add-on packs with their DLC list entry.
//! Installed with any OIV installer, the package rebuilds those changes on a
//! clean mods folder (after a game update, a clean-up, on another machine).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use paleto_rpf::{Archive, Keys};
use sha2::{Digest, Sha256};

use crate::game::{resolve_ci, same_file};
use crate::loader::{DLC_LIST, dlc_pack_order, file_stamp};
use crate::vfs::entry_differs;
use crate::{Error, Game};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Not in the game's files.
    Added,
    Modified,
    /// In the game's archive, not in the mods copy.
    Deleted,
    /// A file of an add-on pack (a dlcpacks folder the game does not have).
    Pack,
}

/// A change of the mods folder: its path relative to the mods folder, with
/// '/' between archives and folders ("x64e.rpf/levels/gta5/vehicles.rpf/adder.yft").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub kind: Kind,
    pub size: u64,
}

/// Every change of the mods folder against the game's files. Reads the
/// archives' files that might differ (the same size), so it takes a while.
pub fn changed_files(game: &Game, keys: Option<&Keys>) -> Result<Vec<Changed>, Error> {
    changed_files_cached(game, keys, &mut ChangesCache::default(), &|| false)
}

/// [`changed_files`], reading again only the files of the mods folder that
/// changed since `cache` saw them (by size and time, of the copy and its original).
/// Gives up with [`Error::Cancelled`] as soon as `stop` says so (checked for each
/// file); the cache then keeps nothing of this run.
pub fn changed_files_cached(game: &Game, keys: Option<&Keys>, cache: &mut ChangesCache, stop: &dyn Fn() -> bool) -> Result<Vec<Changed>, Error> {
    let mods = game.mods_dir();
    let mut out = Vec::new();
    if !mods.is_dir() {
        return Ok(out);
    }
    let mut seen = HashMap::new();
    for (file, _) in crate::vfs::local_files(&mods)? {
        if stop() {
            return Err(Error::Cancelled);
        }
        let rel = file.strip_prefix(&mods).unwrap().to_string_lossy().replace('\\', "/");
        let lower = rel.to_ascii_lowercase();
        let size = std::fs::metadata(&file).map_or(0, |m| m.len());
        if let Some(pack) = lower.strip_prefix("update/x64/dlcpacks/").and_then(|r| r.split('/').next())
            && !resolve_ci(&game.root, &format!("update/x64/dlcpacks/{pack}")).is_dir()
        {
            out.push(Changed { path: rel, kind: Kind::Pack, size });
            continue;
        }
        let original = game.original(&rel);
        if !original.is_file() {
            out.push(Changed { path: rel, kind: Kind::Added, size });
            continue;
        }
        let key = file.to_string_lossy().into_owned();
        let stamp = format!("{}|{}", file_stamp(&file), file_stamp(&original));
        let found = match cache.entries.get(&key).filter(|(s, _)| *s == stamp) {
            Some((_, found)) => found.clone(),
            None => {
                let mut found = Vec::new();
                if lower.ends_with(".rpf") {
                    let modded = Archive::open(&file, keys)?;
                    let original = Archive::open(&original, keys)?;
                    diff_archives(&rel, &modded, &original, keys, stop, &mut found);
                    // What it found before stopping is not all there is.
                    if stop() {
                        return Err(Error::Cancelled);
                    }
                } else if !same_file(&file, &original) {
                    found.push(Changed { path: rel, kind: Kind::Modified, size });
                }
                found
            }
        };
        out.extend(found.iter().cloned());
        seen.insert(key, (stamp, found));
    }
    let prefix = mods.to_string_lossy().into_owned();
    cache.entries.retain(|k, _| !k.starts_with(&prefix));
    cache.entries.extend(seen);
    Ok(out)
}

/// [`changed_files_cached`] with the cache kept in `cache_path`.
pub fn changed_files_cached_at(game: &Game, keys: Option<&Keys>, cache_path: &Path, stop: &dyn Fn() -> bool) -> Result<Vec<Changed>, Error> {
    let mut cache = ChangesCache::load(cache_path);
    let found = changed_files_cached(game, keys, &mut cache, stop)?;
    cache.entries.retain(|k, _| Path::new(k).is_file());
    // A cache that cannot be written only costs time.
    let _ = cache.save(cache_path);
    Ok(found)
}

/// What [`changed_files_cached`] found in each file of mods folders, by its path on disk.
#[derive(Debug, Default)]
pub struct ChangesCache {
    entries: HashMap<String, (String, Vec<Changed>)>,
}

impl ChangesCache {
    /// Where the GUI and the CLI keep it.
    pub fn default_path() -> std::path::PathBuf {
        crate::game::data_dir().join("changes.tsv")
    }

    /// Lines: "file\tstamp", then "\tkind\tsize\tpath" for each change in it.
    pub fn load(path: &Path) -> Self {
        let mut entries: HashMap<String, (String, Vec<Changed>)> = HashMap::new();
        let mut current: Option<String> = None;
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            if let Some(rest) = line.strip_prefix('\t') {
                let mut f = rest.splitn(3, '\t');
                let (Some(kind), Some(size), Some(p), Some(key)) = (f.next(), f.next(), f.next(), &current) else { continue };
                let kind = match kind {
                    "added" => Kind::Added,
                    "modified" => Kind::Modified,
                    "deleted" => Kind::Deleted,
                    _ => continue,
                };
                entries.get_mut(key).unwrap().1.push(Changed { path: p.into(), kind, size: size.parse().unwrap_or(0) });
            } else if let Some((key, stamp)) = line.split_once('\t') {
                entries.insert(key.into(), (stamp.into(), Vec::new()));
                current = Some(key.into());
            }
        }
        ChangesCache { entries }
    }

    /// Saves it (atomically: a new file renamed over the old one).
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        let mut text = String::new();
        for key in keys {
            let (stamp, found) = &self.entries[key];
            text += &format!("{key}\t{stamp}\n");
            for c in found {
                let kind = format!("{:?}", c.kind).to_ascii_lowercase();
                text += &format!("\t{kind}\t{}\t{}\n", c.size, c.path);
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        static SAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Stops early, with part of the changes, once `stop` says so.
fn diff_archives(prefix: &str, modded: &Archive, original: &Archive, keys: Option<&Keys>, stop: &dyn Fn() -> bool, out: &mut Vec<Changed>) {
    let files = |a: &Archive| -> HashMap<String, (String, usize)> {
        // Names that are not plain relative paths (the parser refuses them) never get into a package.
        a.walk(0).into_iter().filter(|(p, i)| !a.entries[*i].is_dir() && crate::vfs::is_plain_entry_path(p)).map(|(p, i)| (p.to_ascii_lowercase(), (p, i))).collect()
    };
    let (ours, theirs) = (files(modded), files(original));
    let mut found = Vec::new();
    for (lower, (path, i)) in &ours {
        if stop() {
            return;
        }
        let full = format!("{prefix}/{path}");
        let entry = &modded.entries[*i];
        match theirs.get(lower) {
            None => found.push(Changed { path: full, kind: Kind::Added, size: entry.size() }),
            Some((_, j)) if entry.is_archive() && original.entries[*j].is_archive() => {
                match (modded.open_child(*i, keys), original.open_child(*j, keys)) {
                    (Ok(a), Ok(b)) => diff_archives(&full, &a, &b, keys, stop, &mut found),
                    _ if entry_differs(modded, *i, original, *j, keys) => found.push(Changed { path: full, kind: Kind::Modified, size: entry.size() }),
                    _ => {}
                }
            }
            Some((_, j)) if entry_differs(modded, *i, original, *j, keys) => {
                found.push(Changed { path: full, kind: Kind::Modified, size: entry.size() })
            }
            Some(_) => {}
        }
    }
    for (lower, (path, j)) in &theirs {
        if !ours.contains_key(lower) {
            found.push(Changed { path: format!("{prefix}/{path}"), kind: Kind::Deleted, size: original.entries[*j].size() });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    out.extend(found);
}

/// What the package says about itself.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

/// The commands for one archive (or the game folder, at the root).
#[derive(Default)]
struct Node {
    archives: BTreeMap<String, Node>,
    /// (target inside, source in the package)
    adds: Vec<(String, String)>,
    deletes: Vec<String>,
    /// Packs to name in this archive's DLC list.
    dlc_list: Vec<String>,
}

/// Writes an OIV package of these changes (from [`changed_files`]). A changed
/// DLC list becomes entries added to the installing user's list (the packs
/// named in the mods copy and not in the game's), not a whole new list.
pub fn write_oiv(game: &Game, keys: Option<&Keys>, changes: &[Changed], meta: &Metadata, out: &Path) -> Result<usize, Error> {
    let mut root = Node::default();
    let mut payload: Vec<(String, Vec<u8>)> = Vec::new();
    let (list_archive, list_inner) = DLC_LIST;
    for c in changes {
        let source = c.path.clone();
        match locate(&c.path) {
            // A file of the game folder: a loose file, a whole archive, a pack.
            None => {
                payload.push((source.clone(), std::fs::read(game.mods_dir().join(&c.path))?));
                root.adds.push((c.path.replace('/', "\\"), source));
            }
            Some((top, nested, file)) if !matches!(c.kind, Kind::Pack | Kind::Added) || game.original(&top).is_file() => {
                let mut node = root.archives.entry(top.clone()).or_default();
                for n in &nested {
                    node = node.archives.entry(n.clone()).or_default();
                }
                if c.kind == Kind::Deleted {
                    node.deletes.push(file.replace('/', "\\"));
                } else if nested.is_empty() && top.eq_ignore_ascii_case(list_archive) && file.eq_ignore_ascii_case(list_inner) {
                    node.dlc_list = added_packs(game, keys)?;
                } else {
                    payload.push((source.clone(), read_inside(game, keys, &top, &nested, &file)?));
                    node.adds.push((file.replace('/', "\\"), source));
                }
            }
            // A file inside an archive the game does not have (an add-on
            // pack's): the archive is copied whole, once.
            Some((top, _, _)) => {
                if !payload.iter().any(|(s, _)| *s == top) {
                    payload.push((top.clone(), std::fs::read(game.mods_dir().join(&top))?));
                    root.adds.push((top.replace('/', "\\"), top));
                }
            }
        }
    }
    // Add-on packs exported need their entry in the DLC list.
    let packs: Vec<String> = changes
        .iter()
        .filter(|c| c.kind == Kind::Pack)
        .filter_map(|c| c.path.to_ascii_lowercase().strip_prefix("update/x64/dlcpacks/").and_then(|r| r.split('/').next()).map(str::to_string))
        .collect();
    if !packs.is_empty() {
        let node = root.archives.entry(list_archive.to_string()).or_default();
        for p in packs {
            if !node.dlc_list.contains(&p) {
                node.dlc_list.push(p);
            }
        }
    }

    let mut xml = String::new();
    write_node(&root, &mut xml, 2, true);
    let id = {
        let hash = Sha256::digest(format!("{}{:?}", meta.name, std::time::SystemTime::now()).as_bytes());
        let h: String = hash.iter().take(16).map(|b| format!("{b:02X}")).collect();
        format!("{{{}-{}-{}-{}-{}}}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
    };
    let (major, minor) = meta.version.split_once('.').unwrap_or((meta.version.as_str(), "0"));
    let assembly = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<package version=\"2.2\" id=\"{id}\" target=\"Five\">\n\t<metadata>\n\t\t<name>{}</name>\n\t\t<version>\n\t\t\t<major>{}</major>\n\t\t\t<minor>{}</minor>\n\t\t</version>\n\t\t<author>\n\t\t\t<displayName>{}</displayName>\n\t\t</author>\n\t\t<description><![CDATA[{}]]></description>\n\t</metadata>\n\t<content>\n{xml}\t</content>\n</package>\n",
        escape(&meta.name),
        escape(major.trim()),
        escape(minor.trim()),
        escape(&meta.author),
        meta.description.replace("]]>", "]] >"),
    );

    // The package is written where the user chose: into a game folder, as a file of the confined writer.
    let folder = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = out.file_name().and_then(|n| n.to_str()).ok_or_else(|| Error::UnsafePath { path: out.display().to_string(), why: "is not a file name".into() })?;
    let need = payload.iter().map(|(_, data)| data.len() as u64).sum::<u64>() + assembly.len() as u64;
    let (_, log) = crate::vfs::write_into(folder, "writing a package", |o| {
        o.write_with(name, need, |file| write_zip(file, &assembly, &payload).map_err(|e| std::io::Error::other(e.to_string()))).map(drop)
    })?;
    for line in log {
        eprintln!("paleto: {line}");
    }
    Ok(changes.len())
}

/// The zip of an OIV package: its `assembly.xml` and the files of the content.
fn write_zip(file: &mut std::fs::File, assembly: &str, payload: &[(String, Vec<u8>)]) -> Result<(), Error> {
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let zip_err = |e: zip::result::ZipError| Error::Package(e.to_string());
    zip.start_file("assembly.xml", opts).map_err(zip_err)?;
    zip.write_all(assembly.as_bytes())?;
    for (source, data) in payload {
        zip.start_file(format!("content/{source}"), opts).map_err(zip_err)?;
        zip.write_all(data)?;
    }
    zip.finish().map_err(zip_err)?;
    Ok(())
}

/// "a/b.rpf/c/d.rpf/e.ydr" → ("a/b.rpf", ["c/d.rpf"], "e.ydr"); the last
/// segment is the file even when it is an archive itself. `None` outside archives.
pub(crate) fn locate(path: &str) -> Option<(String, Vec<String>, String)> {
    let segments: Vec<&str> = path.split('/').collect();
    let (file_at, archives) = (segments.len() - 1, &segments[..segments.len() - 1]);
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, seg) in archives.iter().enumerate() {
        if seg.to_ascii_lowercase().ends_with(".rpf") {
            parts.push(segments[start..=i].join("/"));
            start = i + 1;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let top = parts.remove(0);
    Some((top, parts, segments[start..=file_at].join("/")))
}

/// The packs the mods copy's DLC list names and the game's does not.
fn added_packs(game: &Game, keys: Option<&Keys>) -> Result<Vec<String>, Error> {
    let (rel, inner) = DLC_LIST;
    let read = |disk: std::path::PathBuf| -> Result<Vec<String>, Error> {
        let xml = inner.split('/').fold(crate::vfs::ArchivePath::new(disk), |p, part| p.join(part)).read(keys)?;
        Ok(dlc_pack_order(&String::from_utf8_lossy(&xml)))
    };
    let original = read(game.original(rel))?;
    Ok(read(resolve_ci(&game.mods_dir(), rel))?.into_iter().filter(|p| !original.contains(p)).collect())
}

/// A file inside an archive of the mods folder, as a standalone file.
fn read_inside(game: &Game, keys: Option<&Keys>, top: &str, nested: &[String], file: &str) -> Result<Vec<u8>, Error> {
    read_in(&game.mods_dir().join(top), keys, nested, file)
}

/// A file inside an archive on disk (nested archives named in `nested`).
pub(crate) fn read_in(disk: &Path, keys: Option<&Keys>, nested: &[String], file: &str) -> Result<Vec<u8>, Error> {
    let top = disk.display().to_string();
    let mut archive = Archive::open(disk, keys)?;
    for n in nested {
        let idx = archive.find(n).ok_or_else(|| Error::Package(format!("{n} not found in {top}")))?;
        archive = archive.open_child(idx, keys)?;
    }
    let idx = archive.find(file).ok_or_else(|| Error::Package(format!("{file} not found")))?;
    Ok(archive.read(idx, keys)?)
}

fn write_node(node: &Node, xml: &mut String, depth: usize, root: bool) {
    let tabs = "\t".repeat(depth);
    for (target, source) in &node.adds {
        xml.push_str(&format!("{tabs}<add source=\"{}\">{}</add>\n", escape(&source.replace('/', "\\")), escape(target)));
    }
    for d in &node.deletes {
        xml.push_str(&format!("{tabs}<delete>{}</delete>\n", escape(d)));
    }
    if !node.dlc_list.is_empty() && !root {
        xml.push_str(&format!("{tabs}<xml path=\"common\\data\\dlclist.xml\">\n"));
        for p in &node.dlc_list {
            xml.push_str(&format!(
                "{tabs}\t<remove xpath=\"/SMandatoryPacksData/Paths/Item[text()='dlcpacks:/{p}/']\" />\n{tabs}\t<add xpath=\"/SMandatoryPacksData/Paths\" append=\"Last\"><Item>dlcpacks:/{p}/</Item></add>\n"
            ));
        }
        xml.push_str(&format!("{tabs}</xml>\n"));
    }
    for (path, child) in &node.archives {
        xml.push_str(&format!("{tabs}<archive path=\"{}\" createIfNotExist=\"False\" type=\"RPF7\">\n", escape(&path.replace('/', "\\"))));
        write_node(child, xml, depth + 1, false);
        xml.push_str(&format!("{tabs}</archive>\n"));
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_rpf::ArchiveEditor;

    fn dlc_list(packs: &[&str]) -> Vec<u8> {
        let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
        format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n").into_bytes()
    }

    /// The files of an archive (nested ones too) with their content.
    fn contents(path: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(a: &Archive, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
            for (p, i) in a.walk(0) {
                let e = &a.entries[i];
                if e.is_archive() {
                    walk(&a.open_child(i, None).unwrap(), &format!("{prefix}{p}/"), out);
                } else if !e.is_dir() {
                    out.insert(format!("{prefix}{p}"), a.read(i, None).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&Archive::open(path, None).unwrap(), "", &mut out);
        out
    }

    #[test]
    fn a_diff_never_names_an_entry_that_is_not_a_plain_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = ArchiveEditor::new("a.rpf");
        for name in ["good.ydr", "evil.ydr", "empty.ydr"] {
            ed.add_file(name, vec![1]).unwrap();
        }
        let path = dir.path().join("a.rpf");
        ed.save(&path).unwrap();
        // The parser cannot produce these names: make the entry list by hand.
        let mut modded = Archive::open(&path, None).unwrap();
        let original = Archive::open(&path, None).unwrap();
        for (name, odd) in [("evil.ydr", "../evil.ydr"), ("empty.ydr", "")] {
            modded.entries.iter_mut().find(|e| e.name == name).unwrap().name = odd.into();
        }
        let mut out = Vec::new();
        diff_archives("a.rpf", &modded, &original, None, &|| false, &mut out);
        let paths: Vec<_> = out.iter().map(|c| c.path.as_str()).collect();
        // The odd names are left out (not reported as added); what the original has under the plain names shows as deleted.
        assert_eq!(paths, ["a.rpf/empty.ydr", "a.rpf/evil.ydr"]);
        assert!(out.iter().all(|c| c.kind == Kind::Deleted), "{out:?}");
    }

    #[test]
    fn locates_files_in_archives() {
        assert_eq!(locate("x64e.rpf/levels/extra.rpf"), Some(("x64e.rpf".into(), vec![], "levels/extra.rpf".into())));
        assert_eq!(locate("a/b.rpf/c/d.rpf/e.ydr"), Some(("a/b.rpf".into(), vec!["c/d.rpf".into()], "e.ydr".into())));
        assert_eq!(locate("update/x64/dlcpacks/car/dlc.rpf"), None);
        assert_eq!(locate("readme.txt"), None);
    }

    #[test]
    fn changes_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(root.join("update")).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        // The game's archives.
        let mut x64e = ArchiveEditor::new("x64e.rpf");
        x64e.add_file("props/keep.txt", b"keep".to_vec()).unwrap();
        x64e.add_file("props/gone.txt", b"gone".to_vec()).unwrap();
        let vehicles = x64e.archive("levels/vehicles.rpf", true, None).unwrap();
        vehicles.add_file("adder.txt", b"adder".to_vec()).unwrap();
        vehicles.add_file("zentorno.txt", b"zentorno".to_vec()).unwrap();
        x64e.save(&root.join("x64e.rpf")).unwrap();
        let mut update = ArchiveEditor::new("update.rpf");
        update.add_file("common/data/dlclist.xml", dlc_list(&["mpheist"])).unwrap();
        update.add_file("common/data/handling.meta", b"stock".to_vec()).unwrap();
        update.save(&root.join("update/update.rpf")).unwrap();
        let mut beach = ArchiveEditor::new("dlc.rpf");
        beach.add_file("setup2.xml", b"beach".to_vec()).unwrap();
        std::fs::create_dir_all(root.join("update/x64/dlcpacks/mpbeach")).unwrap();
        beach.save(&root.join("update/x64/dlcpacks/mpbeach/dlc.rpf")).unwrap();

        // The mods folder: changes in a nested archive, a deletion, a new
        // file, the handling changed, an add-on pack listed, a copy unchanged.
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/car")).unwrap();
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/mpbeach")).unwrap();
        std::fs::copy(root.join("update/x64/dlcpacks/mpbeach/dlc.rpf"), root.join("mods/update/x64/dlcpacks/mpbeach/dlc.rpf")).unwrap();
        let mut x64e = ArchiveEditor::open(&root.join("x64e.rpf"), None).unwrap();
        x64e.delete("props/gone.txt").unwrap();
        x64e.add_file("props/new.txt", b"new".to_vec()).unwrap();
        x64e.archive("levels/extra.rpf", true, None).unwrap().add_file("bus.txt", b"bus".to_vec()).unwrap();
        x64e.archive("levels/vehicles.rpf", false, None).unwrap().add_file("adder.txt", b"tuned adder".to_vec()).unwrap();
        x64e.save(&root.join("mods/x64e.rpf")).unwrap();
        let mut update = ArchiveEditor::open(&root.join("update/update.rpf"), None).unwrap();
        update.add_file("common/data/dlclist.xml", dlc_list(&["mpheist", "car"])).unwrap();
        update.add_file("common/data/handling.meta", b"driven".to_vec()).unwrap();
        update.save(&root.join("mods/update/update.rpf")).unwrap();
        let mut car = ArchiveEditor::new("dlc.rpf");
        car.add_file("setup2.xml", b"car".to_vec()).unwrap();
        car.save(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap();
        let game = Game::open(&root).unwrap();

        let cache = dir.path().join("changes.tsv");
        let changes = changed_files_cached_at(&game, None, &cache, &|| false).unwrap();
        // Unchanged archives are not read again: what the cache says comes back.
        let text = std::fs::read_to_string(&cache).unwrap();
        std::fs::write(&cache, text.replace("handling.meta", "cached.meta")).unwrap();
        assert!(changed_files_cached_at(&game, None, &cache, &|| false).unwrap().iter().any(|c| c.path.ends_with("cached.meta")));
        std::fs::write(&cache, text).unwrap();
        assert_eq!(changed_files_cached_at(&game, None, &cache, &|| false).unwrap(), changes);
        // Stopped halfway, it gives up and keeps no cache of that run.
        let calls = std::cell::Cell::new(0);
        let stop = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        let stopped = dir.path().join("stopped.tsv");
        assert!(matches!(changed_files_cached_at(&game, None, &stopped, &stop), Err(Error::Cancelled)));
        assert!(!stopped.exists());
        let list: Vec<(&str, Kind)> = changes.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(
            list,
            [
                ("update/update.rpf/common/data/dlclist.xml", Kind::Modified),
                ("update/update.rpf/common/data/handling.meta", Kind::Modified),
                ("update/x64/dlcpacks/car/dlc.rpf", Kind::Pack),
                ("x64e.rpf/levels/extra.rpf", Kind::Added),
                ("x64e.rpf/levels/vehicles.rpf/adder.txt", Kind::Modified),
                ("x64e.rpf/props/gone.txt", Kind::Deleted),
                ("x64e.rpf/props/new.txt", Kind::Added),
            ]
        );

        let oiv = dir.path().join("mine.oiv");
        let meta = Metadata { name: "My mods".into(), version: "1.0".into(), author: "me".into(), description: "test".into() };
        write_oiv(&game, None, &changes, &meta, &oiv).unwrap();
        // Into the game folder the package goes through the confined writer: not into original data, in `mods/` yes.
        assert!(write_oiv(&game, None, &changes, &meta, &root.join("update/p.oiv")).is_err());
        assert!(!root.join("update/p.oiv").exists());
        write_oiv(&game, None, &changes, &meta, &root.join("mods/p.oiv")).unwrap();
        assert_eq!(crate::Package::open(&root.join("mods/p.oiv")).unwrap().metadata.name, "My mods");
        std::fs::remove_file(root.join("mods/p.oiv")).unwrap();

        // On a clean mods folder, the package rebuilds the same files.
        let before = [contents(&root.join("mods/x64e.rpf")), contents(&root.join("mods/update/update.rpf"))];
        let car_before = std::fs::read(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap();
        std::fs::rename(root.join("mods"), dir.path().join("old mods")).unwrap();
        let mut pkg = crate::Package::open(&oiv).unwrap();
        assert_eq!(pkg.metadata.name, "My mods");
        pkg.install_with(&game, None).unwrap();
        assert_eq!(contents(&root.join("mods/x64e.rpf")), before[0]);
        let after = contents(&root.join("mods/update/update.rpf"));
        assert_eq!(after["common/data/handling.meta"], b"driven");
        let list = String::from_utf8(after["common/data/dlclist.xml"].clone()).unwrap();
        assert_eq!(dlc_pack_order(&list), ["mpheist", "car"], "{list}");
        assert_eq!(std::fs::read(root.join("mods/update/x64/dlcpacks/car/dlc.rpf")).unwrap(), car_before);
        assert!(!root.join("mods/update/x64/dlcpacks/mpbeach").exists(), "unchanged copies are not exported");
        // Installed twice, the pack is listed once.
        let mut pkg = crate::Package::open(&oiv).unwrap();
        pkg.install_with(&game, None).unwrap();
        let list = String::from_utf8(contents(&root.join("mods/update/update.rpf"))["common/data/dlclist.xml"].clone()).unwrap();
        assert_eq!(dlc_pack_order(&list), ["mpheist", "car"], "{list}");
    }
}
