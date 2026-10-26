//! Editing RPF7 archives: add, replace and delete files, edit nested archives.
//!
//! Saved archives get a plain ("OPEN") table of contents, like OpenIV writes
//! them, except AES archives: their table of contents is encrypted again, so
//! their AES-encrypted files still read as such. Files that are not touched
//! keep their bytes, encryption included.
//!
//! A top-level archive is saved in place: new data goes into free gaps or at the
//! end, never over anything the old table of contents still points to, and the
//! table of contents is written last. An interrupted save therefore leaves the
//! old archive readable. Edited nested archives are rebuilt compactly.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::write::DeflateEncoder;

use crate::archive::{AES, BLOCK, DIR_IDENT, MAGIC, OPEN, RSC7_MAGIC, decode};
use crate::crypto::encrypt_aes;
use crate::{Archive, Encryption, Entry, EntryKind, Error, Keys};

/// Extensions the game expects to be stored without compression.
const STORED_RAW: [&str; 3] = ["rpf", "awc", "bik"];
const MAX_SIZE_FIELD: u32 = 0xFF_FFFF;

pub struct ArchiveEditor {
    name: String,
    root: Dir,
    /// Where the archive was read from, for saving in place.
    source: Option<Source>,
    /// Absolute byte ranges of the original archive that must stay untouched
    /// until the new table of contents is written.
    reserved: Vec<(u64, u64)>,
    /// The key of an AES archive, to write its table of contents with.
    aes: Option<[u8; 32]>,
    defragment: bool,
}

struct Source {
    file: Arc<File>,
    base: u64,
    len: u64,
}

#[derive(Default)]
struct Dir {
    children: BTreeMap<String, Node>,
}

enum Node {
    Dir(Dir),
    File(FileNode),
    Archive(Box<ArchiveEditor>),
}

struct FileNode {
    /// Entry fields; the offset is assigned on save.
    kind: EntryKind,
    data: Data,
}

/// The bytes of a file exactly as stored in the archive.
enum Data {
    Stored { file: Arc<File>, offset: u64, len: u64 },
    Bytes(Vec<u8>),
}

impl Data {
    fn len(&self) -> u64 {
        match self {
            Data::Stored { len, .. } => *len,
            Data::Bytes(b) => b.len() as u64,
        }
    }

    fn read(&self) -> Result<Vec<u8>, Error> {
        match self {
            Data::Stored { file, offset, len } => {
                let mut buf = vec![0; *len as usize];
                file.read_exact_at(&mut buf, *offset)?;
                Ok(buf)
            }
            Data::Bytes(b) => Ok(b.clone()),
        }
    }
}

impl ArchiveEditor {
    /// A new, empty archive.
    pub fn new(name: &str) -> Self {
        ArchiveEditor {
            name: name.to_ascii_lowercase(),
            root: Dir::default(),
            source: None,
            reserved: Vec::new(),
            aes: None,
            defragment: false,
        }
    }

    /// Opens an archive on disk for editing; save it with [`ArchiveEditor::save`].
    pub fn open(path: &Path, keys: Option<&Keys>) -> Result<Self, Error> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let size = file.metadata()?.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let archive = Archive::parse(Arc::new(file), name, 0, size, keys)?;
        Ok(Self::from_archive(&archive, keys))
    }

    /// `keys` are the ones the archive was read with (an AES archive needs them).
    fn from_archive(archive: &Archive, keys: Option<&Keys>) -> Self {
        let mut editor = Self::new(&archive.name);
        editor.aes = keys.filter(|_| archive.encryption == Encryption::Aes).map(|k| k.aes);
        editor.source = Some(Source { file: archive.file.clone(), base: archive.base, len: archive.size });
        editor.reserved.push((archive.base, archive.base + archive.toc_len));
        let mut stack = vec![(0usize, Vec::<String>::new())];
        while let Some((dir, prefix)) = stack.pop() {
            for i in archive.children(dir) {
                let entry = &archive.entries[i];
                let mut path = prefix.clone();
                path.push(entry.name.to_ascii_lowercase());
                if entry.is_dir() {
                    editor.insert(&path, Node::Dir(Dir::default())).ok();
                    stack.push((i, path));
                    continue;
                }
                let offset = archive.base + entry.offset().unwrap() as u64 * BLOCK;
                let len = entry.stored_len();
                editor.reserved.push((offset, offset + len));
                let data = Data::Stored { file: archive.file.clone(), offset, len };
                editor.insert(&path, Node::File(FileNode { kind: entry.kind.clone(), data })).ok();
            }
        }
        editor
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds or replaces a file. `data` is the standalone file: resources
    /// (RSC7) are detected from their header.
    pub fn add_file(&mut self, path: &str, data: Vec<u8>) -> Result<(), Error> {
        let parts = split(path)?;
        let node = FileNode::from_file(parts.last().unwrap(), data)?;
        self.insert(&parts, Node::File(node))
    }

    /// Deletes a file or folder. Returns false if it does not exist.
    pub fn delete(&mut self, path: &str) -> Result<bool, Error> {
        let parts = split(path)?;
        let (last, dirs) = parts.split_last().unwrap();
        Ok(self.dir_mut(dirs, false)?.and_then(|d| d.children.remove(last)).is_some())
    }

    /// Creates a folder and any missing parents; an existing folder is fine.
    pub fn create_dir(&mut self, path: &str) -> Result<(), Error> {
        self.dir_mut(&split(path)?, true)?;
        Ok(())
    }

    /// Renames a file or folder inside its folder.
    ///
    /// NG encryption is keyed by the file name, so NG-encrypted files are
    /// decrypted (which needs `keys`) and stored unencrypted under the new name,
    /// and a nested archive with an NG table of contents is written again.
    pub fn rename(&mut self, path: &str, new_name: &str, keys: Option<&Keys>) -> Result<(), Error> {
        let parts = split(path)?;
        let new_name = match split(new_name)?.as_slice() {
            [name] => name.clone(),
            _ => return Err(Error::InvalidPath(new_name.to_string())),
        };
        let (last, dirs) = parts.split_last().unwrap();
        let aes = self.aes.is_some();
        let dir = self.dir_mut(dirs, false)?.ok_or_else(|| Error::NotFound(path.to_string()))?;
        let ng_archive = match dir.children.get(last) {
            None => return Err(Error::NotFound(path.to_string())),
            Some(Node::File(f)) => f.is_ng_archive()?,
            Some(_) => false,
        };
        if *last == new_name {
            return Ok(());
        }
        if dir.children.contains_key(&new_name) {
            return Err(Error::AlreadyExists(new_name));
        }
        if ng_archive {
            self.archive(path, false, keys)?;
        }
        let dir = self.dir_mut(dirs, false)?.ok_or_else(|| Error::NotFound(path.to_string()))?;
        let reencoded = match &dir.children[last] {
            Node::File(f) if matches!(f.kind, EntryKind::Resource { .. }) && (is_script(last) || is_script(&new_name)) => {
                // Scripts are encrypted by name too, and resources carry no
                // flag saying whether they are.
                return Err(Error::Unsupported("renaming .ysc scripts"));
            }
            Node::File(f) if !aes && matches!(f.kind, EntryKind::Binary { encrypted: true, .. }) => {
                let entry = Entry { name: last.clone(), kind: f.kind.clone() };
                let data = decode(&entry, f.data.read()?, aes, keys)?;
                Some(Node::File(FileNode::from_file(&new_name, data)?))
            }
            _ => None,
        };
        let mut node = dir.children.remove(last).unwrap();
        if let Some(reencoded) = reencoded {
            node = reencoded;
        }
        if let Node::Archive(a) = &mut node {
            a.name = new_name.clone();
        }
        dir.children.insert(new_name, node);
        Ok(())
    }

    pub fn exists(&self, path: &str) -> bool {
        split(path).is_ok_and(|p| self.node(&p).is_some())
    }

    /// Reads a file as a standalone file (see [`Archive::read`]).
    pub fn read(&self, path: &str, keys: Option<&Keys>) -> Result<Vec<u8>, Error> {
        let parts = split(path)?;
        match self.node(&parts) {
            Some(Node::File(f)) => {
                let entry = Entry { name: parts.last().unwrap().clone(), kind: f.kind.clone() };
                decode(&entry, f.data.read()?, self.aes.is_some(), keys)
            }
            Some(Node::Archive(_)) => Err(Error::Unsupported("reading an archive that is being edited")),
            _ => Err(Error::NotFound(path.to_string())),
        }
    }

    /// Paths of all files and folders, folders ending with '/'.
    pub fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), &self.root)];
        while let Some((prefix, dir)) = stack.pop() {
            for (name, node) in &dir.children {
                match node {
                    Node::Dir(d) => {
                        out.push(format!("{prefix}{name}/"));
                        stack.push((format!("{prefix}{name}/"), d));
                    }
                    _ => out.push(format!("{prefix}{name}")),
                }
            }
        }
        out.sort();
        out
    }

    /// Opens a nested archive for editing, optionally creating it.
    pub fn archive(&mut self, path: &str, create: bool, keys: Option<&Keys>) -> Result<&mut ArchiveEditor, Error> {
        let parts = split(path)?;
        let (last, dirs) = parts.split_last().unwrap();
        let dir = self.dir_mut(dirs, create)?.ok_or_else(|| Error::NotFound(path.to_string()))?;
        // The file stays in place until it has been read as an archive.
        let node = match dir.children.get(last) {
            Some(Node::File(f)) => {
                if !matches!(f.kind, EntryKind::Binary { size: 0, encrypted: false, .. }) {
                    return Err(Error::NotAnArchive(last.clone()));
                }
                let (file, offset, len) = match &f.data {
                    Data::Stored { file, offset, len } => (file.clone(), *offset, *len),
                    Data::Bytes(bytes) => {
                        let mut tmp = tempfile::tempfile()?;
                        tmp.write_all(bytes)?;
                        (Arc::new(tmp), 0, bytes.len() as u64)
                    }
                };
                let archive = Archive::parse(file, last.clone(), offset, len, keys)?;
                Some(Node::Archive(Box::new(Self::from_archive(&archive, keys))))
            }
            Some(Node::Archive(_)) => None,
            Some(_) => return Err(Error::NotAnArchive(last.clone())),
            None if create => Some(Node::Archive(Box::new(Self::new(last)))),
            None => return Err(Error::NotFound(path.to_string())),
        };
        if let Some(node) = node {
            dir.children.insert(last.clone(), node);
        }
        match dir.children.get_mut(last) {
            Some(Node::Archive(a)) => Ok(a),
            _ => unreachable!(),
        }
    }

    /// Rebuilds the archive compactly when saved, dropping unused space.
    pub fn defragment(&mut self) {
        self.defragment = true;
    }

    /// Saves to `path`. In place when `path` is the archive this editor was
    /// opened from, otherwise a fresh archive is written.
    pub fn save(mut self, path: &Path) -> Result<(), Error> {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let source = self.source.as_ref().filter(|s| !self.defragment && s.base == 0 && same_file(&s.file, path));
        if let Some(file) = source.map(|s| s.file.clone()) {
            self.write(&file, 0, true, None, &dir)?;
            file.sync_data()?;
        } else {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::NamedTempFile::new_in(&dir)?;
            self.write(&Arc::new(tmp.as_file().try_clone()?), 0, false, None, &dir)?;
            tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o644))?;
            tmp.as_file().sync_data()?;
            tmp.persist(path).map_err(|e| e.error)?;
        }
        Ok(())
    }

    /// Writes the archive at `base` in `target` and returns its size.
    ///
    /// In place, data already in `target` stays where it is and new data only
    /// goes into space the old archive does not use. `limit` caps the size;
    /// exceeding it gives [`Error::NoSpace`] before anything of this archive
    /// itself is written.
    fn write(&mut self, target: &Arc<File>, base: u64, in_place: bool, limit: Option<u64>, tmp_dir: &Path) -> Result<u64, Error> {
        let toc_blocks = {
            let (entries, _) = self.flatten();
            let (names, _) = names_table(&entries)?;
            (16 + 16 * entries.len() as u64 + names.len() as u64).div_ceil(BLOCK)
        };
        let limit_blocks = limit.map(|l| l / BLOCK);

        // Space in use, in blocks from `base`: the new table of contents and,
        // in place, everything the old table of contents points to.
        let mut used: Vec<(u64, u64)> = vec![(0, toc_blocks)];
        if in_place {
            used.extend(self.reserved.iter().map(|&(s, e)| ((s - base) / BLOCK, (e - base).div_ceil(BLOCK))));
        }
        // Space that must never be overwritten: the new table of contents and
        // nested archives already written in place.
        let mut protected = vec![(0, toc_blocks)];

        let mut nested = Vec::new();
        let mut files = Vec::new();
        let mut stack = vec![&mut self.root];
        while let Some(dir) = stack.pop() {
            for node in dir.children.values_mut() {
                match node {
                    Node::Dir(d) => stack.push(d),
                    Node::Archive(_) => nested.push(node),
                    Node::File(f) => files.push(f),
                }
            }
        }

        // Other nested archives still to be edited keep their current place.
        for node in &nested {
            if let Node::Archive(child) = &**node
                && let Some(src) = child.source.as_ref().filter(|s| in_place && Arc::ptr_eq(&s.file, target) && s.base >= base) {
                    let start = (src.base - base) / BLOCK;
                    protected.push((start, start + src.len.div_ceil(BLOCK)));
                }
        }

        // Edited nested archives are edited in place when they can grow into
        // the space after them, if need be by moving the files stored there
        // (as OpenIV does); otherwise they are rebuilt into a temp file and
        // placed like new data.
        for node in nested {
            let Node::Archive(child) = node else { unreachable!() };
            let mut written = None;
            let here = child.source.as_ref().filter(|s| {
                in_place && !child.defragment && Arc::ptr_eq(&s.file, target) && s.base >= base + toc_blocks * BLOCK
            });
            if let Some((file, child_base, old_len)) = here.map(|s| (s.file.clone(), s.base, s.len)) {
                let start = (child_base - base) / BLOCK;
                let old_end = start + old_len.div_ceil(BLOCK);
                let next = used.iter().map(|r| r.0).filter(|&s| s >= old_end).min().or(limit_blocks);
                let mut result = child.write(&file, child_base, true, next.map(|n| (n - start) * BLOCK), tmp_dir);
                if let Err(Error::NoSpace { needed }) = result {
                    let need_end = start + needed.div_ceil(BLOCK);
                    let overlaps = |r: &(u64, u64)| r.0 < need_end && r.1 > old_end;
                    let blockers: Vec<usize> = (0..files.len())
                        .filter(|&i| match &files[i].data {
                            Data::Stored { file: f, offset, len } if Arc::ptr_eq(f, target) && *offset >= base => {
                                let b = (offset - base) / BLOCK;
                                overlaps(&(b, b + len.div_ceil(BLOCK)))
                            }
                            _ => false,
                        })
                        .collect();
                    let moved: u64 = blockers.iter().map(|&i| files[i].data.len()).sum();
                    let fits = limit_blocks.is_none_or(|l| need_end <= l) && !protected.iter().any(overlaps);
                    if fits && moved < old_len {
                        for &i in &blockers {
                            let tmp = Arc::new(tempfile::tempfile_in(tmp_dir)?);
                            copy_data(&files[i].data, &tmp, 0)?;
                            let len = files[i].data.len();
                            files[i].data = Data::Stored { file: tmp, offset: 0, len };
                        }
                        used.retain(|r| !overlaps(r));
                        result = child.write(&file, child_base, true, Some((need_end - start) * BLOCK), tmp_dir);
                    }
                }
                match result {
                    Ok(len) => {
                        let range = (start, start + len.div_ceil(BLOCK));
                        used.push(range);
                        protected.push(range);
                        written = Some((file, child_base, len));
                    }
                    Err(Error::NoSpace { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            let (file, offset, len) = match written {
                Some(w) => w,
                None => {
                    let tmp = Arc::new(tempfile::tempfile_in(tmp_dir)?);
                    let len = child.write(&tmp, 0, false, None, tmp_dir)?;
                    (tmp, 0, len)
                }
            };
            let uncompressed_size = u32::try_from(len).map_err(|_| Error::TooLarge(child.name.clone()))?;
            *node = Node::File(FileNode {
                kind: EntryKind::Binary { offset: 0, size: 0, uncompressed_size, encrypted: false },
                data: Data::Stored { file, offset, len },
            });
        }

        let (mut entries, files) = self.flatten();
        let (names, name_offsets) = names_table(&entries)?;
        let mut alloc = Allocator::new(used);
        let mut placed = Vec::with_capacity(files.len());
        for (idx, node) in &files {
            let kept = match &node.data {
                Data::Stored { file, offset, .. } if in_place && Arc::ptr_eq(file, target) && *offset >= base + toc_blocks * BLOCK => {
                    Some((offset - base) / BLOCK)
                }
                _ => None,
            };
            let block = kept.unwrap_or_else(|| alloc.alloc(node.data.len().div_ceil(BLOCK)));
            placed.push((*idx, block, kept.is_some()));
        }
        let end = alloc.end();
        if limit_blocks.is_some_and(|l| end > l) {
            return Err(Error::NoSpace { needed: end * BLOCK });
        }

        // Data first, table of contents last.
        for ((idx, node), &(_, block, kept)) in files.iter().zip(&placed) {
            let entry = &mut entries[*idx];
            set_offset(&mut entry.kind, block, &entry.name)?;
            if !kept {
                copy_data(&node.data, target, base + block * BLOCK)?;
            }
        }
        if target.metadata()?.len() < base + end * BLOCK {
            target.set_len(base + end * BLOCK)?;
        }

        let mut toc = Vec::with_capacity((toc_blocks * BLOCK) as usize);
        let encryption = if self.aes.is_some() { AES } else { OPEN };
        for w in [MAGIC, entries.len() as u32, names.len() as u32, encryption] {
            toc.extend_from_slice(&w.to_le_bytes());
        }
        for (e, &off) in entries.iter().zip(&name_offsets) {
            toc.extend_from_slice(&encode_entry(e, off));
        }
        toc.extend_from_slice(&names);
        if let Some(key) = &self.aes {
            // Entries and names separately, as they are read.
            let (entries_end, names_end) = (16 + 16 * entries.len(), 16 + 16 * entries.len() + names.len());
            encrypt_aes(key, &mut toc[16..entries_end]);
            encrypt_aes(key, &mut toc[entries_end..names_end]);
        }
        toc.resize((toc_blocks * BLOCK) as usize, 0);
        target.write_all_at(&toc, base)?;
        Ok(end * BLOCK)
    }

    /// Entries in table-of-contents order (each folder's children contiguous
    /// and sorted by name), with their name offsets still unset.
    fn flatten(&self) -> (Vec<Entry>, Vec<(usize, &FileNode)>) {
        let mut entries = vec![Entry { name: String::new(), kind: EntryKind::Directory { first: 0, count: 0 } }];
        let mut files = Vec::new();
        let mut queue = VecDeque::from([(0usize, &self.root)]);
        while let Some((idx, dir)) = queue.pop_front() {
            let first = entries.len() as u32;
            for (name, node) in &dir.children {
                let i = entries.len();
                let kind = match node {
                    Node::Dir(d) => {
                        queue.push_back((i, d));
                        EntryKind::Directory { first: 0, count: 0 }
                    }
                    Node::File(f) => {
                        files.push((i, f));
                        f.kind.clone()
                    }
                    // Placeholder, only while sizing the table of contents.
                    Node::Archive(_) => EntryKind::Binary { offset: 0, size: 0, uncompressed_size: 0, encrypted: false },
                };
                entries.push(Entry { name: name.clone(), kind });
            }
            entries[idx].kind = EntryKind::Directory { first, count: dir.children.len() as u32 };
        }
        (entries, files)
    }

    fn node(&self, parts: &[String]) -> Option<&Node> {
        let (last, dirs) = parts.split_last()?;
        let mut dir = &self.root;
        for part in dirs {
            match dir.children.get(part)? {
                Node::Dir(d) => dir = d,
                _ => return None,
            }
        }
        dir.children.get(last)
    }

    fn dir_mut(&mut self, parts: &[String], create: bool) -> Result<Option<&mut Dir>, Error> {
        let mut dir = &mut self.root;
        for part in parts {
            if create {
                dir.children.entry(part.clone()).or_insert_with(|| Node::Dir(Dir::default()));
            }
            dir = match dir.children.get_mut(part) {
                Some(Node::Dir(d)) => d,
                Some(_) => return Err(Error::NotADirectory(part.clone())),
                None => return Ok(None),
            };
        }
        Ok(Some(dir))
    }

    fn insert(&mut self, parts: &[String], node: Node) -> Result<(), Error> {
        let (last, dirs) = parts.split_last().unwrap();
        let dir = self.dir_mut(dirs, true)?.unwrap();
        if matches!(node, Node::Dir(_)) && dir.children.contains_key(last) {
            return Ok(());
        }
        dir.children.insert(last.clone(), node);
        Ok(())
    }
}

impl FileNode {
    /// Whether this is a nested archive whose table of contents is NG-encrypted.
    fn is_ng_archive(&self) -> Result<bool, Error> {
        if !matches!(self.kind, EntryKind::Binary { size: 0, encrypted: false, .. }) {
            return Ok(false);
        }
        let mut head = [0u8; 16];
        match &self.data {
            Data::Stored { file, offset, len } if *len >= 16 => file.read_exact_at(&mut head, *offset)?,
            Data::Bytes(bytes) if bytes.len() >= 16 => head.copy_from_slice(&bytes[..16]),
            _ => return Ok(false),
        }
        let word = |i: usize| u32::from_le_bytes(head[i * 4..i * 4 + 4].try_into().unwrap());
        Ok(word(0) == MAGIC && Encryption::from_u32(word(3)) == Encryption::Ng)
    }

    fn from_file(name: &str, mut data: Vec<u8>) -> Result<Self, Error> {
        if data.len() >= 16 && data[..4] == RSC7_MAGIC.to_le_bytes() {
            let word = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
            let (system_flags, graphics_flags) = (word(8), word(12));
            let len = u32::try_from(data.len()).map_err(|_| Error::TooLarge(name.to_string()))?;
            let size = if len >= MAX_SIZE_FIELD {
                // The game reads oversized resource sizes from these header bytes.
                let b = len.to_le_bytes();
                (data[7], data[14], data[5], data[2]) = (b[0], b[1], b[2], b[3]);
                MAX_SIZE_FIELD
            } else {
                len
            };
            let kind = EntryKind::Resource { offset: 0, size, system_flags, graphics_flags };
            return Ok(FileNode { kind, data: Data::Bytes(data) });
        }

        let uncompressed_size = u32::try_from(data.len()).map_err(|_| Error::TooLarge(name.to_string()))?;
        let ext = name.rsplit('.').next().unwrap_or("");
        if !STORED_RAW.contains(&ext) {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(&data)?;
            let compressed = enc.finish()?;
            if (compressed.len() as u64) < MAX_SIZE_FIELD as u64 && compressed.len() < data.len() {
                let kind = EntryKind::Binary { offset: 0, size: compressed.len() as u32, uncompressed_size, encrypted: false };
                return Ok(FileNode { kind, data: Data::Bytes(compressed) });
            }
        }
        let kind = EntryKind::Binary { offset: 0, size: 0, uncompressed_size, encrypted: false };
        Ok(FileNode { kind, data: Data::Bytes(data) })
    }
}

/// First-fit block allocator over the gaps between used ranges.
struct Allocator {
    /// Sorted, merged (start, end) block ranges.
    used: Vec<(u64, u64)>,
}

impl Allocator {
    fn new(mut used: Vec<(u64, u64)>) -> Self {
        used.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(used.len());
        for (s, e) in used {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        Allocator { used: merged }
    }

    fn alloc(&mut self, blocks: u64) -> u64 {
        let blocks = blocks.max(1);
        let pos = self.used.windows(2).position(|w| w[1].0 - w[0].1 >= blocks);
        match pos {
            Some(i) => {
                let start = self.used[i].1;
                self.used[i].1 += blocks;
                start
            }
            None => {
                let last = self.used.last_mut().unwrap();
                let start = last.1;
                last.1 += blocks;
                start
            }
        }
    }

    fn end(&self) -> u64 {
        self.used.last().map_or(0, |r| r.1)
    }
}

fn copy_data(data: &Data, target: &File, at: u64) -> Result<(), Error> {
    match data {
        Data::Bytes(b) => target.write_all_at(b, at)?,
        Data::Stored { file, offset, len } => {
            let mut buf = vec![0u8; 1 << 20];
            let mut done = 0;
            while done < *len {
                let n = (*len - done).min(buf.len() as u64) as usize;
                file.read_exact_at(&mut buf[..n], offset + done)?;
                target.write_all_at(&buf[..n], at + done)?;
                done += n as u64;
            }
        }
    }
    Ok(())
}

fn set_offset(kind: &mut EntryKind, block: u64, name: &str) -> Result<(), Error> {
    let too_far = || Error::TooLarge(name.to_string());
    match kind {
        EntryKind::Binary { offset, .. } if block <= 0xFF_FFFF => *offset = block as u32,
        EntryKind::Resource { offset, .. } if block <= 0x7F_FFFF => *offset = block as u32,
        _ => return Err(too_far()),
    }
    Ok(())
}

/// Builds the names table (deduplicated, padded to 16 bytes) and returns it
/// with each entry's name offset.
fn names_table(entries: &[Entry]) -> Result<(Vec<u8>, Vec<u32>), Error> {
    let mut table = Vec::new();
    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for e in entries {
        let off = *seen.entry(e.name.as_str()).or_insert_with(|| {
            let off = table.len() as u32;
            table.extend_from_slice(e.name.as_bytes());
            table.push(0);
            off
        });
        offsets.push(off);
    }
    // File entries only have 16 bits for the name offset.
    if table.len() > 0xFFFF {
        return Err(Error::Unsupported("names table larger than 64 KiB"));
    }
    table.resize(table.len().next_multiple_of(16), 0);
    Ok((table, offsets))
}

fn encode_entry(e: &Entry, name_offset: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    let words: [u32; 4] = match e.kind {
        EntryKind::Directory { first, count } => [name_offset, DIR_IDENT, first, count],
        EntryKind::Binary { offset, size, uncompressed_size, encrypted } => {
            let lo = name_offset as u64 | (size as u64) << 16 | (offset as u64) << 40;
            [lo as u32, (lo >> 32) as u32, uncompressed_size, encrypted as u32]
        }
        EntryKind::Resource { offset, size, system_flags, graphics_flags } => {
            let lo = name_offset as u64 | (size.min(MAX_SIZE_FIELD) as u64) << 16 | ((offset as u64) | 0x80_0000) << 40;
            [lo as u32, (lo >> 32) as u32, system_flags, graphics_flags]
        }
    };
    for (c, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *c = w.to_le_bytes();
    }
    out
}

fn is_script(name: &str) -> bool {
    name.ends_with(".ysc")
}

fn split(path: &str) -> Result<Vec<String>, Error> {
    let parts: Vec<String> = path.split(['/', '\\']).filter(|p| !p.is_empty()).map(str::to_ascii_lowercase).collect();
    if parts.is_empty() || parts.iter().any(|p| p == "." || p == "..") {
        return Err(Error::InvalidPath(path.to_string()));
    }
    Ok(parts)
}

fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}
