//! The user names store: names the user of this program added, kept in the
//! program's data dir, one per line (`XDG_DATA_HOME/paleto/names.txt`). The
//! hash tool resolves against the built-in names and these; F07 later fills the
//! store with names harvested from the user's own install. The file is plain
//! text, so it can be written by hand: `#` lines and empty lines are skipped.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use paleto_formats::Names;

use crate::budget::{self, Budget};
use crate::game;

/// A list of names is a plain text file; nobody needs more than this (16 MiB)
/// at once, and a bigger one says "no".
const LIST_LIMIT: u64 = 16 << 20;

/// Where the store is: `$XDG_DATA_HOME/paleto/names.txt`.
pub fn path() -> PathBuf {
    game::data_dir().join("names.txt")
}

/// The names of the store, in file order, first of every equal name kept and
/// normalized (trimmed, no empty or `#` line). No store file means no names.
pub fn load(path: &Path) -> Vec<String> {
    normalize(&fs::read_to_string(path).unwrap_or_default())
}

/// A plain-text list of names (one per line), normalized the way [`load`] does.
pub fn normalize(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let name = line.trim();
        if name.is_empty() || name.starts_with('#') || names.iter().any(|n| n == name) {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

/// Saves the store atomically (a new file renamed over the old one).
pub fn save(path: &Path, names: &[String]) -> Result<(), crate::Error> {
    let mut text = String::new();
    for name in names {
        text += name;
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    static SAVE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SAVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Adds every name of a plain-text list (one per line) to the store, skipping
/// the names it already has. The list is read bounded ([] in [`Budget::read`]);
/// the store file is replaced atomically. Returns how many names were added.
pub fn import(list: &Path, store: &Path) -> Result<usize, crate::Error> {
    let file = fs::File::open(list)?;
    let declared = file.metadata()?.len();
    let budget = Budget::new(budget::Limits::PACKAGE);
    let text = budget.read(&mut file.take(LIST_LIMIT), declared, LIST_LIMIT, "a names list")?;
    let text = String::from_utf8_lossy(&text).into_owned();
    import_text(&text, store)
}

/// [`import`] of names the user pasted instead of picking a file; returns how
/// many names were added.
pub fn import_text(list: &str, store: &Path) -> Result<usize, crate::Error> {
    let mut names = load(store);
    let before = names.len();
    for name in normalize(list) {
        if !names.iter().any(|n| n == &name) {
            names.push(name);
        }
    }
    let added = names.len() - before;
    if added > 0 {
        save(store, &names)?;
    }
    Ok(added)
}

/// The names of the store, learnt by a [`Names`] so hashes resolve to them.
pub fn seed(names: &mut Names, path: &Path) {
    for name in load(path) {
        names.add(&name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The built-in names are on every run; a store name is identifiable by "_x".
    fn seeded() -> (Vec<String>, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("names.txt");
        let names = vec!["Name_one".into(), "Name_two".into(), "Name_three".into()];
        save(&store, &names).unwrap();
        (names, store, dir)
    }

    #[test]
    fn skips_empty_comment_and_duplicate_lines() {
        let names = normalize(" Name_one \n\n# a comment\nName_two\nName_one\n\t\n");
        assert_eq!(names, ["Name_one", "Name_two"]);
    }

    #[test]
    fn no_store_file_means_no_names() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("names.txt")).is_empty());
    }

    #[test]
    fn import_dedupes_within_the_list_and_against_the_store() {
        let (names, store, _dir) = seeded();
        let list = "Name_two\n New_name \nName_two\n# comment\nAnother\n";
        assert_eq!(import_text(list, &store).unwrap(), 2);
        assert_eq!(load(&store), vec!["Name_one", "Name_two", "Name_three", "New_name", "Another"]);
        assert_eq!(import_text(" New_name ", &store).unwrap(), 0);
        // The store file was replaced, not appended to a stale copy.
        assert_eq!(load(&store).len(), 5);
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn a_store_name_resolves_through_names() {
        use paleto_formats::hash::joaat;
        let (_names, store, _dir) = seeded();
        let mut learned = Names::new();
        seed(&mut learned, &store);
        assert_eq!(learned.candidates(joaat(b"Name_one")), ["Name_one"]);
        assert!(learned.lookup(joaat(b"hash_12345678x")).is_none());
    }

    #[test]
    fn the_store_path_is_the_data_dir() {
        assert_eq!(path().file_name().and_then(|n| n.to_str()), Some("names.txt"));
    }
}
