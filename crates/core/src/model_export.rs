//! A model made ready for CodeWalker, Sollumz and Blender: its CodeWalker XML, its textures as DDS files, and a note of
//! where the model and each texture came from and which textures were not found.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use paleto_formats::xml_export;
use paleto_formats::ytd::TextureDictionary;

use crate::{Error, vfs};

/// What [`export`] wrote.
#[derive(Debug)]
pub struct Report {
    /// The XML file.
    pub xml: PathBuf,
    /// The folder of the DDS files (made only when there are any).
    pub texture_folder: PathBuf,
    /// How many DDS files it holds.
    pub textures: usize,
    /// The textures the model names that no dictionary searched holds.
    pub missing: Vec<String>,
    /// The note of the export.
    pub note: PathBuf,
    /// What the writer noted (see [`vfs::write_into`]).
    pub log: Vec<String>,
}

/// Exports the model `file` (its bytes `data`: a `.ydr`, `.ydd`, `.yft` or `.ybn`) into `dir`: `<file>.xml`, its textures
/// as DDS files in `<name>/` (where CodeWalker and Sollumz look for them) and `<file>.export.txt`, the note. `dictionaries`
/// are the texture dictionaries searched, `names[i]` the file of `dictionaries[i]`; `source` says where the model is, for
/// the note. Written as any extraction is (see [`vfs::write_into`]).
pub fn export(dir: &Path, file: &str, source: &str, data: &[u8], dictionaries: &[TextureDictionary], names: &[String]) -> Result<Report, Error> {
    let (stem, ext) = file.rsplit_once('.').ok_or_else(|| Error::Export(format!("{file}: not a model")))?;
    let export = xml_export::export(data, ext, stem, dictionaries).map_err(|e| Error::Export(format!("{file}: {e}")))?;
    let folder = xml_export::texture_folder(stem);
    let note = note(file, source, &folder, &export, names);
    let ((xml, note), log) = vfs::write_into(dir, "exporting a model", |out| {
        out.mkdir("")?;
        let xml = out.write(&format!("{file}.xml"), export.xml.as_bytes())?;
        for texture in &export.textures {
            out.write(&format!("{folder}/{}", texture.file), &texture.dds)?;
        }
        let note = out.write(&format!("{file}.export.txt"), note.as_bytes())?;
        Ok((xml, note))
    })?;
    let texture_folder = xml.with_file_name(&folder);
    Ok(Report { xml, texture_folder, textures: export.textures.len(), missing: export.missing, note, log })
}

/// The text of `<file>.export.txt`.
fn note(file: &str, source: &str, folder: &str, export: &xml_export::Export, names: &[String]) -> String {
    let mut out = format!("Exported by Paleto Modkit {} for CodeWalker, Sollumz and Blender.\n\n", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "Model: {source}");
    let _ = writeln!(out, "XML: {file}.xml");
    let _ = writeln!(out, "Textures: {} DDS files in {folder}/", export.textures.len());
    for texture in &export.textures {
        let from = texture.dictionary.and_then(|i| names.get(i)).map_or_else(|| "the model's own".to_string(), |name| format!("from {name}"));
        let _ = writeln!(out, "  {}  {from}", texture.file);
    }
    let searched = if names.is_empty() { "none".to_string() } else { names.join(", ") };
    let _ = writeln!(out, "Texture dictionaries searched: {searched}");
    if !export.missing.is_empty() {
        let _ = writeln!(out, "Not found: {}", export.missing.join(", "));
        let _ = writeln!(out, "  The model names these textures, but neither it nor the dictionaries searched hold them. A dictionary that a .ytyp names may.");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_formats::xml_export::ExportedTexture;

    #[test]
    fn the_note_says_where_each_texture_came_from_and_what_is_missing() {
        let dds = |file: &str, dictionary| ExportedTexture { file: file.into(), dds: Vec::new(), dictionary };
        let export = xml_export::Export { xml: String::new(), textures: vec![dds("body.dds", None), dds("lights.dds", Some(1))], missing: vec!["decals".into(), "dirt".into()] };
        let text = note("car.yft", "x64e.rpf/levels/gta5/vehicles.rpf/car.yft", "car", &export, &["car.ytd".into(), "vehshare.ytd".into()]);
        for line in [
            "Model: x64e.rpf/levels/gta5/vehicles.rpf/car.yft\n",
            "XML: car.yft.xml\n",
            "Textures: 2 DDS files in car/\n",
            "  body.dds  the model's own\n",
            "  lights.dds  from vehshare.ytd\n",
            "Texture dictionaries searched: car.ytd, vehshare.ytd\n",
            "Not found: decals, dirt\n",
        ] {
            assert!(text.contains(line), "{line:?} in {text}");
        }
        let plain = note("p.ybn", "p.ybn", "p", &xml_export::Export { xml: String::new(), textures: Vec::new(), missing: Vec::new() }, &[]);
        assert!(plain.contains("Texture dictionaries searched: none\n") && !plain.contains("Not found"), "{plain}");
    }

    #[test]
    fn a_file_that_is_not_a_model_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        for file in ["notes", "a.ytd", "b.ydr"] {
            let err = export(&out, file, file, b"not a model", &[], &[]).unwrap_err().to_string();
            assert!(err.starts_with(&format!("{file}: ")), "{err}");
        }
        assert!(!out.exists());
    }
}
