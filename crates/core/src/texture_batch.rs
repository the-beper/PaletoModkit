//! Replacing many textures of a dictionary at once, as mods ship them: a
//! folder of DDS (or PNG) files named after the textures they replace.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Which files replace which textures.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TextureBatch {
    /// Texture name (as in the dictionary) and the file that replaces it.
    pub replace: Vec<(String, PathBuf)>,
    /// Files named after no texture of the dictionary.
    pub unmatched: Vec<PathBuf>,
    /// Files skipped because another one has the same name (a DDS wins over
    /// a PNG, then the first found).
    pub duplicates: Vec<PathBuf>,
}

/// Whether a path is a picture a texture can be made from.
pub fn is_texture_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds") || e.eq_ignore_ascii_case("png"))
}

/// The DDS and PNG files among `paths`, folders searched through, sorted.
pub fn texture_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if is_texture_file(&path) {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Matches files to the dictionary's textures by name, ignoring case and the
/// extension: `Decal_Blood.dds` replaces the texture `decal_blood`.
pub fn match_files(textures: &[&str], files: &[PathBuf]) -> TextureBatch {
    let by_name: HashMap<String, &str> = textures.iter().map(|t| (t.to_ascii_lowercase(), *t)).collect();
    // DDS files first, so they win over PNGs of the same name.
    let mut files: Vec<&PathBuf> = files.iter().collect();
    files.sort_by_key(|f| !f.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds")));
    let mut batch = TextureBatch::default();
    let mut taken = HashSet::new();
    for file in files {
        let stem = file.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !taken.insert(stem.clone()) {
            batch.duplicates.push(file.clone());
            continue;
        }
        match by_name.get(&stem) {
            Some(texture) => batch.replace.push((texture.to_string(), file.clone())),
            None => batch.unmatched.push(file.clone()),
        }
    }
    batch.replace.sort();
    batch.unmatched.sort();
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("mod/textures")).unwrap();
        for f in ["mod/Decal_Blood.dds", "mod/textures/decal_mud.png", "mod/textures/decal_mud.dds", "mod/new_decal.png", "mod/readme.txt"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let files = texture_files(&[root.join("mod")]);
        assert_eq!(files.len(), 4, "{files:?}");
        let batch = match_files(&["decal_blood", "decal_mud", "decal_oil"], &files);
        assert_eq!(batch.replace, [("decal_blood".to_string(), root.join("mod/Decal_Blood.dds")), ("decal_mud".to_string(), root.join("mod/textures/decal_mud.dds"))]);
        assert_eq!(batch.unmatched, [root.join("mod/new_decal.png")]);
        assert_eq!(batch.duplicates, [root.join("mod/textures/decal_mud.png")]);
    }
}
