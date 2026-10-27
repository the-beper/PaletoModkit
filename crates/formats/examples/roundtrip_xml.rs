//! Checks that CodeWalker accepts our CodeWalker XML export: takes every Nth
//! model of a game folder (nested archives included), exports it with
//! `xml_export` into a work folder (XML and DDS files), imports that XML with
//! CodeWalker's own importers (`target/cwxml/paleto-cwxml`, built with
//! `PROJECT=cwxml packaging/build-cwconv.sh`; `PALETO_CWXML` names another
//! one), then reads the file CodeWalker built with our readers and compares
//! it with the original: drawables and their names, levels of detail, and per
//! level the vertices, triangles and positions. Enhanced files (told by their
//! resource version) are saved as gen9 resources, Legacy ones as Legacy.
//!
//! cargo run --release -p paleto-formats --example roundtrip_xml -- <game dir> <N> [kinds...] [--keep]
//! where kinds are some of: ydr ydd yft ybn (all exported kinds by default). The work
//! folder is in the temporary folder, deleted after each batch unless --keep.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use paleto_formats::drawable::Drawable;
use paleto_rpf::{Archive, Keys};

/// Files passed to one run of paleto-cwxml.
const BATCH: usize = 100;
/// Largest difference allowed between a position and its round-tripped one.
const TOLERANCE: f32 = 0.01;

/// Kinds with an XML exporter. To add one, list it here and in `export`.
const KINDS: [&str; 4] = ["ydr", "ydd", "yft", "ybn"];

/// Our XML of a file and the DDS files it names.
/// The XML and the DDS files (name, contents) of an export.
type Exported = (String, Vec<(String, Vec<u8>)>);

fn export(ext: &str, data: &[u8], stem: &str) -> Result<Exported, String> {
    match ext {
        "ydr" | "ydd" | "yft" | "ybn" => paleto_formats::xml_export::export(data, ext, stem, &[]).map(|e| (e.xml, e.textures)).map_err(|e| e.to_string()),
        other => Err(format!("no exporter for .{other}")),
    }
}

/// A file read with our readers, for comparison.
fn read(ext: &str, data: &[u8]) -> Result<Vec<Drawable>, String> {
    let read = if ext == "ybn" { paleto_formats::bounds::parse(data) } else { paleto_formats::drawable::parse(data, ext, &[]) };
    read.map_err(|e| e.to_string())
}

/// A file waiting for CodeWalker.
struct Pending {
    /// Where it is in the game, for the report.
    name: String,
    ext: String,
    data: Vec<u8>,
    xml: PathBuf,
}

#[derive(Default)]
struct Stats {
    files: usize,
    export: usize,
    codewalker: usize,
    mismatch: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--keep").collect();
    let keep = std::env::args().any(|a| a == "--keep");
    if args.len() < 2 {
        eprintln!("usage: roundtrip_xml <game dir> <N> [kinds...] [--keep]");
        std::process::exit(2);
    }
    let game = PathBuf::from(&args[0]);
    let every: usize = args[1].parse::<usize>().expect("N must be a number").max(1);
    let kinds: Vec<&str> = if args.len() > 2 { args[2..].iter().map(String::as_str).collect() } else { KINDS.to_vec() };
    let tool = std::env::var_os("PALETO_CWXML").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cwxml/paleto-cwxml"));
    assert!(tool.is_file(), "{} not found: build it with PROJECT=cwxml packaging/build-cwconv.sh", tool.display());
    let work = std::env::temp_dir().join(format!("paleto-roundtrip-xml-{}", std::process::id()));
    let keys = keys();
    let mut stats: BTreeMap<String, Stats> = BTreeMap::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut seen = 0usize;
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
                let file = p.rsplit('/').next().unwrap_or(&p).to_string();
                let Some((stem, ext)) = file.rsplit_once('.') else { continue };
                let ext = ext.to_ascii_lowercase();
                if !kinds.contains(&ext.as_str()) {
                    continue;
                }
                seen += 1;
                if !(seen - 1).is_multiple_of(every) {
                    continue;
                }
                let Ok(data) = archive.read(i, Some(&keys)) else { continue };
                let full = format!("{name}/{p}");
                let s = stats.entry(ext.clone()).or_default();
                s.files += 1;
                let (xml, textures) = match export(&ext, &data, stem) {
                    Ok(x) => x,
                    Err(err) => {
                        s.export += 1;
                        report(&mut errors, &full, &format!("{ext} export: {err}"));
                        continue;
                    }
                };
                let dir = work.join(format!("{seen:07}"));
                std::fs::create_dir_all(&dir).unwrap();
                let xml_path = dir.join(format!("{file}.xml"));
                std::fs::write(&xml_path, xml).unwrap();
                // Next to the XML, in a folder named after the model, as CodeWalker does.
                let textures_dir = dir.join(stem);
                for (tex, dds) in &textures {
                    std::fs::create_dir_all(&textures_dir).unwrap();
                    std::fs::write(textures_dir.join(tex), dds).unwrap();
                }
                pending.push(Pending { name: full, ext, data, xml: xml_path });
                if pending.len() >= BATCH {
                    run_batch(&tool, std::mem::take(&mut pending), &mut stats, &mut errors, &work, keep);
                }
            }
        }
    }
    run_batch(&tool, pending, &mut stats, &mut errors, &work, keep);
    if keep {
        println!("work folder: {}", work.display());
    } else {
        let _ = std::fs::remove_dir_all(&work);
    }
    for (ext, s) in &stats {
        let ok = s.files - s.export - s.codewalker - s.mismatch;
        println!("{ext}: {} files, {ok} ok, {} export errors, {} CodeWalker errors, {} mismatches", s.files, s.export, s.codewalker, s.mismatch);
    }
    for (err, n) in &errors {
        println!("  {n:6} × {err}");
    }
}

/// Prints the first few files of each kind of error and counts them all.
fn report(errors: &mut BTreeMap<String, usize>, file: &str, err: &str) {
    let count = errors.entry(err.to_string()).or_default();
    *count += 1;
    if *count <= 3 {
        eprintln!("{file}: {err}");
    }
}

/// Imports a batch with CodeWalker (gen9 and Legacy files apart) and compares what it built.
fn run_batch(tool: &Path, batch: Vec<Pending>, stats: &mut BTreeMap<String, Stats>, errors: &mut BTreeMap<String, usize>, work: &Path, keep: bool) {
    let (gen9, legacy): (Vec<Pending>, Vec<Pending>) = batch.into_iter().partition(|f| is_gen9(&f.ext, &f.data));
    for (files, gen9) in [(gen9, true), (legacy, false)] {
        if files.is_empty() {
            continue;
        }
        let mut cmd = Command::new(tool);
        if gen9 {
            cmd.arg("--gen9");
        }
        let out = cmd.args(files.iter().map(|f| &f.xml)).output().expect("running paleto-cwxml");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut answers: BTreeMap<&str, Result<(), &str>> = BTreeMap::new();
        for line in stdout.lines() {
            if let Some(file) = line.strip_prefix("ok ") {
                answers.insert(file, Ok(()));
            } else if let Some(rest) = line.strip_prefix("error ")
                && let Some(f) = files.iter().find(|f| rest.starts_with(&format!("{}: ", f.xml.display())))
            {
                let (file, msg) = rest.split_at(f.xml.display().to_string().len());
                answers.insert(file, Err(&msg[2..]));
            }
        }
        for f in &files {
            let s = stats.get_mut(&f.ext).unwrap();
            let xml = f.xml.display().to_string();
            match answers.get(xml.as_str()) {
                None => {
                    s.codewalker += 1;
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    report(errors, &f.name, &format!("{} CodeWalker: no answer ({}, {})", f.ext, out.status, stderr.lines().next().unwrap_or("")));
                }
                Some(Err(msg)) => {
                    s.codewalker += 1;
                    report(errors, &f.name, &format!("{} CodeWalker: {msg}", f.ext));
                }
                Some(Ok(())) => {
                    let built = std::fs::read(format!("{}.cw", xml.strip_suffix(".xml").unwrap())).unwrap();
                    if let Err(msg) = compare(&f.ext, &f.data, &built) {
                        s.mismatch += 1;
                        eprintln!("{}: {msg}", f.name);
                        *errors.entry(format!("{} mismatch: {}", f.ext, msg.split(':').next().unwrap_or(&msg))).or_default() += 1;
                    }
                }
            }
        }
    }
    if !keep {
        let _ = std::fs::remove_dir_all(work);
    }
}

/// Whether a resource is in the Enhanced layout (CodeWalker saves collision the same way for both).
fn is_gen9(ext: &str, data: &[u8]) -> bool {
    ext != "ybn" && paleto_formats::rsc::Resource::parse(data).is_ok_and(|r| !matches!(r.version, 162 | 165))
}

/// Compares the original with what CodeWalker built from our XML; the first difference.
fn compare(ext: &str, original: &[u8], built: &[u8]) -> Result<(), String> {
    let mut a = read(ext, original).map_err(|e| format!("original unreadable: {e}"))?;
    let mut b = read(ext, built).map_err(|e| format!("CodeWalker's file unreadable: {e}"))?;
    // A dictionary may come back in another order.
    a.sort_by(|x, y| x.name.cmp(&y.name));
    b.sort_by(|x, y| x.name.cmp(&y.name));
    if a.len() != b.len() {
        return Err(format!("drawables: {} → {}", a.len(), b.len()));
    }
    for (x, y) in a.iter().zip(&b) {
        let who = if x.name.is_empty() { String::new() } else { format!(" ({})", x.name) };
        if x.name != y.name {
            return Err(format!("drawable names: {} → {}", x.name, y.name));
        }
        let lods = |d: &Drawable| d.lods.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" ");
        if lods(x) != lods(y) {
            return Err(format!("levels{who}: {} → {}", lods(x), lods(y)));
        }
        for ((lod, m), (_, n)) in x.lods.iter().zip(&y.lods) {
            if m.indices.len() != n.indices.len() {
                return Err(format!("triangles{who} {lod}: {} → {}", m.indices.len() / 3, n.indices.len() / 3));
            }
            if m.positions.len() != n.positions.len() {
                return Err(format!("vertices{who} {lod}: {} → {}", m.positions.len(), n.positions.len()));
            }
            let worst = m.positions.iter().zip(&n.positions).map(|(p, q)| (0..3).map(|k| (p[k] - q[k]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
            if worst > TOLERANCE {
                return Err(format!("positions{who} {lod}: off by up to {worst}"));
            }
        }
    }
    Ok(())
}

/// The cached keys.
fn keys() -> Keys {
    let path = Path::new(&std::env::var("HOME").unwrap()).join(".local/share/paleto/keys.bin");
    Keys::from_bytes(&std::fs::read(path).expect("run `paleto setup` first")).unwrap()
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
