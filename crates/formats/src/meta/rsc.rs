//! Resource metadata: an RSC7 resource (version 2) holding a `Meta` root
//! with structure infos, enum infos and data blocks (CodeWalker's `Meta.cs`).

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

use super::{Blocks, Field, Kind, Schema, StructInfo};
use crate::Error;
use crate::pages::{self, MAX_PAGES};
use crate::rsc::{Resource, u16_at, u32_at, u64_at};

const SYS: u64 = 0x5000_0000;
const ROOT_LEN: usize = 112;
const PAGES_INFO_LEN: usize = 16 + 8 * 128;
const ARRAYINFO: u32 = 0x100;

/// What is copied unchanged when writing: the root structure and the schema.
pub(super) struct Header {
    root: Vec<u8>,
    /// Structure infos (32 bytes each) and their entries.
    structs: Vec<(Vec<u8>, Vec<u8>)>,
    /// Enum infos (24 bytes each) and their entries.
    enums: Vec<(Vec<u8>, Vec<u8>)>,
    name: Option<String>,
}

pub(super) fn parse(data: &[u8]) -> Result<(Header, Schema, Blocks, u32), Error> {
    let res = Resource::parse(data)?;
    let root = res.block(SYS, ROOT_LEN)?.to_vec();
    let list = |ptr: u64, count: usize, len: usize| -> Result<Vec<u8>, Error> {
        if count == 0 { Ok(Vec::new()) } else { Ok(res.block(ptr, count * len)?.to_vec()) }
    };

    let mut schema = Schema::default();
    let mut structs = Vec::new();
    let infos = list(u64_at(&root, 0x20), u16_at(&root, 0x48) as usize, 32)?;
    for info in infos.as_chunks::<32>().0 {
        let entries = list(u64_at(info, 0x10), u16_at(info, 0x1E) as usize, 16)?;
        let name = u32_at(info, 0);
        let fields = struct_fields(&entries)?;
        schema.structs.insert(name, StructInfo { size: u32_at(info, 0x18) as usize, fields });
        structs.push((info.to_vec(), entries));
    }
    let mut enums = Vec::new();
    let infos = list(u64_at(&root, 0x28), u16_at(&root, 0x4A) as usize, 24)?;
    for info in infos.as_chunks::<24>().0 {
        let entries = list(u64_at(info, 8), u32_at(info, 0x10) as usize, 8)?;
        let values = entries.as_chunks::<8>().0.iter().map(|e| (u32_at(e, 0), u32_at(e, 4) as i32 as i64)).collect();
        schema.enums.insert(u32_at(info, 0), values);
        enums.push((info.to_vec(), entries));
    }
    let mut blocks = Vec::new();
    let infos = list(u64_at(&root, 0x30), u16_at(&root, 0x4C) as usize, 16)?;
    for info in infos.as_chunks::<16>().0 {
        let len = u32_at(info, 4) as usize;
        let data = if len == 0 { Vec::new() } else { res.block(u64_at(info, 8), len)?.to_vec() };
        blocks.push((u32_at(info, 0), data));
    }
    let name_ptr = u64_at(&root, 0x38);
    let name = if name_ptr == 0 { None } else { Some(res.string(name_ptr)?) };
    let root_id = u32_at(&root, 0x1C);
    Ok((Header { root, structs, enums, name }, schema, Blocks { list: blocks, big_endian: false }, root_id))
}

/// Entries: name, offset, type, (unused), array info index, reference.
fn struct_fields(entries: &[u8]) -> Result<Vec<Field>, Error> {
    let raw: Vec<(u32, usize, u8, i16, u32)> = entries
        .as_chunks::<16>().0.iter()
        .map(|e| (u32_at(e, 0), u32_at(e, 4) as usize, e[8], u16_at(e, 10) as i16, u32_at(e, 12)))
        .collect();
    let mut fields = Vec::new();
    for (i, &(name, offset, ty, array_index, reference)) in raw.iter().enumerate() {
        if name == ARRAYINFO {
            continue;
        }
        // The element type of an array: its own index, or the info just before it.
        let array_info = || {
            let at = usize::try_from(array_index).ok().filter(|&j| raw.get(j).is_some_and(|e| e.0 == ARRAYINFO));
            at.or_else(|| raw[..i].iter().rposition(|e| e.0 == ARRAYINFO)).map(|j| raw[j])
        };
        let kind = match ty {
            0x52 => {
                let info = array_info().ok_or_else(|| Error::Format("array without element type".into()))?;
                Kind::Array { elem: Box::new(element(info.2, info.4)?), inline: None, wide: false }
            }
            0x50 => {
                let elem = array_info().map_or(Ok(Kind::U8), |info| element(info.2, info.4))?;
                Kind::Array { elem: Box::new(elem), inline: Some(reference as usize), wide: false }
            }
            ty => scalar(ty, reference)?,
        };
        fields.push(Field { name, offset, kind });
    }
    Ok(fields)
}

fn scalar(ty: u8, reference: u32) -> Result<Kind, Error> {
    Ok(match ty {
        0x01 => Kind::Bool,
        0x10 => Kind::I8,
        // Byte enums are shown as numbers, as CodeWalker does.
        0x11 | 0x60 => Kind::U8,
        0x12 => Kind::I16,
        0x13 => Kind::U16,
        0x14 => Kind::I32,
        0x15 => Kind::U32,
        0x21 => Kind::F32,
        0x33 => Kind::Vec3,
        0x34 => Kind::Vec4,
        0x4A => Kind::Hash,
        0x62 => Kind::Enum { size: 4, enumeration: reference },
        0x64 => Kind::Enum { size: 2, enumeration: reference },
        0x63 | 0x65 => Kind::Flags { size: 4, enumeration: Some(reference), sep: ", " },
        0x40 => Kind::Chars(reference as usize),
        0x44 => Kind::CharPtr,
        0x59 => Kind::Bytes,
        0x05 => Kind::Struct(reference),
        0x07 => Kind::StructPtr,
        other => return Err(Error::Unsupported(format!("metadata field type {other:#x}"))),
    })
}

fn element(ty: u8, reference: u32) -> Result<Kind, Error> {
    Ok(match ty {
        // Vectors in arrays take 16 bytes each.
        0x33 => Kind::Vec3Padded,
        0x05 | 0x07 | 0x10..=0x15 | 0x21 | 0x34 | 0x4A => scalar(ty, reference)?,
        other => return Err(Error::Unsupported(format!("metadata array of type {other:#x}"))),
    })
}

/// CodeWalker's `MetaTypeName` block types.
pub(super) fn default_block(content: &str) -> u32 {
    match content {
        "U32" | "I32" => 0x15,
        "Hash" => 0x4A,
        "U16" | "I16" => 0x13,
        "U8" | "I8" => 0x11,
        "F32" => 0x21,
        "Vec3Padded" | "Vec4" => 0x33,
        "pointer" => 0x07,
        "chars" | "string" => 0x10,
        _ => 0x11,
    }
}

pub(super) fn write(h: &Header, blocks: &[(u32, Vec<u8>)], root_id: u32) -> Result<Vec<u8>, Error> {
    // System blocks: root, pages info, structure infos, their entries, enum
    // infos, their entries, data block infos, each block's data, the name.
    let mut sizes = vec![ROOT_LEN, PAGES_INFO_LEN];
    let mut add = |len: usize| {
        sizes.push(len.max(1));
        sizes.len() - 1
    };
    let struct_infos = add(32 * h.structs.len());
    let struct_entries: Vec<usize> = h.structs.iter().map(|(_, e)| add(e.len())).collect();
    let enum_infos = add(24 * h.enums.len());
    let enum_entries: Vec<usize> = h.enums.iter().map(|(_, e)| add(e.len())).collect();
    let block_infos = add(16 * blocks.len());
    let block_data: Vec<usize> = blocks.iter().map(|(_, d)| add(d.len())).collect();
    let name = h.name.as_ref().map(|n| add(n.len() + 1));
    let sys = pages::layout(&sizes, true, MAX_PAGES)?;

    let at = |i: usize| SYS + sys.offsets[i] as u64;
    let mut system = vec![0u8; sys.size];
    let mut put = |i: usize, bytes: &[u8]| {
        let o = sys.offsets[i];
        system[o..o + bytes.len()].copy_from_slice(bytes);
    };
    let ptr_if = |present: bool, i: usize| if present { at(i) } else { 0 };

    let mut infos = Vec::new();
    for ((info, entries), &e) in h.structs.iter().zip(&struct_entries) {
        let mut info = info.clone();
        info[0x10..0x18].copy_from_slice(&ptr_if(!entries.is_empty(), e).to_le_bytes());
        infos.extend_from_slice(&info);
        put(e, entries);
    }
    put(struct_infos, &infos);
    let mut infos = Vec::new();
    for ((info, entries), &e) in h.enums.iter().zip(&enum_entries) {
        let mut info = info.clone();
        info[8..0x10].copy_from_slice(&ptr_if(!entries.is_empty(), e).to_le_bytes());
        infos.extend_from_slice(&info);
        put(e, entries);
    }
    put(enum_infos, &infos);
    let mut infos = Vec::new();
    for ((ty, data), &d) in blocks.iter().zip(&block_data) {
        infos.extend_from_slice(&ty.to_le_bytes());
        infos.extend_from_slice(&(data.len() as u32).to_le_bytes());
        infos.extend_from_slice(&ptr_if(!data.is_empty(), d).to_le_bytes());
        put(d, data);
    }
    put(block_infos, &infos);
    if let (Some(i), Some(n)) = (name, &h.name) {
        put(i, &[n.as_bytes(), &[0]].concat());
    }

    let mut root = h.root.clone();
    root[0x08..0x10].copy_from_slice(&at(1).to_le_bytes());
    root[0x16] = 0; // encrypted strings are not kept
    root[0x1C..0x20].copy_from_slice(&root_id.to_le_bytes());
    root[0x20..0x28].copy_from_slice(&ptr_if(!h.structs.is_empty(), struct_infos).to_le_bytes());
    root[0x28..0x30].copy_from_slice(&ptr_if(!h.enums.is_empty(), enum_infos).to_le_bytes());
    root[0x30..0x38].copy_from_slice(&ptr_if(!blocks.is_empty(), block_infos).to_le_bytes());
    root[0x38..0x40].copy_from_slice(&name.map_or(0, at).to_le_bytes());
    root[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
    root[0x48..0x4A].copy_from_slice(&(h.structs.len() as u16).to_le_bytes());
    root[0x4A..0x4C].copy_from_slice(&(h.enums.len() as u16).to_le_bytes());
    root[0x4C..0x4E].copy_from_slice(&(blocks.len() as u16).to_le_bytes());
    put(0, &root);
    let mut info = [0u8; 16];
    info[8] = sys.pages as u8;
    put(1, &info);

    let version = 2u32;
    let mut out = Vec::new();
    for word in [0x3743_5352, version, sys.flags | ((version >> 4) << 28), (version & 0xF) << 28] {
        out.extend_from_slice(&u32::to_le_bytes(word));
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&system)?;
    Ok(z.finish()?)
}
