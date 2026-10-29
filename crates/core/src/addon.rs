//! Add-on DLC packs: a mod's `dlc.rpf` installed as
//! `mods/update/x64/dlcpacks/<name>/dlc.rpf` and named in the mods copy's
//! DLC list (`update.rpf/common/data/dlclist.xml`), which the game reads to
//! know which packs to load.

use std::io::Write;
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::game::{Edition, Game, resolve_ci};
use crate::relpath::RelPath;
use crate::loader::{DLC_LIST, add_dlc_packs, dlc_pack_order};
use crate::{Error, convert};

/// A `dlc.rpf` found among dropped or chosen files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub dlc: PathBuf,
    /// The name it would be installed under (see [`guess_name`]).
    pub name: String,
}

/// The `dlc.rpf` files among `paths`: the files themselves, or inside the
/// folders (searched through).
pub fn find_packs(paths: &[PathBuf], keys: Option<&Keys>) -> Vec<Pack> {
    let mut found = Vec::new();
    for path in paths {
        let files = if path.is_dir() { crate::vfs::local_files(path).unwrap_or_default() } else { vec![(path.clone(), String::new())] };
        for (file, _) in files {
            if file.file_name().is_some_and(|n| n.eq_ignore_ascii_case("dlc.rpf")) {
                found.push(Pack { name: guess_name(&file, keys), dlc: file });
            }
        }
    }
    found
}

/// Folders a `dlc.rpf` is often left in that say nothing of the pack.
const GENERIC: &[&str] = &["downloads", "download", "desktop", "tmp", "temp", "dlcpacks", "x64", "update", "mods"];

/// The name to install a pack under: its folder's name, as mods ship them
/// ("dlcpacks/piccolo/dlc.rpf"), unless that folder says nothing (Downloads);
/// then the pack's own name in its setup2.xml; else "addon".
pub fn guess_name(dlc: &Path, keys: Option<&Keys>) -> String {
    let folder = dlc.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().to_ascii_lowercase());
    if let Some(name) = folder.filter(|n| valid_name(n) && !GENERIC.contains(&n.as_str())) {
        return name;
    }
    setup_name(dlc, keys).unwrap_or_else(|| "addon".into())
}

/// The `<nameHash>` of a pack's setup2.xml, in lower case.
fn setup_name(dlc: &Path, keys: Option<&Keys>) -> Option<String> {
    let archive = Archive::open(dlc, keys).ok()?;
    let (_, idx) = archive.walk(0).into_iter().find(|(p, _)| p.eq_ignore_ascii_case("setup2.xml"))?;
    let xml = String::from_utf8_lossy(&archive.read(idx, keys).ok()?).into_owned();
    let start = xml.find("<nameHash>")? + "<nameHash>".len();
    let name = xml[start..].split('<').next()?.trim().to_ascii_lowercase();
    valid_name(&name).then_some(name)
}

/// Pack folder names: letters, digits, '_' and '-'.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Where a pack of this name goes in the mods folder.
pub fn pack_path(game: &Game, name: &str) -> PathBuf {
    let rel = format!("update/x64/dlcpacks/{name}/dlc.rpf");
    RelPath::parse(&rel).and_then(|p| game.roots()?.mods_dest(&p)).map_or_else(|_| resolve_ci(&game.mods_dir(), &rel), |dest| dest.path())
}

/// Installs a pack under `name` (replacing one of that name) and names it in
/// the mods copy's DLC list (made from the game's update.rpf if needed).
/// For the Enhanced edition, Legacy files inside it are converted with `tool`
/// when given; the file on disk is not changed. Returns a log.
pub fn install_pack(game: &Game, keys: Option<&Keys>, dlc: &Path, name: &str, tool: Option<&Path>) -> Result<Vec<String>, Error> {
    let name = name.to_ascii_lowercase();
    if !valid_name(&name) {
        return Err(Error::Package(format!("\"{name}\" is not a pack name: use letters, digits, '_' and '-'")));
    }
    let roots = game.roots()?;
    let mut dest = roots.mods_dest(&RelPath::parse(&format!("update/x64/dlcpacks/{name}/dlc.rpf"))?)?;
    let mut log = Vec::new();
    let data = match tool.filter(|_| game.edition == Edition::Enhanced) {
        Some(tool) => {
            let (mut data, converted) = convert::read_converting(&[(dlc.to_path_buf(), "dlc.rpf".into())], tool, keys)?;
            log.extend(converted);
            data.remove(0)
        }
        None => std::fs::read(dlc)?,
    };
    roots.replace(&mut dest, |f| f.write_all(&data))?;
    log.push(format!("installed {}", dest.path().display()));

    let (rel, inner) = DLC_LIST;
    let update = roots.mods_copy(&RelPath::parse(rel)?)?.ok_or_else(|| Error::Package(format!("{rel} not found in the game folder")))?;
    let mut editor = ArchiveEditor::open(&update, keys)?;
    let xml = String::from_utf8_lossy(&editor.read(inner, keys)?).into_owned();
    if dlc_pack_order(&xml).contains(&name) {
        log.push(format!("{name} is already in the DLC list"));
    } else {
        editor.add_file(inner, add_dlc_packs(&xml, std::slice::from_ref(&name))?.into_bytes())?;
        editor.save(&update)?;
        log.push(format!("added {name} to the DLC list of {}", update.display()));
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_rpf(path: &Path, packs: &[&str]) {
        let items: String = packs.iter().map(|p| format!("\t\t<Item>dlcpacks:/{p}/</Item>\n")).collect();
        let mut ed = ArchiveEditor::new("update.rpf");
        ed.add_file("common/data/dlclist.xml", format!("<SMandatoryPacksData>\n\t<Paths>\n{items}\t</Paths>\n</SMandatoryPacksData>\n").into_bytes()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        ed.save(path).unwrap();
    }

    fn pack(path: &Path, name_hash: &str) {
        let mut ed = ArchiveEditor::new("dlc.rpf");
        ed.add_file("setup2.xml", format!("<SSetupData><nameHash>{name_hash}</nameHash></SSetupData>").into_bytes()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        ed.save(path).unwrap();
    }

    #[test]
    fn finds_and_installs_packs() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("Downloads");
        pack(&downloads.join("SuperCar/dlcpacks/supercar/dlc.rpf"), "SuperCar");
        pack(&downloads.join("dlc.rpf"), "Piccolo");
        std::fs::write(downloads.join("readme.txt"), b"").unwrap();
        let packs = find_packs(&[downloads.join("SuperCar"), downloads.join("dlc.rpf"), downloads.join("readme.txt")], None);
        let names: Vec<&str> = packs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["supercar", "piccolo"]);

        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
        let game = Game::open(&root).unwrap();
        let log = install_pack(&game, None, &packs[1].dlc, "piccolo", None).unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(std::fs::read(root.join("mods/update/x64/dlcpacks/piccolo/dlc.rpf")).unwrap(), std::fs::read(&packs[1].dlc).unwrap());
        let list = |path: &Path| {
            let ed = ArchiveEditor::open(path, None).unwrap();
            String::from_utf8(ed.read("common/data/dlclist.xml", None).unwrap()).unwrap()
        };
        assert!(list(&root.join("mods/update/update.rpf")).contains("<Item>dlcpacks:/mpheist/</Item>\n\t\t<Item>dlcpacks:/piccolo/</Item>"));
        assert!(!list(&root.join("update/update.rpf")).contains("piccolo"), "the game's own list is untouched");
        // Installing again replaces the pack and does not list it twice.
        let log = install_pack(&game, None, &packs[1].dlc, "Piccolo", None).unwrap();
        assert_eq!(log[1], "piccolo is already in the DLC list");
        assert_eq!(list(&root.join("mods/update/update.rpf")).matches("piccolo").count(), 1);
        assert!(install_pack(&game, None, &packs[1].dlc, "my car", None).is_err());
    }
}
