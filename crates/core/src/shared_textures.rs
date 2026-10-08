//! Texture dictionaries that models share. The game gives each vehicle a
//! dictionary of its own and links it to a parent (`<txdRelationships>` in the
//! vehicles.meta of the game and of each DLC pack), and so on up to
//! vehshare.ytd: a texture missing from a dictionary comes from its parents.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, Keys};

use crate::Game;
use crate::game::resolve_ci;
use crate::vfs::ArchivePath;

/// The textures every vehicle shares.
const VEHSHARE: &str = "vehshare";

/// Where the shared dictionaries are, and how they link.
#[derive(Debug, Default, Clone)]
pub struct SharedTextures {
    /// Child dictionary → parent, lowercase.
    parents: HashMap<String, String>,
    /// Dictionary name (lowercase, no extension) → its `.ytd`.
    places: HashMap<String, ArchivePath>,
}

impl SharedTextures {
    /// Reads the links and finds the vehicle dictionaries: the game's, its
    /// update's, then each DLC pack's (later ones win). Archives with a copy in
    /// the mods folder are read from there, as the game does.
    pub fn index(game: &Game, keys: Option<&Keys>) -> Self {
        let mut index = SharedTextures::default();
        let disk = |rel: &str| -> PathBuf {
            let modded = resolve_ci(&game.mods_dir(), rel);
            if modded.is_file() { modded } else { game.original(rel) }
        };
        index.add(&disk("common.rpf"), &["data/levels/gta5/vehicles.meta"], keys);
        index.add(&disk("x64e.rpf"), &["levels/gta5/vehicles.rpf"], keys);
        index.add(&disk("update/update.rpf"), &["common/data/levels/gta5/vehicles.meta", "x64/levels/gta5/vehicles.rpf"], keys);
        // DLC packs, the game's and those added in the mods folder.
        let mut packs: Vec<String> = [game.original("update/x64/dlcpacks"), resolve_ci(&game.mods_dir(), "update/x64/dlcpacks")]
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
            .collect();
        // In the game's loading order (dlclist.xml), packs it does not list last.
        let order = Archive::open(&disk("update/update.rpf"), keys)
            .ok()
            .and_then(|a| a.read(a.find("common/data/dlclist.xml")?, keys).ok())
            .map(|xml| crate::loader::dlc_pack_order(&String::from_utf8_lossy(&xml)))
            .unwrap_or_default();
        packs.sort_by_key(|p| (order.iter().position(|o| o == p).unwrap_or(usize::MAX), p.clone()));
        packs.dedup();
        for pack in packs {
            let rel = format!("update/x64/dlcpacks/{pack}/dlc.rpf");
            index.add_pack(&disk(&rel), keys);
        }
        index
    }

    /// The dictionaries a dictionary takes missing textures from, nearest
    /// first. Vehicle dictionaries end with vehshare, linked or not.
    pub fn parents_of(&self, name: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = name.to_ascii_lowercase();
        while let Some(parent) = self.parents.get(&current) {
            if !seen.insert(parent.clone()) {
                break;
            }
            chain.push(parent.clone());
            current = parent.clone();
        }
        if current != VEHSHARE && self.places.contains_key(&current) && self.places.contains_key(VEHSHARE) {
            chain.push(VEHSHARE.to_string());
        }
        chain
    }

    /// Where a vehicle dictionary is.
    pub fn place(&self, name: &str) -> Option<&ArchivePath> {
        self.places.get(&name.to_ascii_lowercase())
    }

    /// Reads the given vehicles.meta files and indexes the dictionaries of the
    /// given archives inside an archive on disk.
    fn add(&mut self, disk: &Path, paths: &[&str], keys: Option<&Keys>) {
        let Ok(archive) = Archive::open(disk, keys) else { return };
        for path in paths {
            let Some(idx) = archive.find(path) else { continue };
            if archive.entries[idx].is_archive() {
                let inner: Vec<String> = path.split('/').map(str::to_string).collect();
                self.add_archive(&archive, idx, ArchivePath { disk: disk.to_path_buf(), inner }, keys);
            } else if let Ok(data) = archive.read(idx, keys) {
                self.add_links(&String::from_utf8_lossy(&data));
            }
        }
    }

    /// A DLC pack: its vehicles.meta files and its vehicle archives.
    fn add_pack(&mut self, disk: &Path, keys: Option<&Keys>) {
        let Ok(archive) = Archive::open(disk, keys) else { return };
        for (path, idx) in archive.walk(0) {
            let lower = path.to_ascii_lowercase();
            if lower.ends_with("vehicles.meta") {
                if let Ok(data) = archive.read(idx, keys) {
                    self.add_links(&String::from_utf8_lossy(&data));
                }
            } else if archive.entries[idx].is_archive() && (lower.contains("levels/gta5/vehicles") || lower.contains("/vehicles/")) {
                let inner: Vec<String> = path.split('/').map(str::to_string).collect();
                self.add_archive(&archive, idx, ArchivePath { disk: disk.to_path_buf(), inner }, keys);
            }
        }
    }

    /// The `.ytd` files of a nested archive.
    fn add_archive(&mut self, parent: &Archive, idx: usize, at: ArchivePath, keys: Option<&Keys>) {
        let Ok(archive) = parent.open_child(idx, keys) else { return };
        for (path, _) in archive.walk(0) {
            let Some(stem) = path.rsplit('/').next().and_then(|n| n.strip_suffix(".ytd").or_else(|| n.strip_suffix(".YTD"))) else { continue };
            let mut place = at.clone();
            place.inner.extend(path.split('/').map(str::to_string));
            self.places.insert(stem.to_ascii_lowercase(), place);
        }
    }

    /// The `<parent>` / `<child>` pairs of a vehicles.meta.
    fn add_links(&mut self, meta: &str) {
        let tag = |text: &str, name: &str| -> Option<String> {
            let start = text.find(&format!("<{name}>"))? + name.len() + 2;
            let end = text[start..].find(&format!("</{name}>"))? + start;
            Some(text[start..end].trim().to_ascii_lowercase())
        };
        let Some(start) = meta.find("<txdRelationships>") else { return };
        let end = meta[start..].find("</txdRelationships>").map_or(meta.len(), |e| start + e);
        for item in meta[start..end].split("<Item>").skip(1) {
            if let (Some(parent), Some(child)) = (tag(item, "parent"), tag(item, "child"))
                && !parent.is_empty()
                && parent != child
            {
                self.parents.insert(child, parent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_rpf::ArchiveEditor;

    fn meta(pairs: &[(&str, &str)]) -> Vec<u8> {
        let items: String = pairs.iter().map(|(p, c)| format!("<Item>\n<parent>{p}</parent>\n<child>{c}</child>\n</Item>\n")).collect();
        format!("<CVehicleModelInfo__InitDataList>\n<txdRelationships>\n{items}</txdRelationships>\n</CVehicleModelInfo__InitDataList>").into_bytes()
    }

    #[test]
    fn links_and_places() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        let game = Game::open(root).unwrap();

        // The game's vehicles and links.
        let mut vehicles = ArchiveEditor::new("vehicles.rpf");
        for name in ["vehshare.ytd", "vehicles_supergt_interior.ytd", "adder.ytd", "adder.yft"] {
            vehicles.add_file(name, vec![0; 16]).unwrap();
        }
        let nested = root.join("vehicles.rpf");
        vehicles.save(&nested).unwrap();
        let mut x64e = ArchiveEditor::new("x64e.rpf");
        x64e.add_file("levels/gta5/vehicles.rpf", std::fs::read(&nested).unwrap()).unwrap();
        x64e.save(&root.join("x64e.rpf")).unwrap();
        let mut update = ArchiveEditor::new("update.rpf");
        update.add_file("common/data/levels/gta5/vehicles.meta", meta(&[("vehshare", "vehicles_supergt_interior"), ("vehicles_supergt_interior", "adder")])).unwrap();
        std::fs::create_dir_all(root.join("update")).unwrap();
        update.save(&root.join("update/update.rpf")).unwrap();

        // A DLC pack added in the mods folder, linking to the game's dictionaries.
        let mut pack_vehicles = ArchiveEditor::new("mypack.rpf");
        pack_vehicles.add_file("mycar.ytd", vec![0; 16]).unwrap();
        pack_vehicles.save(&nested).unwrap();
        let mut dlc = ArchiveEditor::new("dlc.rpf");
        dlc.add_file("common/data/levels/gta5/vehicles.meta", meta(&[("vehicles_supergt_interior", "mycar"), ("mycar", "mycar")])).unwrap();
        dlc.add_file("x64/levels/gta5/vehicles/mypack.rpf", std::fs::read(&nested).unwrap()).unwrap();
        std::fs::create_dir_all(root.join("mods/update/x64/dlcpacks/mypack")).unwrap();
        dlc.save(&root.join("mods/update/x64/dlcpacks/mypack/dlc.rpf")).unwrap();

        let shared = SharedTextures::index(&game, None);
        assert_eq!(shared.parents_of("ADDER"), ["vehicles_supergt_interior", "vehshare"]);
        assert_eq!(shared.parents_of("mycar"), ["vehicles_supergt_interior", "vehshare"]);
        assert!(shared.parents_of("vehshare").is_empty());
        // Vehicles without a link still share vehshare's textures.
        assert_eq!(shared.parents_of("vehicles_supergt_interior"), ["vehshare"]);
        assert!(shared.parents_of("not_a_vehicle").is_empty());
        assert_eq!(shared.place("vehshare").unwrap().inner, ["levels", "gta5", "vehicles.rpf", "vehshare.ytd"]);
        let mine = shared.place("mycar").unwrap();
        assert_eq!(mine.read(None).unwrap(), vec![0; 16]);
        assert!(mine.disk.starts_with(root.join("mods")));
    }
}
