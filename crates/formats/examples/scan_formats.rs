//! Reads every metadata file, text table, audio container and model in a game
//! folder (nested archives included), as a smoke test:
//! - metadata: XML → file → XML must give the same XML; PSO checksums must match;
//! - text tables: text → file → entries must give the same entries;
//! - audio and models: they must decode.
//!
//! cargo run --release -p paleto-formats --example scan_formats -- <game dir> [kinds...]
//! where kinds are some of: meta gxt2 awc model (all by default).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use paleto_formats::MetaFile;
use paleto_rpf::{Archive, Keys};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let game = PathBuf::from(&args[1]);
    let kinds: Vec<&str> = if args.len() > 2 { args[2..].iter().map(String::as_str).collect() } else { vec!["meta", "gxt2", "awc", "model"] };
    let keys = keys(&game);
    let mut stats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    for path in walk_dir(&game) {
        let Ok(archive) = Archive::open(&path, Some(&keys)) else { continue };
        let mut queue = vec![(path.display().to_string(), archive)];
        while let Some((name, archive)) = queue.pop() {
            for (p, i) in archive.walk(0) {
                let e = &archive.entries[i];
                if e.is_archive() {
                    if let Ok(child) = archive.open_child(i, Some(&keys)) {
                        queue.push((format!("{name}/{p}"), child));
                    }
                    continue;
                }
                let ext = p.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()).unwrap_or_default();
                let kind = match ext.as_str() {
                    "ymt" | "ytyp" | "ymap" | "pso" => "meta",
                    "gxt2" => "gxt2",
                    "awc" => "awc",
                    "ydr" | "yft" | "ydd" => "model",
                    _ => continue,
                };
                if !kinds.contains(&kind) {
                    continue;
                }
                let Ok(data) = archive.read(i, Some(&keys)) else { continue };
                let result = match kind {
                    "meta" => check_meta(&data),
                    "gxt2" => check_gxt2(&data),
                    "awc" => paleto_formats::awc::parse(&data, keys.awc.as_ref()).map(|_| ()).map_err(|e| e.to_string()),
                    _ => paleto_formats::drawable::parse(&data, &ext, &[]).map(|_| ()).map_err(|e| e.to_string()),
                };
                let entry = stats.entry(ext.clone()).or_default();
                entry.0 += 1;
                if let Err(err) = result {
                    entry.1 += 1;
                    let count = errors.entry(format!("{ext}: {err}")).or_default();
                    *count += 1;
                    if *count <= 3 {
                        eprintln!("{name}/{p}: {err}");
                    }
                }
            }
        }
    }
    for (ext, (files, failed)) in &stats {
        println!("{ext}: {files} files, {failed} errors");
    }
    for (err, n) in &errors {
        println!("  {n:6} × {err}");
    }
}

fn check_meta(data: &[u8]) -> Result<(), String> {
    if !MetaFile::is_meta(data) {
        return Err("not metadata".into());
    }
    if MetaFile::pso_checksum_ok(data) == Some(false) {
        return Err("original PSO checksum does not match our algorithm".into());
    }
    let meta = MetaFile::parse(data).map_err(|e| format!("parse: {e}"))?;
    let xml = meta.to_xml();
    let rebuilt = meta.rebuild_from_xml(&xml).map_err(|e| format!("rebuild: {e}"))?;
    if MetaFile::pso_checksum_ok(&rebuilt) == Some(false) {
        return Err("rebuilt PSO checksum is wrong".into());
    }
    let again = MetaFile::parse(&rebuilt).map_err(|e| format!("reparse: {e}"))?;
    if again.to_xml() != xml {
        return Err("XML changed after a round trip".into());
    }
    // Rebuilt blocks are packed again, but should hold about as much data.
    // PSO blocks have no padding of their own: their data must nearly match.
    let (num, den) = if data.starts_with(b"PSIN") { (97, 100) } else { (8, 10) };
    if again.data_len() * den < meta.data_len() * num {
        return Err(format!("rebuilt data is smaller ({} of {} bytes)", again.data_len(), meta.data_len()));
    }
    Ok(())
}

fn check_gxt2(data: &[u8]) -> Result<(), String> {
    let mut entries = paleto_formats::gxt2::parse(data).map_err(|e| e.to_string())?;
    let back = paleto_formats::gxt2::from_text(&paleto_formats::gxt2::to_text(&entries)).map_err(|e| e.to_string())?;
    // Written tables are sorted by hash.
    entries.sort_by_key(|(hash, _)| *hash);
    if paleto_formats::gxt2::parse(&back).map_err(|e| e.to_string())? != entries {
        return Err("text round trip changed the entries".into());
    }
    Ok(())
}

/// The cached keys, with the audio key derived again when missing.
fn keys(game: &Path) -> Keys {
    let data_dir = Path::new(&std::env::var("HOME").unwrap()).join(".local/share/paleto");
    let keys = Keys::from_bytes(&std::fs::read(data_dir.join("keys.bin")).expect("run `paleto setup` first")).unwrap();
    if keys.awc.is_some() {
        return keys;
    }
    let exe = std::fs::read(game.join("GTA5.exe")).or_else(|_| std::fs::read(game.join("GTA5_Enhanced.exe"))).unwrap();
    Keys::from_exe_and_magic(&exe, &std::fs::read(data_dir.join("magic.dat")).unwrap()).unwrap()
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
