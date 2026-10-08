//! Paths that go through archives: "update/update.rpf/dlc_patch/x.rpf/file".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::{Error, Game};

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
    /// file, copying it there first. Other archives are returned unchanged.
    pub fn editable(&self, game: Option<&Game>) -> Result<Self, Error> {
        let Some(game) = game else { return Ok(self.clone()) };
        let Some(rel) = in_game(&self.disk, game).filter(|rel| !rel.starts_with("mods")) else { return Ok(self.clone()) };
        let disk = game.mods_copy(&rel.to_string_lossy())?.expect("the original exists");
        Ok(ArchivePath { disk, inner: self.inner.clone() })
    }

    /// The copy of this archive in the mods folder, if there is one.
    pub fn in_mods(&self, game: &Game) -> Option<Self> {
        let rel = in_game(&self.disk, game)?;
        if rel.starts_with("mods") {
            return None;
        }
        let disk = crate::game::resolve_ci(&game.mods_dir(), &rel.to_string_lossy());
        disk.is_file().then(|| ArchivePath { disk, inner: self.inner.clone() })
    }

    /// Edits the folder (or nested archive) at this path: `f` gets the
    /// innermost editor and the folder inside it. The archive is saved after.
    pub fn edit<T>(
        &self,
        keys: Option<&Keys>,
        f: impl FnOnce(&mut ArchiveEditor, &str) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut root = ArchiveEditor::open(&self.disk, keys)?;
        let (editor, folder) = nested(&mut root, &self.inner, keys)?;
        let out = f(editor, &folder)?;
        root.save(&self.disk)?;
        Ok(out)
    }
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

/// Lists local files below `path` (or `path` itself) with their path relative
/// to `path`'s parent, for adding to an archive.
pub fn local_files(path: &Path) -> Result<Vec<(PathBuf, String)>, Error> {
    let base = path.parent().unwrap_or(Path::new(""));
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for entry in std::fs::read_dir(&p)? {
                stack.push(entry?.path());
            }
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            out.push((p, rel));
        }
    }
    out.sort();
    Ok(out)
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
}
