use std::path::Path;

use paleto_rpf::{Archive, ArchiveEditor};

fn read(path: &Path, name: &str) -> Option<Vec<u8>> {
    let archive = Archive::open(path, None).unwrap();
    let index = archive.children(0).find(|&i| archive.entries[i].name == name)?;
    Some(archive.read(index, None).unwrap())
}

/// Saving to the path an editor was opened from must build the new archive
/// aside and rename it: whoever holds the old file (a hard link, an open
/// handle) keeps a complete archive.
#[test]
#[ignore = "repro OIV-01: save() to the opened path rewrites the same inode in place, the hard-linked old archive changes"]
fn saving_over_the_opened_archive_leaves_the_old_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("old.txt", (0..5000u32).map(|i| (i % 251) as u8).collect()).unwrap();
    ed.save(&path).unwrap();
    let old_link = dir.path().join("old-link.rpf");
    std::fs::hard_link(&path, &old_link).unwrap();
    let before = std::fs::read(&path).unwrap();

    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.add_file("new.txt", vec![7u8; 3000]).unwrap();
    ed.save(&path).unwrap();

    assert!(std::fs::read(&old_link).unwrap() == before, "the old inode was modified in place");
    assert_eq!(read(&path, "new.txt").unwrap(), vec![7u8; 3000]);
    assert!(read(&path, "old.txt").is_some());
    let mut names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["a.rpf", "old-link.rpf"], "no temporary file left");
}
