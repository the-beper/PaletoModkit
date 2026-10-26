//! RPF7 archive reader.
//!
//! Layout: 16-byte header, `entry_count` 16-byte entries, then the names table.
//! Entries and names may be encrypted; file data lives at 512-byte block offsets
//! relative to the start of the archive. Nested archives are stored inline, so a
//! child [`Archive`] is just a window into the same physical file.

use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;

use flate2::read::DeflateDecoder;

use crate::crypto::{decrypt_aes, decrypt_ng, ng_key};
use crate::{Error, Keys};

pub(crate) const MAGIC: u32 = 0x5250_4637; // "RPF7"
pub(crate) const RSC7_MAGIC: u32 = 0x3743_5352; // "RSC7"
pub(crate) const BLOCK: u64 = 512;
pub(crate) const DIR_IDENT: u32 = 0x7FFF_FF00;
pub(crate) const OPEN: u32 = 0x4E45_504F;
pub(crate) const AES: u32 = 0x0FFF_FFF9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    None,
    /// OpenIV-style archive with a plain table of contents.
    Open,
    Aes,
    Ng,
}

impl Encryption {
    pub(crate) fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::None,
            OPEN => Self::Open,
            AES => Self::Aes,
            // 0x0FEFFFFF; anything else is treated as NG too, like the game does.
            _ => Self::Ng,
        }
    }
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    Directory { first: u32, count: u32 },
    Binary { offset: u32, size: u32, uncompressed_size: u32, encrypted: bool },
    Resource { offset: u32, size: u32, system_flags: u32, graphics_flags: u32 },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory { .. })
    }

    pub fn is_archive(&self) -> bool {
        matches!(self.kind, EntryKind::Binary { .. }) && self.name.to_ascii_lowercase().ends_with(".rpf")
    }

    /// Size of the file once extracted.
    pub fn size(&self) -> u64 {
        match self.kind {
            EntryKind::Directory { .. } => 0,
            EntryKind::Binary { uncompressed_size, .. } => uncompressed_size as u64,
            EntryKind::Resource { size, .. } => size as u64,
        }
    }

    /// Block offset of the file data, relative to the archive start.
    pub fn offset(&self) -> Option<u32> {
        match self.kind {
            EntryKind::Directory { .. } => None,
            EntryKind::Binary { offset, .. } | EntryKind::Resource { offset, .. } => Some(offset),
        }
    }

    /// Number of bytes the file occupies inside the archive.
    pub fn stored_len(&self) -> u64 {
        match self.kind {
            EntryKind::Directory { .. } => 0,
            EntryKind::Binary { size: 0, uncompressed_size, .. } => uncompressed_size as u64,
            EntryKind::Binary { size, .. } | EntryKind::Resource { size, .. } => size as u64,
        }
    }

    /// Resource version, from the top nibbles of the page flags.
    pub fn resource_version(&self) -> Option<u32> {
        match self.kind {
            EntryKind::Resource { system_flags, graphics_flags, .. } => {
                Some(((system_flags >> 28) << 4) | (graphics_flags >> 28))
            }
            _ => None,
        }
    }
}

pub struct Archive {
    pub(crate) file: Arc<File>,
    pub name: String,
    pub(crate) base: u64,
    pub size: u64,
    pub encryption: Encryption,
    pub entries: Vec<Entry>,
    /// Bytes taken by the header, entries and names.
    pub(crate) toc_len: u64,
}

impl Archive {
    /// Opens an archive file on disk. `keys` is only needed for encrypted archives.
    pub fn open(path: &Path, keys: Option<&Keys>) -> Result<Self, Error> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self::parse(Arc::new(file), name, 0, size, keys)
    }

    pub(crate) fn parse(file: Arc<File>, name: String, base: u64, size: u64, keys: Option<&Keys>) -> Result<Self, Error> {
        let mut header = [0u8; 16];
        file.read_exact_at(&mut header, base)?;
        let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
        if word(0) != MAGIC {
            return Err(Error::NotAnArchive(name));
        }
        let (count, names_len) = (word(1) as usize, word(2) as usize);
        let encryption = Encryption::from_u32(word(3));

        let mut toc = vec![0u8; count * 16];
        let mut names = vec![0u8; names_len];
        file.read_exact_at(&mut toc, base + 16)?;
        file.read_exact_at(&mut names, base + 16 + toc.len() as u64)?;

        match encryption {
            Encryption::None | Encryption::Open => {}
            Encryption::Aes => {
                let keys = keys.ok_or(Error::KeysRequired)?;
                decrypt_aes(&keys.aes, &mut toc);
                decrypt_aes(&keys.aes, &mut names);
            }
            Encryption::Ng => {
                let keys = keys.ok_or(Error::KeysRequired)?;
                let key = ng_key(keys, &name, size as u32);
                decrypt_ng(&keys.ng_tables, key, &mut toc);
                decrypt_ng(&keys.ng_tables, key, &mut names);
            }
        }

        let mut entries = Vec::with_capacity(count);
        for raw in toc.as_chunks::<16>().0 {
            let lo = u64::from_le_bytes(raw[0..8].try_into().unwrap());
            let w = |i: usize| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
            let (name_offset, kind) = if w(1) == DIR_IDENT {
                (w(0) as usize, EntryKind::Directory { first: w(2), count: w(3) })
            } else {
                let size = ((lo >> 16) & 0xFF_FFFF) as u32;
                if w(1) & 0x8000_0000 == 0 {
                    let kind = EntryKind::Binary {
                        offset: ((lo >> 40) & 0xFF_FFFF) as u32,
                        size,
                        uncompressed_size: w(2),
                        encrypted: w(3) == 1,
                    };
                    ((lo & 0xFFFF) as usize, kind)
                } else {
                    let offset = ((lo >> 40) & 0x7F_FFFF) as u32;
                    let size = if size == 0xFF_FFFF {
                        // Oversized resource: the real size is scattered over its RSC7 header.
                        let mut h = [0u8; 16];
                        file.read_exact_at(&mut h, base + offset as u64 * BLOCK)?;
                        u32::from_le_bytes([h[7], h[14], h[5], h[2]])
                    } else {
                        size
                    };
                    let kind = EntryKind::Resource { offset, size, system_flags: w(2), graphics_flags: w(3) };
                    ((lo & 0xFFFF) as usize, kind)
                }
            };
            let name = names.get(name_offset..).ok_or(Error::Corrupt("name offset"))?;
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            let name = String::from_utf8_lossy(&name[..end]).into_owned();
            // A name that is a path would let an extract write outside its folder.
            if name.contains(['/', '\\']) || name == "." || name == ".." {
                return Err(Error::Corrupt("entry name is a path"));
            }
            entries.push(Entry { name, kind });
        }
        if !entries.first().is_some_and(Entry::is_dir) {
            return Err(Error::Corrupt("root is not a directory"));
        }

        let toc_len = 16 + (count * 16 + names_len) as u64;
        Ok(Archive { file, name, base, size, encryption, entries, toc_len })
    }

    /// Indices of the direct children of directory `dir` (0 is the root).
    pub fn children(&self, dir: usize) -> Range<usize> {
        match self.entries[dir].kind {
            EntryKind::Directory { first, count } => {
                let end = (first as usize + count as usize).min(self.entries.len());
                (first as usize).min(end)..end
            }
            _ => 0..0,
        }
    }

    /// Looks up an entry by path relative to the archive root, case-insensitively.
    pub fn find(&self, path: &str) -> Option<usize> {
        let mut cur = 0;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            cur = self.children(cur).find(|&i| self.entries[i].name.eq_ignore_ascii_case(part))?;
        }
        Some(cur)
    }

    /// Every entry below `dir` as (path relative to `dir`, index), depth first.
    pub fn walk(&self, dir: usize) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), dir)];
        while let Some((prefix, d)) = stack.pop() {
            for i in self.children(d).rev() {
                let path = format!("{prefix}{}", self.entries[i].name);
                if self.entries[i].is_dir() {
                    stack.push((format!("{path}/"), i));
                }
                out.push((path, i));
            }
        }
        out.reverse();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Reads a file the way it would look as a standalone file on disk:
    /// binary files are decrypted and inflated, resources are returned as a
    /// complete RSC7 file (header + compressed payload).
    pub fn read(&self, idx: usize, keys: Option<&Keys>) -> Result<Vec<u8>, Error> {
        let entry = &self.entries[idx];
        let offset = entry.offset().ok_or_else(|| Error::NotAFile(entry.name.clone()))?;
        let raw = self.read_raw(offset, 0, entry.stored_len() as usize)?;
        decode(entry, raw, self.encryption == Encryption::Aes, keys)
    }

    /// A file's bytes as stored in the archive (still compressed and encrypted).
    pub fn read_stored(&self, idx: usize) -> Result<Vec<u8>, Error> {
        let entry = &self.entries[idx];
        let offset = entry.offset().ok_or_else(|| Error::NotAFile(entry.name.clone()))?;
        self.read_raw(offset, 0, entry.stored_len() as usize)
    }

    /// Opens a nested archive entry.
    pub fn open_child(&self, idx: usize, keys: Option<&Keys>) -> Result<Archive, Error> {
        let entry = &self.entries[idx];
        match entry.kind {
            EntryKind::Binary { offset, size: 0, uncompressed_size, encrypted: false } if entry.is_archive() => {
                let base = self.base + offset as u64 * BLOCK;
                Self::parse(self.file.clone(), entry.name.clone(), base, uncompressed_size as u64, keys)
            }
            _ => Err(Error::NotAnArchive(entry.name.clone())),
        }
    }

    fn read_raw(&self, block: u32, skip: u64, len: usize) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; len];
        self.file.read_exact_at(&mut buf, self.base + block as u64 * BLOCK + skip)?;
        Ok(buf)
    }
}

/// Turns the stored bytes of a file entry into the standalone file.
pub(crate) fn decode(entry: &Entry, mut raw: Vec<u8>, aes: bool, keys: Option<&Keys>) -> Result<Vec<u8>, Error> {
    let decrypt = |data: &mut [u8], length: u32| -> Result<(), Error> {
        let keys = keys.ok_or(Error::KeysRequired)?;
        if aes {
            decrypt_aes(&keys.aes, data);
        } else {
            // OPEN archives edited by OpenIV can still hold NG-encrypted files.
            decrypt_ng(&keys.ng_tables, ng_key(keys, &entry.name, length), data);
        }
        Ok(())
    };
    match entry.kind {
        EntryKind::Directory { .. } => Err(Error::NotAFile(entry.name.clone())),
        EntryKind::Binary { size, uncompressed_size, encrypted, .. } => {
            if encrypted {
                decrypt(&mut raw, uncompressed_size)?;
            }
            if size == 0 {
                return Ok(raw);
            }
            let mut out = Vec::with_capacity(uncompressed_size as usize);
            DeflateDecoder::new(&raw[..]).read_to_end(&mut out)?;
            Ok(out)
        }
        EntryKind::Resource { size, system_flags, graphics_flags, .. } => {
            if raw.len() < 16 {
                return Err(Error::Corrupt("resource too small"));
            }
            if entry.name.to_ascii_lowercase().ends_with(".ysc") {
                decrypt(&mut raw[16..], size)?;
            }
            // Rebuild the header: oversized resources store their size in the on-disk one.
            let version = entry.resource_version().unwrap();
            for (i, w) in [RSC7_MAGIC, version, system_flags, graphics_flags].into_iter().enumerate() {
                raw[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            Ok(raw)
        }
    }
}
