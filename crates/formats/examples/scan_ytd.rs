//! Parses every texture dictionary in a game folder (nested archives included)
//! and decodes the smallest mip level of each texture, as a smoke test. With
//! --rebuild, also rebuilds each dictionary (re-importing its first texture
//! from its own DDS export) and checks that every texture survives unchanged,
//! then adds a texture from a PNG and renames one, undoes both and checks
//! that the dictionary is back as it was.
//!
//! cargo run --release -p paleto-formats --example scan_ytd -- <game dir> <keys.bin>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use paleto_formats::{Edit, TextureDictionary, edit_textures, replace_textures};
use paleto_rpf::{Archive, Keys};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let keys = Keys::from_bytes(&std::fs::read(&args[2]).unwrap()).unwrap();
    let rebuild = args.iter().any(|a| a == "--rebuild");
    let (mut files, mut textures, mut errors) = (0, 0, 0);
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for path in walk_dir(Path::new(&args[1])) {
        let Ok(archive) = Archive::open(&path, Some(&keys)) else { continue };
        let mut queue = vec![(path.display().to_string(), archive)];
        while let Some((name, archive)) = queue.pop() {
            for (p, i) in archive.walk(0) {
                let e = &archive.entries[i];
                if e.is_archive() {
                    if let Ok(child) = archive.open_child(i, Some(&keys)) {
                        queue.push((format!("{name}/{p}"), child));
                    }
                } else if p.to_ascii_lowercase().ends_with(".ytd") {
                    files += 1;
                    let result = archive.read(i, Some(&keys)).map_err(|e| e.to_string()).and_then(|data| {
                        let ytd = TextureDictionary::parse(&data).map_err(|e| e.to_string())?;
                        if rebuild {
                            check_rebuild(&data, &ytd)?;
                        }
                        Ok(ytd)
                    });
                    match result {
                        Ok(ytd) => {
                            for tex in &ytd.textures {
                                textures += 1;
                                *formats.entry(tex.format.name().to_string()).or_default() += 1;
                                tex.rgba(tex.levels - 1).expect("level present");
                            }
                        }
                        Err(err) => {
                            errors += 1;
                            if errors <= 20 {
                                eprintln!("{name}/{p}: {err}");
                            }
                        }
                    }
                }
            }
        }
    }
    println!("{files} texture dictionaries, {textures} textures, {errors} errors");
    for (format, n) in formats {
        println!("  {format}: {n}");
    }
}

fn check_rebuild(data: &[u8], ytd: &TextureDictionary) -> Result<(), String> {
    let first = ytd.textures.first().map(|t| (t.name.clone(), t.to_dds()));
    let replacements: Vec<(&str, &[u8])> = first.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let rebuilt = replace_textures(data, &replacements).map_err(|e| format!("rebuild: {e}"))?;
    same(ytd, &rebuilt)?;

    const PROBE: &str = "zz_paleto_probe";
    let png = probe_png();
    let renamed = first.as_ref().map(|(n, _)| (n.as_str(), format!("{n}_paleto")));
    let mut edits = vec![Edit::Add { name: PROBE, file: &png, format: None }];
    edits.extend(renamed.iter().map(|(from, to)| Edit::Rename { from, to }));
    let edited = edit_textures(data, &edits).map_err(|e| format!("edit: {e}"))?;
    let parsed = TextureDictionary::parse(&edited).map_err(|e| format!("reparse: {e}"))?;
    let probe = parsed.textures.iter().find(|t| t.name == PROBE).ok_or("added texture missing")?;
    if (probe.width, probe.height, probe.levels) != (32, 16, 3) || probe.rgba(2).is_none() {
        return Err("added texture is wrong".into());
    }
    let mut undo = vec![Edit::Remove(PROBE)];
    undo.extend(renamed.iter().map(|(from, to)| Edit::Rename { from: to, to: from }));
    same(ytd, &edit_textures(&edited, &undo).map_err(|e| format!("undo: {e}"))?)
}

/// Whether a rebuilt dictionary has the same textures, in the same order.
fn same(ytd: &TextureDictionary, rebuilt: &[u8]) -> Result<(), String> {
    let again = TextureDictionary::parse(rebuilt).map_err(|e| format!("reparse: {e}"))?;
    if again.textures.len() != ytd.textures.len() {
        return Err("texture count changed".into());
    }
    for (a, b) in ytd.textures.iter().zip(&again.textures) {
        if (&a.name, a.width, a.height, a.levels, a.format, &a.data) != (&b.name, b.width, b.height, b.levels, b.format, &b.data) {
            return Err(format!("texture {} changed", a.name));
        }
    }
    Ok(())
}

fn probe_png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(32, 16, |x, y| image::Rgba([(x * 8) as u8, (y * 16) as u8, 128, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn walk_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "mods") {
                out.extend(walk_dir(&p));
            }
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpf")) {
            out.push(p);
        }
    }
    out
}
