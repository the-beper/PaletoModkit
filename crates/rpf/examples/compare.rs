//! Compares every file (nested archives included) of two archives and lists
//! what differs. With a third archive, also tells whether each change in `b`
//! matches the one in `c` (e.g. the same mods installed by OpenIV).
//!
//! cargo run --release -p paleto-rpf --example compare -- <keys.bin> <a.rpf> <b.rpf> [c.rpf]

use std::collections::BTreeMap;
use std::path::Path;

use paleto_rpf::{Archive, Keys};
use sha1::Digest;

fn contents(path: &Path, keys: &Keys) -> BTreeMap<String, [u8; 20]> {
    let mut out = BTreeMap::new();
    let mut queue = vec![(String::new(), Archive::open(path, Some(keys)).unwrap())];
    while let Some((prefix, a)) = queue.pop() {
        for (p, i) in a.walk(0) {
            let e = &a.entries[i];
            if e.is_archive() {
                queue.push((format!("{prefix}{p}/"), a.open_child(i, Some(keys)).unwrap()));
            } else if !e.is_dir() {
                out.insert(format!("{prefix}{p}"), sha1::Sha1::digest(a.read(i, Some(keys)).unwrap()).into());
            }
        }
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap();
    let (x, y) = (contents(a[2].as_ref(), &keys), contents(a[3].as_ref(), &keys));
    let z = a.get(4).map(|p| contents(p.as_ref(), &keys));
    let vs = |k: &str| match &z {
        None => "",
        Some(z) if z.get(k) == y.get(k) => "  [same as reference]",
        Some(z) if z.get(k).is_none() => "  [missing in reference]",
        Some(z) if z.get(k) == x.get(k) => "  [unchanged in reference]",
        Some(_) => "  [DIFFERENT from reference]",
    };
    let mut same = 0;
    for (k, v) in &x {
        match y.get(k) {
            Some(w) if w == v => same += 1,
            Some(_) => println!("changed  {k}{}", vs(k)),
            None => println!("removed  {k}"),
        }
    }
    for k in y.keys().filter(|k| !x.contains_key(*k)) {
        println!("added    {k}{}", vs(k));
    }
    println!("{same} identical of {}", x.len());
}
