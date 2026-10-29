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

/// The archive at `path` as a save cut short before its table of contents
/// would leave it: the table of contents of `before` over the new data.
fn cut_before_toc(path: &Path, before: &[u8]) -> std::path::PathBuf {
    let word = |i: usize| u32::from_le_bytes(before[i..i + 4].try_into().unwrap()) as usize;
    let old_toc = (16 + 16 * word(4) + word(8)).div_ceil(512) * 512;
    let mut cut = std::fs::read(path).unwrap();
    cut[..old_toc].copy_from_slice(&before[..old_toc]);
    let cut_path = path.with_extension("cut.rpf");
    std::fs::write(&cut_path, cut).unwrap();
    cut_path
}

#[test]
fn nested_edited_in_place_when_it_fits() {
    let dir = tempfile::tempdir().unwrap();
    // "z_inner.rpf" sorts after "a.bin", so it is stored last with room to grow.
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    let before = std::fs::read(&path).unwrap();
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.archive("z_inner.rpf", false, None).unwrap().add_file("small.txt", b"hi".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let growth = std::fs::metadata(&path).unwrap().len() - before.len() as u64;
    assert!(growth <= 2048, "grew by {growth}");
    assert_eq!(read(&path, "z_inner.rpf/small.txt"), b"hi");
    assert!(read(&path, "z_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&path, "a.bin") == noise(100_000, 1));
    // Cut short before the outer table of contents: the nested archive already
    // shows its new content, and everything reads.
    let cut = cut_before_toc(&path, &before);
    assert_eq!(read(&cut, "z_inner.rpf/small.txt"), b"hi");
    assert!(read(&cut, "z_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&cut, "a.bin") == noise(100_000, 1));
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
fn nested_growth_never_overwrites_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outer.rpf");
    let mut ed = ArchiveEditor::new("outer.rpf");
    // Stored in name order: big nested archive, then a small file right after it.
    ed.archive("a_inner.rpf", true, None).unwrap().add_file("big.bin", noise(300_000, 2)).unwrap();
    ed.add_file("b_small.bin", noise(5_000, 1)).unwrap();
    ed.archive("c_other.rpf", true, None).unwrap().add_file("x.bin", noise(20_000, 4)).unwrap();
    ed.save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();

    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    // Growing a_inner would need the space of b_small and of c_other: it is rebuilt instead.
    ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(10_000, 3)).unwrap();
    ed.save(&path).unwrap();
    let growth = std::fs::metadata(&path).unwrap().len() - before.len() as u64;
    assert!(growth > 300_000, "grew by {growth}: not rebuilt");
    assert!(read(&path, "a_inner.rpf/more.bin") == noise(10_000, 3));
    assert!(read(&path, "a_inner.rpf/big.bin") == noise(300_000, 2));
    assert!(read(&path, "b_small.bin") == noise(5_000, 1));
    assert!(read(&path, "c_other.rpf/x.bin") == noise(20_000, 4));

    // A save cut short before its table of contents still reads as the old archive.
    let cut_path = cut_before_toc(&path, &before);
    assert!(read(&cut_path, "a_inner.rpf/big.bin") == noise(300_000, 2));
    assert!(read(&cut_path, "b_small.bin") == noise(5_000, 1));
    assert!(read(&cut_path, "c_other.rpf/x.bin") == noise(20_000, 4));
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
fn scripts() {
    // Not in a plain archive: they would have to be NG-encrypted.
    let mut ed = ArchiveEditor::new("a.rpf");
    assert!(matches!(ed.add_file("main.ysc", resource(100)), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(!ed.exists("main.ysc"));

    // In an AES archive, encrypted with its key: they read back as added.
    let keys = test_keys();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    std::fs::write(&path, aes_archive(dir.path(), &keys, b"x")).unwrap();
    let mut ed = ArchiveEditor::open(&path, Some(&keys)).unwrap();
    ed.add_file("main.ysc", resource(100)).unwrap();
    ed.add_file("other.ytd", resource(100)).unwrap();
    assert_eq!(ed.read("main.ysc", Some(&keys)).unwrap(), resource(100));
    // Scripts are encrypted by name, and resources carry no flag saying so: never renamed.
    assert!(matches!(ed.rename("main.ysc", "x.ysc", None), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(matches!(ed.rename("other.ytd", "x.ysc", None), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(ed.exists("main.ysc") && ed.exists("other.ytd"));
    ed.save(&path).unwrap();
    let archive = Archive::open(&path, Some(&keys)).unwrap();
    assert_eq!(archive.read(archive.find("main.ysc").unwrap(), Some(&keys)).unwrap(), resource(100));
}

#[test]
fn stored_files_are_copied_as_they_are() {
    use paleto_rpf::StoredFile;
    let keys = test_keys();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    std::fs::write(&path, aes_archive(dir.path(), &keys, b"x")).unwrap();
    let mut ed = ArchiveEditor::open(&path, Some(&keys)).unwrap();
    ed.add_file("main.ysc", resource(100)).unwrap();
    ed.add_file("b.txt", b"plain text ".repeat(10)).unwrap();
    let script = ed.stored("main.ysc").unwrap();

    // Kept outside an archive and read back; anything else is not one.
    let bytes = script.to_bytes();
    let script = StoredFile::from_bytes(&bytes).unwrap().unwrap();
    assert!(StoredFile::from_bytes(&resource(100)).unwrap().is_none());
    assert!(StoredFile::from_bytes(&bytes[..bytes.len() - 1]).is_err());

    // Encryption is tied to the archive and the name.
    let mut plain = ArchiveEditor::new("p.rpf");
    assert!(matches!(plain.add_stored("main.ysc", script.clone()), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(matches!(ed.add_stored("other.ysc", script.clone()), Err(paleto_rpf::Error::Unsupported(_))));
    ed.add_stored("main.ysc", script).unwrap();
    // A plain file goes anywhere, under any name.
    plain.add_stored("c.txt", ed.stored("b.txt").unwrap()).unwrap();
    assert_eq!(plain.read("c.txt", None).unwrap(), b"plain text ".repeat(10));

    ed.save(&path).unwrap();
    let archive = Archive::open(&path, Some(&keys)).unwrap();
    assert_eq!(archive.read(archive.find("main.ysc").unwrap(), Some(&keys)).unwrap(), resource(100));
    let copied = archive.stored(archive.find("main.ysc").unwrap()).unwrap();
    assert_eq!(copied.to_bytes(), StoredFile::from_bytes(&bytes).unwrap().unwrap().to_bytes());
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
fn an_entry_without_a_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("zz", b"x".to_vec()).unwrap();
    ed.add_file("d/yy", b"y".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let clean = std::fs::read(&path).unwrap();
    // The root has no name and is fine.
    assert!(Archive::open(&path, None).is_ok());
    // A file, then a folder, with its name emptied: the archive is damaged, not one that extracts to "/".
    let names = 16 + 16 * u32::from_le_bytes(clean[4..8].try_into().unwrap()) as usize;
    for name in [&b"zz\0"[..], b"d\0", b"yy\0"] {
        let at = names + clean[names..].windows(name.len()).position(|w| w == name).unwrap();
        let mut data = clean.clone();
        data[at] = 0;
        std::fs::write(&path, data).unwrap();
        assert!(matches!(Archive::open(&path, None), Err(paleto_rpf::Error::Corrupt(_))), "{}", String::from_utf8_lossy(name));
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

/// Keys for archives made by the tests. The NG tables only move bytes, so with
/// them NG decryption is a fixed shuffle of each block XOR a pattern made by
/// the key, which [`encrypt_ng`] undoes.
fn test_keys() -> paleto_rpf::Keys {
    let mut tables = Box::new([[[0u32; 256]; 16]; 17]);
    for round in tables.iter_mut() {
        for (i, table) in round.iter_mut().enumerate() {
            for (b, v) in table.iter_mut().enumerate() {
                *v = (b as u32) << (8 * (i % 4));
            }
        }
    }
    let ng_keys = (0..101u32).map(|k| std::array::from_fn(|w| k.wrapping_mul(0x9E37_79B9) ^ w as u32)).collect();
    paleto_rpf::Keys { aes: [7; 32], ng_keys, ng_tables: tables, lut: std::array::from_fn(|b| b as u8), awc: None }
}

/// The reverse of `decrypt_ng` with the tables of [`test_keys`].
fn encrypt_ng(keys: &paleto_rpf::Keys, key: &paleto_rpf::keys::NgKey, data: &mut [u8]) {
    use paleto_rpf::crypto::decrypt_ng;
    let mut shuffle: [u8; 16] = std::array::from_fn(|i| i as u8 + 1);
    decrypt_ng(&keys.ng_tables, &[0; 68], &mut shuffle);
    let mut pattern = [0u8; 16];
    decrypt_ng(&keys.ng_tables, key, &mut pattern);
    for block in data.as_chunks_mut::<16>().0 {
        let plain = *block;
        for j in 0..16 {
            block[shuffle[j] as usize - 1] = plain[j] ^ pattern[j];
        }
    }
}

#[test]
fn renamed_ng_archive_stays_readable() {
    use paleto_rpf::crypto::ng_key;
    let keys = test_keys();
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner.rpf");
    let mut ed = ArchiveEditor::new("inner.rpf");
    ed.add_file("f.txt", b"hello".to_vec()).unwrap();
    ed.save(&inner).unwrap();
    // Its table of contents encrypted with the NG key of its name and size.
    let mut data = std::fs::read(&inner).unwrap();
    let word = |data: &[u8], i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
    // Header: magic, entry count, names length, encryption.
    let (toc_end, names_len) = (16 + 16 * word(&data, 4), word(&data, 8));
    let key = ng_key(&keys, "inner.rpf", data.len() as u32);
    data[12..16].copy_from_slice(&0x0FEF_FFFFu32.to_le_bytes());
    encrypt_ng(&keys, key, &mut data[16..toc_end]);
    encrypt_ng(&keys, key, &mut data[toc_end..toc_end + names_len]);

    let path = dir.path().join("parent.rpf");
    let mut ed = ArchiveEditor::new("parent.rpf");
    ed.add_file("inner.rpf", data).unwrap();
    ed.save(&path).unwrap();
    let read = |name: &str| {
        let outer = Archive::open(&path, Some(&keys)).unwrap();
        let inner = outer.open_child(outer.find(name).unwrap(), Some(&keys)).unwrap();
        inner.read(inner.find("f.txt").unwrap(), Some(&keys)).unwrap()
    };
    assert_eq!(read("inner.rpf"), b"hello");

    let mut ed = ArchiveEditor::open(&path, Some(&keys)).unwrap();
    // Without keys it cannot be written again: refused, and left as it was.
    assert!(matches!(ed.rename("inner.rpf", "renamed.rpf", None), Err(paleto_rpf::Error::KeysRequired)));
    assert!(ed.exists("inner.rpf") && !ed.exists("renamed.rpf"));
    ed.rename("inner.rpf", "renamed.rpf", Some(&keys)).unwrap();
    ed.save(&path).unwrap();
    assert_eq!(read("renamed.rpf"), b"hello");
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

#[test]
fn clashing_names_are_never_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    // Names that differ only in case, and a file named like a folder: such an
    // archive reads, but is not edited (saving would lose one of them).
    // The file named like the folder comes after it in the archive, then before it.
    for (patch, to) in [(b"\0bb.txt\0".as_slice(), b"\0AA.txt\0".as_slice()), (b"\0ee\0", b"\0dd\0"), (b"\0cc\0", b"\0dd\0")] {
        let mut ed = ArchiveEditor::new("a.rpf");
        for (name, data) in [("aa.txt", "a"), ("bb.txt", "b"), ("cc", "c"), ("dd/xx", "x"), ("ee", "e")] {
            ed.add_file(name, data.as_bytes().to_vec()).unwrap();
        }
        ed.save(&path).unwrap();
        let mut data = std::fs::read(&path).unwrap();
        let at = data.windows(patch.len()).position(|w| w == patch).unwrap();
        data[at..at + to.len()].copy_from_slice(to);
        std::fs::write(&path, data).unwrap();
        assert!(Archive::open(&path, None).is_ok());
        assert!(matches!(ArchiveEditor::open(&path, None), Err(paleto_rpf::Error::DuplicateName(_))), "{}", String::from_utf8_lossy(to));
    }

    // A file never replaces a folder, nor a folder a file.
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("dd/xx", b"x".to_vec()).unwrap();
    ed.add_file("ee", b"e".to_vec()).unwrap();
    assert!(matches!(ed.add_file("DD", b"d".to_vec()), Err(paleto_rpf::Error::AlreadyExists(_))));
    assert!(matches!(ed.create_dir("ee"), Err(paleto_rpf::Error::NotADirectory(_))));
    assert_eq!(ed.read("dd/xx", None).unwrap(), b"x");
    assert_eq!(ed.read("ee", None).unwrap(), b"e");
    ed.add_file("ee", b"new".to_vec()).unwrap();
    assert_eq!(ed.read("ee", None).unwrap(), b"new");
    // A nested archive open for editing is still replaced by a file.
    ed.archive("x.rpf", true, None).unwrap().add_file("a", b"a".to_vec()).unwrap();
    ed.add_file("x.rpf", b"plain".to_vec()).unwrap();
    assert_eq!(ed.read("x.rpf", None).unwrap(), b"plain");
}

/// Damaged archives give an error, never a hang, a panic or a huge allocation.
#[test]
fn malformed_archives_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rpf");
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.add_file("a.txt", vec![b'z'; 100_000]).unwrap(); // compressed
    ed.add_file("b.awc", noise(1000, 1)).unwrap(); // stored as is
    ed.add_file("d/x", b"x".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let clean = std::fs::read(&path).unwrap();
    // Entries in order: root, a.txt, b.awc, d, d/x; 16 bytes each after the header.
    let at = |i: usize, word: usize| 16 + 16 * i + 4 * word;
    let patched = |patches: &[(usize, u32)]| {
        let mut data = clean.clone();
        for &(offset, value) in patches {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        std::fs::write(&path, data).unwrap();
        Archive::open(&path, None)
    };
    fn corrupt<T>(r: Result<T, paleto_rpf::Error>) -> bool {
        matches!(r, Err(paleto_rpf::Error::Corrupt(_)))
    }

    assert!(corrupt(patched(&[(at(0, 2), 0)])), "root inside itself");
    assert!(corrupt(patched(&[(at(3, 2), 0)])), "folder holding the root");
    assert!(corrupt(patched(&[(4, 0x0FFF_FFFF)])), "huge entry count");
    assert!(corrupt(patched(&[(8, 0xFFFF_FFF0)])), "huge names length");
    // The root also holding d/x (entries 1 to 4): reached twice.
    assert!(corrupt(patched(&[(at(0, 3), 4)])), "entry in two folders");
    assert!(corrupt(patched(&[(at(3, 3), 9)])), "folder past the last entry");

    let read = |archive: Result<Archive, paleto_rpf::Error>, name: &str| {
        let archive = archive.unwrap();
        archive.read(archive.find(name).unwrap(), None)
    };
    // b.awc (stored as is): its size, then its place, far past the end.
    assert!(corrupt(read(patched(&[(at(2, 2), 0xFFFF_FFF0)]), "b.awc")));
    let lo = u32::from_le_bytes(clean[at(2, 1)..at(2, 1) + 4].try_into().unwrap());
    assert!(corrupt(read(patched(&[(at(2, 1), lo | 0x7FFF_0000)]), "b.awc")));
    // a.txt says it inflates to 10 bytes: no more is made.
    assert_eq!(read(patched(&[(at(1, 2), 10)]), "a.txt").unwrap(), vec![b'z'; 10]);
    assert_eq!(read(patched(&[]), "a.txt").unwrap(), vec![b'z'; 100_000]);

    // A nested archive whose data is the archive holding it would nest forever.
    let mut ed = ArchiveEditor::new("a.rpf");
    ed.archive("x.rpf", true, None).unwrap().add_file("f", b"f".to_vec()).unwrap();
    ed.save(&path).unwrap();
    let mut data = std::fs::read(&path).unwrap();
    let lo = u64::from_le_bytes(data[at(1, 0)..at(1, 0) + 8].try_into().unwrap());
    data[at(1, 0)..at(1, 0) + 8].copy_from_slice(&(lo & 0xFF_FFFF_FFFF).to_le_bytes()); // offset 0
    std::fs::write(&path, data).unwrap();
    assert!(corrupt(Archive::open(&path, None)));
}

// Saving never writes the archive that was opened: it stages a new file.

/// Offsets (in blocks) of the files in the archive at `path`, by path.
fn offsets(path: &Path, keys: Option<&paleto_rpf::Keys>) -> std::collections::BTreeMap<String, u32> {
    let a = Archive::open(path, keys).unwrap();
    a.walk(0).into_iter().filter_map(|(p, i)| a.entries[i].offset().map(|o| (p, o))).collect()
}

/// Names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Saves the archive at `path` to itself after `edit`, and checks that the old
/// inode (kept by a hard link) was not written, that the path holds the new
/// archive with the same permission bits, that nothing else is left in the
/// folder and that the files in `untouched` kept their place.
fn staged_save(path: &Path, keys: Option<&paleto_rpf::Keys>, untouched: &[&str], edit: impl FnOnce(&mut ArchiveEditor)) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = path.parent().unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let keep = dir.join("old-inode.keep");
    std::fs::hard_link(path, &keep).unwrap();
    let before = std::fs::read(path).unwrap();
    let before_offsets = offsets(path, keys);
    let old_ino = std::fs::metadata(path).unwrap().ino();

    let mut ed = ArchiveEditor::open(path, keys).unwrap();
    edit(&mut ed);
    ed.save(path).unwrap();

    assert_eq!(std::fs::read(&keep).unwrap(), before, "the old inode was written");
    assert_eq!(std::fs::metadata(&keep).unwrap().ino(), old_ino);
    let new = std::fs::metadata(path).unwrap();
    assert_ne!(new.ino(), old_ino, "saved in place");
    assert_eq!(new.mode() & 0o777, 0o640);
    assert_ne!(std::fs::read(path).unwrap(), before);
    let mut expected = vec!["old-inode.keep".to_string(), path.file_name().unwrap().to_string_lossy().into_owned()];
    expected.sort();
    assert_eq!(names(dir), expected);
    let after_offsets = offsets(path, keys);
    for name in untouched {
        assert_eq!(after_offsets[*name], before_offsets[*name], "{name} moved");
    }
}

#[test]
fn saving_to_its_own_path_never_writes_the_old_file() {
    // The nested archive grows in the space after it.
    let dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    staged_save(&path, None, &["a.bin"], |ed| {
        ed.archive("z_inner.rpf", false, None).unwrap().add_file("small.txt", b"hi".to_vec()).unwrap();
        ed.add_file("top.txt", b"top".to_vec()).unwrap();
    });
    assert_eq!(read(&path, "z_inner.rpf/small.txt"), b"hi");
    assert_eq!(read(&path, "top.txt"), b"top");
    assert!(read(&path, "z_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&path, "a.bin") == noise(100_000, 1));

    // The nested archive is blocked by a file after it: it is rebuilt.
    let dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "a_inner.rpf", "z.bin");
    staged_save(&path, None, &["z.bin"], |ed| {
        ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(50_000, 3)).unwrap();
    });
    assert!(read(&path, "a_inner.rpf/more.bin") == noise(50_000, 3));
    assert!(read(&path, "a_inner.rpf/big.bin") == noise(100_000, 2));
    assert!(read(&path, "z.bin") == noise(100_000, 1));

    // Plain edits: replace and grow.
    let dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    staged_save(&path, None, &["z_inner.rpf"], |ed| {
        ed.add_file("a.bin", b"small".to_vec()).unwrap();
        ed.add_file("b.bin", noise(70_000, 5)).unwrap();
    });
    assert_eq!(read(&path, "a.bin"), b"small");
    assert!(read(&path, "b.bin") == noise(70_000, 5));
    assert!(read(&path, "z_inner.rpf/big.bin") == noise(100_000, 2));
}

#[test]
fn aes_archive_saved_to_its_own_path_never_writes_the_old_file() {
    let keys = test_keys();
    let dir = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..64).collect();
    let path = dir.path().join("a.rpf");
    std::fs::write(&path, aes_archive(dir.path(), &keys, &content)).unwrap();
    std::fs::remove_file(dir.path().join("aes.rpf")).unwrap();
    staged_save(&path, Some(&keys), &["a.awc"], |ed| ed.add_file("b.txt", b"b".to_vec()).unwrap());
    let archive = Archive::open(&path, Some(&keys)).unwrap();
    assert_eq!(archive.encryption, Encryption::Aes);
    assert_eq!(archive.read(archive.find("a.awc").unwrap(), Some(&keys)).unwrap(), content);
    assert_eq!(archive.read(archive.find("b.txt").unwrap(), Some(&keys)).unwrap(), b"b");
}

fn new_file(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(path).unwrap()
}

#[test]
fn save_to_edits_a_copy_and_never_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let scratch_dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    let before = std::fs::read(&path).unwrap();
    let before_offsets = offsets(&path, None);

    let mut ed = ArchiveEditor::open_file(std::fs::File::open(&path).unwrap(), "outer.rpf", None).unwrap();
    assert_eq!(ed.name(), "outer.rpf");
    ed.archive("z_inner.rpf", false, None).unwrap().add_file("small.txt", b"hi".to_vec()).unwrap();
    ed.add_file("top.txt", b"top".to_vec()).unwrap();
    let out_path = scratch_dir.path().join("staged.bin");
    let len = ed.save_to(&new_file(&out_path), &mut || tempfile::tempfile_in(scratch_dir.path())).unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(len, std::fs::metadata(&out_path).unwrap().len());
    // Edited as a copy, like an in-place save: untouched data is where it was.
    assert_eq!(offsets(&out_path, None)["a.bin"], before_offsets["a.bin"]);
    assert_eq!(read(&out_path, "z_inner.rpf/small.txt"), b"hi");
    assert_eq!(read(&out_path, "top.txt"), b"top");
    assert!(read(&out_path, "a.bin") == noise(100_000, 1));
    assert!(read(&out_path, "z_inner.rpf/big.bin") == noise(100_000, 2));

    // A rebuilt nested archive uses the scratch files, and the source stays as it is.
    let path = with_nested(dir.path(), "a_inner.rpf", "z.bin");
    let before = std::fs::read(&path).unwrap();
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(50_000, 3)).unwrap();
    let out_path = scratch_dir.path().join("staged2.bin");
    let mut asked = 0;
    ed.save_to(&new_file(&out_path), &mut || {
        asked += 1;
        tempfile::tempfile_in(scratch_dir.path())
    })
    .unwrap();
    assert!(asked > 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(read(&out_path, "a_inner.rpf/more.bin") == noise(50_000, 3));
    assert!(read(&out_path, "z.bin") == noise(100_000, 1));

    // A scratch file that cannot be made fails the save; the source is untouched.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.archive("a_inner.rpf", false, None).unwrap().add_file("more.bin", noise(50_000, 3)).unwrap();
    let out = new_file(&scratch_dir.path().join("staged3.bin"));
    let failed = ed.save_to(&out, &mut || Err(std::io::Error::other("no scratch space")));
    assert!(matches!(failed, Err(paleto_rpf::Error::Io(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // The file to save into must be empty.
    let ed = ArchiveEditor::open(&path, None).unwrap();
    let full = new_file(&scratch_dir.path().join("full.bin"));
    std::io::Write::write_all(&mut &full, b"x").unwrap();
    assert!(ed.save_to(&full, &mut || tempfile::tempfile_in(scratch_dir.path())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_new_archive_is_written_fresh_by_save_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = ArchiveEditor::new("n.rpf");
    ed.add_file("a.txt", b"a".to_vec()).unwrap();
    let out_path = dir.path().join("n.rpf");
    let len = ed.save_to(&new_file(&out_path), &mut || tempfile::tempfile_in(dir.path())).unwrap();
    assert_eq!(len, std::fs::metadata(&out_path).unwrap().len());
    assert_eq!(read(&out_path, "a.txt"), b"a");
}

#[test]
fn saving_over_a_symlink_replaces_the_link_not_its_target() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("real")).unwrap();
    let target = with_nested(&dir.path().join("real"), "z_inner.rpf", "a.bin");
    let link = dir.path().join("link.rpf");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let before = std::fs::read(&target).unwrap();
    let target_ino = std::fs::metadata(&target).unwrap().ino();

    // Opened through the link, so it is edited as a copy; the link is replaced.
    let mut ed = ArchiveEditor::open(&link, None).unwrap();
    ed.add_file("top.txt", b"top".to_vec()).unwrap();
    ed.save(&link).unwrap();

    assert!(std::fs::symlink_metadata(&link).unwrap().is_file());
    assert_eq!(read(&link, "top.txt"), b"top");
    assert!(read(&link, "a.bin") == noise(100_000, 1));
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert_eq!(std::fs::metadata(&target).unwrap().ino(), target_ino);
    assert_eq!(names(&dir.path().join("real")), [target.file_name().unwrap().to_string_lossy().into_owned()]);
    assert_eq!(names(dir.path()), ["link.rpf", "real"]);

    // A link that leads nowhere is replaced too, and nothing is written through it.
    let dangling = dir.path().join("dangling.rpf");
    let nowhere = dir.path().join("nowhere.rpf");
    std::os::unix::fs::symlink(&nowhere, &dangling).unwrap();
    let mut ed = ArchiveEditor::new("d.rpf");
    ed.add_file("a.txt", b"a".to_vec()).unwrap();
    ed.save(&dangling).unwrap();
    assert!(!nowhere.exists());
    assert!(std::fs::symlink_metadata(&dangling).unwrap().is_file());
    assert_eq!(read(&dangling, "a.txt"), b"a");

    // The permission bits are those of the link's target, and a read-only one is refused.
    use std::os::unix::fs::PermissionsExt;
    let (real, link2) = (dir.path().join("ro.rpf"), dir.path().join("ro-link.rpf"));
    std::fs::write(&real, b"x").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::os::unix::fs::symlink(&real, &link2).unwrap();
    let ed = ArchiveEditor::new("n.rpf");
    assert!(matches!(ed.save(&link2), Err(paleto_rpf::Error::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied));
    assert!(std::fs::symlink_metadata(&link2).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&real).unwrap(), b"x");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    ArchiveEditor::new("n.rpf").save(&link2).unwrap();
    assert_eq!(std::fs::symlink_metadata(&link2).unwrap().mode() & 0o777, 0o640);
    assert_eq!(std::fs::read(&real).unwrap(), b"x");
}

#[test]
fn a_read_only_archive_is_opened_but_not_saved_over() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    assert_eq!(ed.read("a.bin", None).unwrap(), noise(100_000, 1));
    ed.add_file("top.txt", b"top".to_vec()).unwrap();
    match ed.save(&path) {
        Err(paleto_rpf::Error::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
        other => panic!("{:?}", other.err()),
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o444);
    assert_eq!(names(dir.path()), [path.file_name().unwrap().to_string_lossy().into_owned()]);
}

#[test]
fn a_failed_save_leaves_the_archive_and_the_folder_as_they_were() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = with_nested(dir.path(), "z_inner.rpf", "a.bin");
    let before = std::fs::read(&path).unwrap();
    let only = names(dir.path());

    // The staging file cannot be made in a folder that is not writable.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.add_file("top.txt", b"top".to_vec()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    // (Running as root, a folder is always writable: nothing to check then.)
    let writable_anyway = std::fs::File::create(dir.path().join("probe")).is_ok();
    let result = ed.save(&path);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable_anyway {
        std::fs::remove_file(dir.path().join("probe")).unwrap();
    } else {
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(names(dir.path()), only);
    }

    // An error after the staging file exists: its names table is too large.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    for i in 0..3000 {
        ed.add_file(&format!("many/a_rather_long_file_name_for_the_table_{i:05}.txt"), vec![1]).unwrap();
    }
    assert!(matches!(ed.save(&path), Err(paleto_rpf::Error::Unsupported(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(names(dir.path()), only);
}

#[test]
fn files_added_from_disk_are_not_read_into_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut inner = ArchiveEditor::new("inner.rpf");
    inner.add_file("a/b.xml", b"<x/>".to_vec()).unwrap();
    let inner_path = dir.path().join("inner.bin");
    inner.save(&inner_path).unwrap();
    let mut bytes = std::fs::read(&inner_path).unwrap();
    let len = bytes.len() as u64;
    bytes.extend_from_slice(b"trailing bytes that are not part of it");
    std::fs::write(&inner_path, &bytes).unwrap();
    let open = || std::fs::File::open(&inner_path).unwrap();

    let mut ed = ArchiveEditor::new("outer.rpf");
    ed.add_file_from("sub/Inner.RPF", open(), len).unwrap();
    ed.add_file("t.txt", b"t".to_vec()).unwrap();
    // Not stored as is: refused, and nothing is added.
    assert!(matches!(ed.add_file_from("sub/x.txt", open(), len), Err(paleto_rpf::Error::Unsupported(_))));
    assert!(!ed.exists("sub/x.txt"));
    // Longer than the file, or too large for the entry: refused.
    assert!(matches!(ed.add_file_from("sub/long.rpf", open(), bytes.len() as u64 + 1), Err(paleto_rpf::Error::Io(_))));
    assert!(matches!(ed.add_file_from("sub/huge.awc", open(), 1 << 32), Err(paleto_rpf::Error::TooLarge(_))));
    assert!(!ed.exists("sub/long.rpf") && !ed.exists("sub/huge.awc"));
    let path = dir.path().join("outer.rpf");
    ed.save(&path).unwrap();
    assert_eq!(read(&path, "sub/inner.rpf/a/b.xml"), b"<x/>");

    // Into an archive that was opened (edited as a copy), and into a nested one.
    let mut ed = ArchiveEditor::open(&path, None).unwrap();
    ed.add_file_from("sub/second.rpf", open(), len).unwrap();
    ed.archive("sub/inner.rpf", false, None).unwrap().add_file_from("deep.awc", open(), 5).unwrap();
    ed.save(&path).unwrap();
    assert_eq!(read(&path, "sub/second.rpf/a/b.xml"), b"<x/>");
    assert_eq!(read(&path, "sub/inner.rpf/a/b.xml"), b"<x/>");
    assert_eq!(read(&path, "sub/inner.rpf/deep.awc"), &bytes[..5]);
    assert_eq!(read(&path, "t.txt"), b"t");
}
