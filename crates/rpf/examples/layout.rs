//! Prints an archive's entries in on-disk order: offset, blocks, path.
use paleto_rpf::{Archive, Keys};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap();
    let ar = Archive::open(a[2].as_ref(), Some(&keys)).unwrap();
    let mut v: Vec<_> = ar.walk(0).into_iter().filter_map(|(p, i)| {
        let e = &ar.entries[i];
        e.offset().map(|o| (o, e.stored_len().div_ceil(512), p))
    }).collect();
    v.sort();
    for (o, b, p) in v { println!("{o:>10} {b:>10} {p}"); }
}
