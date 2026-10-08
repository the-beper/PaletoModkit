//! Opens every archive (including nested ones) under a game folder and reads
//! one file from each, as a smoke test for the reader.
//!
//! cargo run --release -p paleto-rpf --example scan -- <game dir> <keys.bin>

use std::path::Path;

use paleto_rpf::{Archive, Keys};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&args[2]).unwrap()).unwrap();
    let (mut archives, mut entries, mut errors) = (0, 0, 0);
    let mut stack: Vec<_> = walk_dir(Path::new(&args[1]));
    while let Some(path) = stack.pop() {
        let mut queue = match Archive::open(&path, Some(&keys)) {
            Ok(a) => vec![(path.display().to_string(), a)],
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                errors += 1;
                continue;
            }
        };
        while let Some((name, archive)) = queue.pop() {
            archives += 1;
            entries += archive.entries.len();
            let mut read_one = false;
            for (p, i) in archive.walk(0) {
                let e = &archive.entries[i];
                if e.is_archive() {
                    match archive.open_child(i, Some(&keys)) {
                        Ok(child) => queue.push((format!("{name}/{p}"), child)),
                        Err(err) => {
                            eprintln!("{name}/{p}: {err}");
                            errors += 1;
                        }
                    }
                } else if !e.is_dir() && !read_one {
                    read_one = true;
                    if let Err(err) = archive.read(i, Some(&keys)) {
                        eprintln!("{name}/{p}: {err}");
                        errors += 1;
                    }
                }
            }
        }
    }
    println!("{archives} archives, {entries} entries, {errors} errors");
}

fn walk_dir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(walk_dir(&p));
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpf")) {
            out.push(p);
        }
    }
    out
}
