use std::collections::BTreeMap;
use paleto_rpf::{Archive, EntryKind, Keys};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap();
    let mut raw: BTreeMap<String, usize> = BTreeMap::new();
    let mut comp: BTreeMap<String, usize> = BTreeMap::new();
    let mut enc: BTreeMap<String, usize> = BTreeMap::new();
    let mut upper = 0;
    for p in &a[2..] {
        let mut q = vec![(p.clone(), Archive::open(p.as_ref(), Some(&keys)).unwrap())];
        while let Some((name, ar)) = q.pop() {
            *enc.entry(format!("{:?}", ar.encryption)).or_default() += 1;
            for (path, i) in ar.walk(0) {
                let e = &ar.entries[i];
                if e.name.chars().any(|c| c.is_ascii_uppercase()) { upper += 1; if upper < 5 { println!("upper: {name}/{path}"); } }
                if let EntryKind::Binary { size, encrypted, .. } = e.kind {
                    let ext = e.name.rsplit('.').next().unwrap_or("").to_lowercase() + if encrypted {"(enc)"} else {""};
                    *if size == 0 { raw.entry(ext) } else { comp.entry(ext) }.or_default() += 1;
                }
                if e.is_archive() { q.push((format!("{name}/{path}"), ar.open_child(i, Some(&keys)).unwrap())); }
            }
        }
    }
    println!("uncompressed: {raw:?}\ncompressed: {comp:?}\narchive encryption: {enc:?}\nuppercase names: {upper}");
}
