//! Add-on DLC packs: a mod's `dlc.rpf` installed as
//! `mods/update/x64/dlcpacks/<name>/dlc.rpf` and named in the mods copy's
//! DLC list (`update.rpf/common/data/dlclist.xml`), which the game reads to
//! know which packs to load.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use paleto_rpf::{Archive, ArchiveEditor, Keys};

use crate::budget::{Budget, Limits};
use crate::fsroot::{Dir, Fingerprint, GameRoots, Resolved};
use crate::game::{Edition, Game, resolve_ci};
use crate::relpath::RelPath;
use crate::loader::{DLC_LIST, add_dlc_packs, dlc_pack_order};
use crate::txn::{self, Staged, Txn};
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

/// Installs a pack under `name`: [`install_packs`] for one.
pub fn install_pack(game: &Game, keys: Option<&Keys>, dlc: &Path, name: &str, tool: Option<&Path>) -> Result<Vec<String>, Error> {
    install_packs(game, keys, &[(dlc.to_path_buf(), name.to_string())], tool)
}

/// Room asked for beyond the sizes, for the table of contents of a rebuilt `update.rpf`.
const SLACK: u64 = 1 << 20;

/// Installs packs, each under its name (replacing one of that name), and names them in the mods copy's DLC list
/// (made from the game's update.rpf if needed). One transaction, so an error at any step undoes the whole
/// install; the packs are put in place first and `update.rpf` last, so an interruption never leaves a pack
/// listed but missing (at worst one present but unlisted, which the next change undoes). Not atomic: the
/// files are put in place one after the other, see [`crate::txn`]. A pack already there byte for byte, and one
/// already listed, make no write at all. For the Enhanced edition, Legacy files inside a pack are converted
/// with `tool` when given; the file on disk is not changed. Returns a log.
pub fn install_packs(game: &Game, keys: Option<&Keys>, packs: &[(PathBuf, String)], tool: Option<&Path>) -> Result<Vec<String>, Error> {
    let mut names: Vec<String> = Vec::new();
    for (_, name) in packs {
        let name = name.to_ascii_lowercase();
        if !valid_name(&name) {
            return Err(Error::Package(format!("\"{name}\" is not a pack name: use letters, digits, '_' and '-'")));
        }
        if names.contains(&name) {
            return Err(Error::Package(format!("\"{name}\" is the name of more than one pack")));
        }
        names.push(name);
    }
    let roots = game.roots()?;
    // A conversion can take long: it runs before the lock is taken.
    let mut log = Vec::new();
    let mut prepared = Vec::new();
    for ((dlc, _), name) in packs.iter().zip(&names) {
        let Some(tool) = tool.filter(|_| game.edition == Edition::Enhanced) else {
            prepared.push(convert::Prepared::Disk(dlc.clone()));
            continue;
        };
        let shown = if packs.len() == 1 { "dlc.rpf".to_string() } else { format!("{name}/dlc.rpf") };
        let (mut data, converted) = convert::prepare(&[(dlc.clone(), shown)], tool, keys)?;
        log.extend(converted);
        prepared.push(data.remove(0));
    }
    // One change at a time per game, for the whole install; an interrupted one is dealt with first.
    let mut txn = Txn::begin(&roots, "installing add-on packs")?;
    let mut lines = txn.recovered().to_vec();
    lines.append(&mut log);
    let mut plan = match plan(&roots, keys, names, prepared) {
        Ok(plan) => plan,
        // Nothing is on disk yet but the journal: the abort ends it and says so.
        Err(e) => return Err(txn.abort(e, lines)),
    };
    // A pack can be gigabytes: it goes from its file into the new one a piece at a time.
    let budget = Budget::new(Limits::PACKAGE);
    let mut staged = Vec::new();
    for item in plan.items.iter_mut().filter(|i| !i.identical) {
        let Item { name, dest, source, .. } = item;
        match stage_with(&mut txn, dest, source.len(), |f| source.copy_to(&budget, f)) {
            Ok(s) => {
                txn.release(s);
                staged.push((s, name.clone()));
            }
            Err(e) => return Err(txn::failure(e, lines, "installed")),
        }
    }
    // The list last: it is the last file put in place.
    let mut listed = None;
    if let Some(List { mut dest, editor, base_len, added, spool }) = plan.list.take() {
        match stage_with(&mut txn, &mut dest, base_len.saturating_add(added).saturating_add(SLACK), |f| editor.save_to(f, &mut || scratch(spool.as_ref())).map(drop).map_err(Error::from)) {
            Ok(s) => {
                txn.release(s);
                listed = Some(s);
            }
            Err(e) => return Err(txn::failure(e, lines, "installed")),
        }
    }
    #[cfg(test)]
    if let Some(hook) = BEFORE_COMMIT.with(|h| h.borrow_mut().take()) {
        hook();
    }
    // A pack left alone because it was there already must still be what it was: the commit does not look at it.
    if let Err(e) = plan.items.iter().try_for_each(|i| still_identical(&roots, i)) {
        return Err(txn.abort(e, lines));
    }
    // Every pack must still open as an archive, and the list must name them all, before anything is put in place.
    let all: Vec<String> = plan.items.iter().map(|i| i.name.clone()).collect();
    let committed = txn.commit_validated(Vec::new(), |at, file| {
        let (rel, inner) = DLC_LIST;
        if let Some((_, name)) = staged.iter().find(|(s, _)| *s == at) {
            match ArchiveEditor::open_file(file.try_clone()?, "dlc.rpf", keys) {
                Ok(_) => {}
                // Encrypted, and no keys to read its table of contents with: all that is known is that the header is an
                // archive's, its table of contents (which opening checked to fit in the file) is not empty.
                Err(paleto_rpf::Error::KeysRequired) => {
                    let mut count = [0u8; 4];
                    file.read_exact_at(&mut count, 4)?;
                    if u32::from_le_bytes(count) == 0 {
                        return Err(Error::Package(format!("the dlc.rpf staged for {name} has an empty table of contents")));
                    }
                }
                Err(e) => return Err(Error::Package(format!("the dlc.rpf staged for {name} does not open as an archive: {e}"))),
            }
        } else if listed == Some(at) {
            let editor = ArchiveEditor::open_file(file.try_clone()?, "update.rpf", keys).map_err(|e| Error::Package(format!("the {rel} staged for the DLC list does not open as an archive: {e}")))?;
            let order = dlc_pack_order(&String::from_utf8_lossy(&editor.read(inner, keys)?));
            if let Some(missing) = all.iter().find(|n| !order.contains(n)) {
                return Err(Error::Package(format!("the DLC list staged in {rel} does not name {missing}")));
            }
        }
        Ok(())
    });
    match committed {
        Ok(notes) => {
            lines.extend(plan.items.into_iter().flat_map(|i| i.lines));
            lines.extend(notes);
            Ok(lines)
        }
        Err(e) => Err(txn::failure(e, lines, "installed")),
    }
}

/// The folder the new `update.rpf` goes to, if it exists.
fn spool_dir(update: &Resolved) -> Result<Option<Dir>, Error> {
    Ok(if update.missing.is_empty() { Some(update.parent.try_clone()?) } else { None })
}

/// An unnamed file next to the new `update.rpf`, for the spool of a rebuilt nested archive. Editing dlclist.xml
/// rebuilds none, so this is not asked for; if it is, it fails with a message rather than write elsewhere.
fn scratch(dir: Option<&Dir>) -> io::Result<File> {
    let Some(dir) = dir else { return Err(io::Error::other("no folder for a temporary file of the new update.rpf: mods/update does not exist yet")) };
    dir.tmpfile().map_err(|e| if Dir::unnamed_unsupported(&e) { io::Error::other("this filesystem has no unnamed temporary files, which the new update.rpf needs") } else { e })
}

// Runs once, just before the packs left alone are looked at again (tests change one there).
#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// What an install does, found with the transaction open and nothing staged.
struct Plan {
    items: Vec<Item>,
    /// The DLC list as it must be written; none if it names every pack already.
    list: Option<List>,
}

struct Item {
    name: String,
    dest: Resolved,
    source: Source,
    /// The pack there already is this one, byte for byte: nothing is staged for it.
    identical: bool,
    /// What the pack that is there looked like when it was found identical.
    seen: Option<Fingerprint>,
    /// What the log says of it, if the install succeeds.
    lines: Vec<String>,
}

struct List {
    dest: Resolved,
    /// `update.rpf` as it was read, with the new list in it.
    editor: ArchiveEditor,
    base_len: u64,
    /// The size of the new list.
    added: u64,
    /// The folder of `dest`, for the spool; none while it does not exist (the folder above is the game's or the mods folder).
    spool: Option<Dir>,
}

/// Reads what the install needs: the DLC list (the mods copy's, else the original's, read through a handle: no
/// copy is made), and for each pack whether it is there already. A missing or corrupt update.rpf fails here.
fn plan(roots: &GameRoots, keys: Option<&Keys>, names: Vec<String>, prepared: Vec<convert::Prepared>) -> Result<Plan, Error> {
    let (list_rel, inner) = DLC_LIST;
    let rel = RelPath::parse(list_rel)?;
    let update = roots.mods_dest(&rel)?;
    let (base, from_mods) = match roots.read_dest(&update)? {
        Some(file) => (file, true),
        None => (roots.original(&rel)?.ok_or_else(|| Error::Package(format!("{list_rel} not found in the game folder")))?, false),
    };
    let base_len = base.metadata()?.len();
    let mut editor = ArchiveEditor::open_file(base, &update.name, keys)?;
    let xml = String::from_utf8_lossy(&editor.read(inner, keys)?).into_owned();
    let listed = dlc_pack_order(&xml);
    // The game's own DLC: a pack of that name in the mods folder would take its place.
    let official = match (from_mods, roots.original(&rel)?) {
        (true, Some(original)) => dlc_pack_order(&String::from_utf8_lossy(&ArchiveEditor::open_file(original, &update.name, keys)?.read(inner, keys)?)),
        _ => listed.clone(),
    };
    if let Some(name) = names.iter().find(|n| official.contains(n)) {
        return Err(Error::Package(format!("{name} is the name of one of the game's own DLC packs (it is in the DLC list of the game's {list_rel}): a pack installed under it would replace that one; use another name")));
    }
    let missing: Vec<String> = names.iter().filter(|n| !listed.contains(n)).cloned().collect();
    let mut items = Vec::new();
    for (name, data) in names.into_iter().zip(prepared) {
        let dest = roots.mods_dest(&RelPath::parse(&format!("update/x64/dlcpacks/{name}/dlc.rpf"))?)?;
        let source = Source::open(data)?;
        let (identical, seen) = match roots.read_dest(&dest)? {
            Some(target) if source.same_as(&target)? => (true, Some(Fingerprint::of(&target)?)),
            _ => (false, None),
        };
        let installed = if identical { format!("{name}: already installed (identical)") } else { format!("installed {}", dest.path().display()) };
        let listing = if missing.contains(&name) { format!("added {name} to the DLC list of {}", update.path().display()) } else { format!("{name} is already in the DLC list") };
        items.push(Item { name, dest, source, identical, seen, lines: vec![installed, listing] });
    }
    let list = if missing.is_empty() {
        None
    } else {
        let new = add_dlc_packs(&xml, &missing)?.into_bytes();
        let added = new.len() as u64;
        editor.add_file(inner, new)?;
        let spool = spool_dir(&update)?;
        Some(List { dest: update, editor, base_len, added, spool })
    };
    Ok(Plan { items, list })
}

/// A pack that was found identical is still the same file, with the same ends, as when it was compared.
fn still_identical(roots: &GameRoots, item: &Item) -> Result<(), Error> {
    let Some(seen) = &item.seen else { return Ok(()) };
    match roots.read_dest(&item.dest) {
        Ok(Some(file)) if Fingerprint::of(&file)? == *seen => Ok(()),
        Ok(_) | Err(Error::Io(_)) => Err(Error::Conflict(format!("{} was changed by something else after it was found to be the pack to install; nothing was written", item.dest.path().display()))),
        Err(e) => Err(e),
    }
}

/// A pack to copy: held (a converted one that is small), or a file read from its start.
enum Source {
    Bytes(Vec<u8>),
    File(File, u64),
}

impl Source {
    /// The file is opened here and read through the handle from then on (a name that is replaced meanwhile is not followed).
    fn open(data: convert::Prepared) -> Result<Self, Error> {
        Ok(match data {
            convert::Prepared::Bytes(data) => Source::Bytes(data),
            convert::Prepared::File(file, len) => Source::File(file, len),
            convert::Prepared::Disk(path) => {
                // O_NONBLOCK: a pipe is not waited for, it is refused below.
                let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path)?;
                let meta = file.metadata()?;
                if !meta.is_file() {
                    return Err(Error::Package(format!("{} is not a file", path.display())));
                }
                Source::File(file, meta.len())
            }
        })
    }

    fn len(&self) -> u64 {
        match self {
            Source::Bytes(data) => data.len() as u64,
            Source::File(_, len) => *len,
        }
    }

    /// Whether `target` holds these bytes: the sizes first, then a comparison a piece at a time.
    fn same_as(&self, target: &File) -> io::Result<bool> {
        if target.metadata()?.len() != self.len() {
            return Ok(false);
        }
        let mut target = target;
        target.seek(SeekFrom::Start(0))?;
        match self {
            Source::Bytes(data) => same_bytes(&mut data.as_slice(), &mut target, self.len()),
            Source::File(file, len) => {
                let mut file = file;
                file.seek(SeekFrom::Start(0))?;
                same_bytes(&mut file, &mut target, *len)
            }
        }
    }

    fn copy_to(&self, budget: &Budget, out: &mut File) -> Result<(), Error> {
        match self {
            Source::Bytes(data) => out.write_all(data)?,
            Source::File(file, len) => {
                let mut file = file;
                file.seek(SeekFrom::Start(0))?;
                budget.copy(&mut file, *len, out, "dlc.rpf")?;
            }
        }
        Ok(())
    }
}

/// Whether the next `len` bytes of both readers are the same.
fn same_bytes(a: &mut impl Read, b: &mut impl Read, len: u64) -> io::Result<bool> {
    let (mut x, mut y) = (vec![0u8; 256 * 1024], vec![0u8; 256 * 1024]);
    let mut left = len;
    while left > 0 {
        let n = left.min(x.len() as u64) as usize;
        a.read_exact(&mut x[..n])?;
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
        left -= n as u64;
    }
    Ok(true)
}

/// [`Txn::stage_sized`] for a `write` that fails with an [`Error`] of its own: that error, not the I/O error it
/// had to be turned into for the engine, is the cause of what comes back.
fn stage_with(txn: &mut Txn, dest: &mut Resolved, need: u64, write: impl FnOnce(&mut File) -> Result<(), Error>) -> Result<Staged, Error> {
    let mut failed = None;
    let staged = txn.stage_sized(dest, need, |f| write(f).map_err(|e| io::Error::other(failed.insert(e).to_string())));
    staged.map_err(|e| match (failed, e) {
        (Some(cause), Error::Txn(mut f)) => {
            f.cause = cause;
            Error::Txn(f)
        }
        (Some(cause), _) => cause,
        (None, e) => e,
    })
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

    #[test]
    fn a_converted_pack_is_copied_from_its_file() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let dlc = dir.path().join("dlc.rpf");
        let mut ed = ArchiveEditor::new("dlc.rpf");
        ed.archive("x64/vehicles.rpf", true, None).unwrap().add_file("car.yft", res(162)).unwrap();
        ed.save(&dlc).unwrap();
        let original = std::fs::read(&dlc).unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
        let game = Game::open(&root).unwrap();
        let log = install_pack(&game, None, &dlc, "car", Some(&convert::tests::fake_tool(dir.path()))).unwrap();
        assert_eq!(log[0], "converted dlc.rpf/x64/vehicles.rpf/car.yft");
        let mut installed = ArchiveEditor::open(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), None).unwrap();
        assert_eq!(installed.archive("x64/vehicles.rpf", false, None).unwrap().read("car.yft", None).unwrap(), res(171));
        assert_eq!(std::fs::read(&dlc).unwrap(), original, "the pack on disk is left alone");
    }

    #[test]
    fn the_spool_of_the_new_list_is_in_its_folder_or_an_error_that_says_so() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5.exe"), b"game").unwrap();
        let roots = Game::open(&root).unwrap().roots().unwrap();
        let rel = RelPath::parse("update/update.rpf").unwrap();
        // No mods folder: the deepest folder there is the game's, which gets no file.
        let spool = spool_dir(&roots.mods_dest(&rel).unwrap()).unwrap();
        assert!(spool.is_none());
        let err = scratch(spool.as_ref()).unwrap_err().to_string();
        assert!(err.contains("mods/update does not exist"), "{err}");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "something was made in the game folder");
        std::fs::create_dir_all(root.join("mods/update")).unwrap();
        let spool = spool_dir(&roots.mods_dest(&rel).unwrap()).unwrap();
        let file = scratch(spool.as_ref()).unwrap();
        assert_eq!(file.metadata().unwrap().nlink(), 0, "the spool has a name");
        assert_eq!(std::fs::read_dir(root.join("mods/update")).unwrap().count(), 0);
    }

    #[test]
    fn several_packs_are_converted_and_named_in_the_log_by_pack() {
        let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
        let dir = tempfile::tempdir().unwrap();
        let (car, plain) = (dir.path().join("car/dlc.rpf"), dir.path().join("plain/dlc.rpf"));
        std::fs::create_dir_all(car.parent().unwrap()).unwrap();
        let mut ed = ArchiveEditor::new("dlc.rpf");
        ed.archive("x64/vehicles.rpf", true, None).unwrap().add_file("car.yft", res(162)).unwrap();
        ed.save(&car).unwrap();
        pack(&plain, "plain");
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        update_rpf(&root.join("update/update.rpf"), &["mpheist"]);
        let game = Game::open(&root).unwrap();
        let tool = convert::tests::fake_tool(dir.path());
        // A lock held by somebody else: the install is refused, and writes nothing.
        let roots = game.roots().unwrap();
        let held = crate::txn::guard(&roots, "a test").unwrap();
        assert!(matches!(install_packs(&game, None, &[(car.clone(), "car".into()), (plain.clone(), "plain".into())], Some(&tool)), Err(Error::Busy(_))));
        drop(held);
        let log = install_packs(&game, None, &[(car, "car".into()), (plain, "plain".into())], Some(&tool)).unwrap();
        assert_eq!(log[0], "converted car/dlc.rpf/x64/vehicles.rpf/car.yft", "{log:?}");
        let mut installed = ArchiveEditor::open(&root.join("mods/update/x64/dlcpacks/car/dlc.rpf"), None).unwrap();
        assert_eq!(installed.archive("x64/vehicles.rpf", false, None).unwrap().read("car.yft", None).unwrap(), res(171));
        let list = ArchiveEditor::open(&root.join("mods/update/update.rpf"), None).unwrap().read("common/data/dlclist.xml", None).unwrap();
        let list = String::from_utf8(list).unwrap();
        assert!(list.contains("dlcpacks:/car/") && list.contains("dlcpacks:/plain/"), "{list}");
    }
}
