use std::path::Path;

use paleto_rpf::{Archive, ArchiveEditor, Encryption, EntryKind};

/// Reads `inner` (which may go through nested archives) from the archive at `path`.
fn read(path: &Path, inner: &str) -> Vec<u8> {
    let mut archive = Archive::open(path, None).unwrap();
    let mut cur = 0;
    for part in inner.split('/') {
        if archive.entries[cur].is_archive() {
            archive = archive.open_child(cur, None).unwrap();
            cur = 0;
        }
        cur = archive.children(cur).find(|&i| archive.entries[i].name == part).unwrap_or_else(|| panic!("{inner}: {part} missing"));
    }
    archive.read(cur, None).unwrap()
}

fn resource(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    data[..4].copy_from_slice(b"RSC7");
    data[4..8].copy_from_slice(&13u32.to_le_bytes());
    data[8..12].copy_from_slice(&0x0000_0001u32.to_le_bytes());
    data[12..16].copy_from_slice(&0xD000_0002u32.to_le_bytes()); // version 13 = 0x0D
    for (i, b) in data[16..].iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    data
}

#[test]
fn create_edit_nested_defragment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.rpf");
    let text = b"hello world ".repeat(1000);

    let mut ed = ArchiveEditor::new("test.rpf");
    ed.add_file("data/Hello.TXT", text.clone()).unwrap();
    ed.add_file("data/zz.bin", vec![1, 2, 3]).unwrap();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    ed.add_file("data/noise.bin", noise).unwrap();
    ed.add_file("models/car.ytd", resource(5000)).unwrap();
    ed.archive("levels/inner.rpf", true, None).unwrap().add_file("a/b.xml", b"<x/>".to_vec()).unwrap();
    ed.save(&path).unwrap();

    let a = Archive::open(&path, None).unwrap();
    assert_eq!(a.encryption, Encryption::Open);
    let names: Vec<_> = a.walk(0).into_iter().map(|(p, _)| p).collect();
    assert_eq!(names, ["data", "data/hello.txt", "data/noise.bin", "data/zz.bin", "levels", "levels/inner.rpf", "models", "models/car.ytd"]);
    assert_eq!(read(&path, "data/hello.txt"), text);
    assert!(read(&path, "models/car.ytd") == resource(5000));
    assert_eq!(read(&path, "levels/inner.rpf/a/b.xml"), b"<x/>");
    let idx = a.find("data/hello.txt").unwrap();
    assert!(matches!(a.entries[idx].kind, EntryKind::Binary { size, .. } if size > 0 && (size as usize) < text.len()));
    let size_before = std::fs::metadata(&path).unwrap().len();

    // In-place edit: replace, delete, add, edit the nested archive.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.add_file("data/hello.txt", b"replaced".to_vec()).unwrap();
    ed.add_file("data/noise.bin", b"small now".to_vec()).unwrap();
    assert!(ed.delete("data/zz.bin").unwrap());
    assert!(!ed.delete("data/missing").unwrap());
    let inner = ed.archive("levels/inner.rpf", false, None).unwrap();
    inner.add_file("c.txt", b"inner".to_vec()).unwrap();
    for i in 0..300 {
        ed.add_file(&format!("many/file_with_a_long_name_{i:04}.dat"), vec![i as u8; 10]).unwrap();
    }
    ed.save(&path).unwrap();

    assert_eq!(read(&path, "data/hello.txt"), b"replaced");
    assert!(read(&path, "models/car.ytd") == resource(5000));
    assert_eq!(read(&path, "levels/inner.rpf/a/b.xml"), b"<x/>");
    assert_eq!(read(&path, "levels/inner.rpf/c.txt"), b"inner");
    assert_eq!(read(&path, "many/file_with_a_long_name_0299.dat"), vec![43u8; 10]);
    let a = Archive::open(&path, None).unwrap();
    assert!(a.find("data/zz.bin").is_none());
    let grown = std::fs::metadata(&path).unwrap().len();
    assert!(grown > size_before);

    // Defragmenting drops the dead space left by in-place edits.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.defragment();
    ed.save(&path).unwrap();
    let defragged = std::fs::metadata(&path).unwrap().len();
    assert!(defragged < grown, "{size_before} -> {grown} -> {defragged}");
    assert_eq!(read(&path, "levels/inner.rpf/c.txt"), b"inner");
    assert!(read(&path, "models/car.ytd") == resource(5000));
}

#[test]
fn oversized_resource() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rpf");
    let big = resource(0x100_0100);
    let mut ed = ArchiveEditor::new("big.rpf");
    ed.add_file("big.ydr", big.clone()).unwrap();
    ed.save(&path).unwrap();
    let a = Archive::open(&path, None).unwrap();
    let idx = a.find("big.ydr").unwrap();
    assert_eq!(a.entries[idx].size(), big.len() as u64);
    assert!(a.read(idx, None).unwrap() == big);
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Builds an archive with a nested archive (holding 100 KB) next to a 100 KB file.
fn with_nested(dir: &Path, nested: &str, other: &str) -> std::path::PathBuf {
    let path = dir.join(format!("{nested}.rpf"));
    let mut ed = ArchiveEditor::new("outer.rpf");
    ed.add_file(other, noise(100_000, 1)).unwrap();
    ed.archive(nested, true, None).unwrap().add_file("big.bin", noise(100_000, 2)).unwrap();
    ed.save(&path).unwrap();
    path
}

#[test]
fn nested_edited_in_place_when_it_fits() {
    let dir = tempfile::tempdir().unwrap();
    // "z_inner.rpf" sorts after "a.bin", so it is stored last with room to grow.
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    let before = std::fs::metadata(&path).unwrap().len();
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.archive("z_inner.rpf", false, None).unwrap().add_file("small.txt", b"hi".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let growth = std::fs::metadata(&path).unwrap().len() - before;
    assert!(growth <= 2048, "grew by {growth}");
    assert_eq!(read(&path, "z_inner.rpf/small.txt"), b"hi");
    assert!(read(&path, "z_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&path, "a.bin") == noise(100_000, 1));
}

#[test]
fn nested_rebuilt_when_blocked() {
    let dir = tempfile::tempdir().unwrap();
    // "a_inner.rpf" is stored first, directly followed by "z.bin".
    let path = with_nested(dir.path(), "a_inner.rpf", "z.bin");
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(50_000, 3)).unwrap();
    ed.save(&path).unwrap();
    assert!(read(&path, "a_inner.rpf/more.bin") == noise(50_000, 3));
    assert!(read(&path, "a_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&path, "z.bin") == noise(100_000, 1));
}

#[test]
fn nested_grows_by_moving_small_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outer.rpf");
    let mut ed = ArchiveEditor::new("outer.rpf");
    // Stored in name order: big nested archive, then a small file right after it.
    ed.archive("a_inner.rpf", true, None).unwrap().add_file("big.bin", noise(300_000, 2)).unwrap();
    ed.add_file("b_small.bin", noise(5_000, 1)).unwrap();
    ed.archive("c_other.rpf", true, None).unwrap().add_file("x.bin", noise(20_000, 4)).unwrap();
    ed.save(&path).unwrap();
    let before = std::fs::metadata(&path).unwrap().len();

    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    // Growing a_inner needs the space of b_small and of the untouched c_other.
    ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(10_000, 3)).unwrap();
    ed.save(&path).unwrap();
    let growth = std::fs::metadata(&path).unwrap().len() - before;
    assert!(growth < 100_000, "grew by {growth}: the big nested archive was rebuilt");
    assert!(read(&path, "a_inner.rpf/more.bin") == noise(10_000, 3));
    assert!(read(&path, "a_inner.rpf/big.bin") == noise(300_000, 2));
    assert!(read(&path, "b_small.bin") == noise(5_000, 1));
    assert!(read(&path, "c_other.rpf/x.bin") == noise(20_000, 4));
}

#[test]
fn rename_and_create_folders() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("data/handling.meta", b"<handling/>".to_vec()).unwrap();
    ed.add_file("data/sub/deep.txt", b"deep".to_vec()).unwrap();
    ed.add_file("tex.ytd", resource(3000)).unwrap();
    ed.archive("inner.rpf", true, None).unwrap().add_file("x.txt", b"x".to_vec()).unwrap();
    ed.save(&path).unwrap();

    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.rename("data/handling.meta", "Handling_Old.meta", None).unwrap();
    ed.rename("data/sub", "renamed", None).unwrap();
    ed.rename("tex.ytd", "other.ytd", None).unwrap();
    ed.rename("inner.rpf", "outer.rpf", None).unwrap();
    ed.create_dir("empty").unwrap();
    ed.create_dir("data/new/nested").unwrap();
    ed.create_dir("empty").unwrap(); // already there: fine

    // Clashes, missing files and bad names are refused without changes.
    assert!(matches!(ed.rename("other.ytd", "data", None), Err(paleto_rpf::Error::AlreadyExists(_))));
    assert!(matches!(ed.rename("missing.txt", "x", None), Err(paleto_rpf::Error::NotFound(_))));
    assert!(ed.rename("other.ytd", "a/b", None).is_err());
    assert!(matches!(ed.create_dir("other.ytd/x"), Err(paleto_rpf::Error::NotADirectory(_))));
    ed.save(&path).unwrap();

    assert_eq!(
        ArchiveEditor::open(&path, None).unwrap().list(),
        [
            "data/", "data/handling_old.meta", "data/new/", "data/new/nested/", "data/renamed/", "data/renamed/deep.txt",
            "empty/", "other.ytd", "outer.rpf",
        ]
    );
    assert_eq!(read(&path, "data/handling_old.meta"), b"<handling/>");
    assert_eq!(read(&path, "data/renamed/deep.txt"), b"deep");
    assert_eq!(read(&path, "other.ytd"), resource(3000));
    assert_eq!(read(&path, "outer.rpf/x.txt"), b"x");
    let archive = Archive::open(&path, None).unwrap();
    let empty = archive.find("empty").unwrap();
    assert!(matches!(archive.entries[empty].kind, EntryKind::Directory { count: 0, .. }));
}

#[test]
fn scripts_are_not_renamed() {
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("main.ysc", resource(100)).unwrap();
    ed.add_file("other.ytd", resource(100)).unwrap();
    assert!(matches!(ed.rename("main.ysc", "x.ysc", None), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(matches!(ed.rename("other.ytd", "x.ysc", None), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(ed.exists("main.ysc") && ed.exists("other.ytd"));
}

#[test]
fn names_that_are_paths_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("zz", b"x".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let clean = std::fs::read(&path).unwrap();
    let at = clean.windows(3).position(|w| w == b"zz\0").unwrap();
    for bad in [b"..", b".\0", b"z/", b"\\z"] {
        let mut data = clean.clone();
        data[at..at + 2].copy_from_slice(bad);
        std::fs::write(&path, data).unwrap();
        assert!(matches!(Archive::open(&path, None), Err(paleto_rpf::Error::Corrupt(_))), "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn a_file_that_is_not_an_archive_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("bad.rpf", b"not an archive".to_vec()).unwrap();
    ed.add_file("res.rpf", resource(100)).unwrap();
    // Added in this session (bytes in memory), then read back from the saved file.
    for round in 0..2 {
        assert!(ed.archive("bad.rpf", false, None).is_err(), "round {round}");
        assert!(matches!(ed.archive("res.rpf", true, None), Err(paleto_rpf::Error::NotAnArchive(_))), "round {round}");
        ed.save(&path).unwrap();
        assert_eq!(read(&path, "bad.rpf"), b"not an archive");
        assert_eq!(read(&path, "res.rpf"), resource(100));
        ed = ArchiveEditor::open(&path, None).unwrap();
    }
}

/// Keys good enough for AES archives made by the tests.
fn test_keys() -> paleto_rpf::Keys {
    paleto_rpf::Keys { aes: [7; 32], ng_keys: vec![[0; 68]; 101], ng_tables: Box::new([[[0; 256]; 16]; 17]), lut: [0; 256], awc: None }
}

/// An AES archive holding `content` as an AES-encrypted `a.awc`, as the game has them.
fn aes_archive(dir: &Path, keys: &paleto_rpf::Keys, content: &[u8]) -> Vec<u8> {
    use paleto_rpf::crypto::encrypt_aes;
    let path = dir.join("aes.rpf");
    let mut ed = ArchiveEditor::new("aes.rpf");
    ed.add_file("a.awc", content.to_vec()).unwrap();
    ed.save(&path).unwrap();
    let mut data = std::fs::read(&path).unwrap();
    let word = |data: &[u8], i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
    let (toc_end, names_len) = (16 + 16 * word(&data, 4), word(&data, 8));
    // The second 16-byte entry (after the root folder): offset in blocks in
    // bits 40-63 of its first word, the encrypted flag in its last word.
    let entry = 32;
    data[entry + 12..entry + 16].copy_from_slice(&1u32.to_le_bytes());
    let offset = ((u64::from_le_bytes(data[entry..entry + 8].try_into().unwrap()) >> 40) & 0xFF_FFFF) as usize * 512;
    encrypt_aes(&keys.aes, &mut data[offset..offset + content.len()]);
    data[12..16].copy_from_slice(&0x0FFF_FFF9u32.to_le_bytes());
    encrypt_aes(&keys.aes, &mut data[16..toc_end]);
    encrypt_aes(&keys.aes, &mut data[toc_end..toc_end + names_len]);
    data
}

#[test]
fn aes_archives_stay_readable() {
    let keys = test_keys();
    let dir = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..64).collect();
    let path = dir.path().join("a.rpf");
    std::fs::write(&path, aes_archive(dir.path(), &keys, &content)).unwrap();

    let read = |archive: &Archive, name: &str| {
        assert_eq!(archive.encryption, Encryption::Aes);
        archive.read(archive.find(name).unwrap(), Some(&keys)).unwrap()
    };
    assert_eq!(read(&Archive::open(&path, Some(&keys)).unwrap(), "a.awc"), content);
    let mut ed = ArchiveEditor::open(&path, Some(&keys)).unwrap();
    ed.add_file("b.txt", b"b".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let archive = Archive::open(&path, Some(&keys)).unwrap();
    assert_eq!((read(&archive, "a.awc"), read(&archive, "b.txt")), (content.clone(), b"b".to_vec()));

    // Nested in a plain archive, and edited there.
    let parent = dir.path().join("parent.rpf");
    let mut ed = ArchiveEditor::new("parent.rpf");
    ed.add_file("x.rpf", aes_archive(dir.path(), &keys, &content)).unwrap();
    ed.save(&parent).unwrap();
    let mut ed = ArchiveEditor::open(&parent, Some(&keys)).unwrap();
    ed.archive("x.rpf", false, Some(&keys)).unwrap().add_file("b.txt", b"b".to_vec()).unwrap();
    ed.save(&parent).unwrap();
    let outer = Archive::open(&parent, Some(&keys)).unwrap();
    let inner = outer.open_child(outer.find("x.rpf").unwrap(), Some(&keys)).unwrap();
    assert_eq!((read(&inner, "a.awc"), read(&inner, "b.txt")), (content, b"b".to_vec()));
}
