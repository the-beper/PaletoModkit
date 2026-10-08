//! Edits a copy of a real archive and checks every other file is unchanged.
//!
//! cargo run --release -p paleto-rpf --example roundtrip -- <keys.bin> <archive.rpf> <work dir>

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use paleto_rpf::{Archive, ArchiveEditor, Keys};

fn contents(path: &Path, keys: &Keys) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let mut queue = vec![(String::new(), Archive::open(path, Some(keys)).unwrap())];
    while let Some((prefix, a)) = queue.pop() {
        for (p, i) in a.walk(0) {
            let e = &a.entries[i];
            if e.is_archive() {
                queue.push((format!("{prefix}{p}/"), a.open_child(i, Some(keys)).unwrap()));
            } else if !e.is_dir() {
                let data = a.read(i, Some(keys)).unwrap();
                out.insert(format!("{prefix}{p}"), sha(&data));
            }
        }
    }
    out
}

fn sha(data: &[u8]) -> Vec<u8> {
    use sha1::Digest;
    sha1::Sha1::digest(data).to_vec()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap();
    let src = Path::new(&a[2]);
    let copy = Path::new(&a[3]).join(src.file_name().unwrap());
    std::fs::copy(src, &copy).unwrap();

    let before = contents(src, &keys);
    let first_nested = Archive::open(src, Some(&keys)).unwrap();
    let nested = first_nested.walk(0).into_iter().find(|(_, i)| first_nested.entries[*i].is_archive()).map(|(p, _)| p).unwrap();
    println!("{} files; editing nested {nested}", before.len());

    let t = Instant::now();
    let mut ed = ArchiveEditor::open(&copy, Some(&keys)).unwrap();
    ed.add_file("paleto/test.txt", b"added by paleto".to_vec()).unwrap();
    ed.archive(&nested, false, Some(&keys)).unwrap().add_file("paleto_inner.txt", b"inner".to_vec()).unwrap();
    ed.save(&copy).unwrap();
    println!("saved in {:?}", t.elapsed());

    let mut after = contents(&copy, &keys);
    assert_eq!(after.remove("paleto/test.txt").unwrap(), sha(b"added by paleto"));
    assert_eq!(after.remove(&format!("{nested}/paleto_inner.txt")).unwrap(), sha(b"inner"));
    let diff: Vec<_> = before.keys().filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(diff.is_empty(), "changed: {diff:?}");
    assert_eq!(before.len(), after.len());
    println!("ok: all {} original files identical; size {} -> {}", before.len(),
        std::fs::metadata(src).unwrap().len(), std::fs::metadata(&copy).unwrap().len());
}
