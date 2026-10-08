//! PSO files: big-endian sections (CodeWalker's `Pso.cs`, MIT). PSIN holds
//! the data blocks, PMAP their types and offsets, PSCH the schema; STRF,
//! STRS, STRE and PSIG are kept as they are and CHKS is recomputed.

use super::{Blocks, Field, Kind, Schema, StructInfo};
use crate::Error;
use crate::names::Names;

const ARRAYINFO: u32 = 0x100;

pub(super) struct Header {
    /// Section idents in file order, with the bytes of those kept unchanged.
    sections: Vec<([u8; 4], Option<Vec<u8>>)>,
    pmap_flags: u16,
    chks_tail: u32,
    /// What fills the gaps between blocks: 'p' in files made by CodeWalker, 0 in Rockstar's.
    fill: u8,
    /// The block types in the order the file holds them.
    pub(super) block_types: Vec<u32>,
}

fn be32(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4).map(|v| u32::from_be_bytes(v.try_into().unwrap())).ok_or_else(|| Error::Format("PSO: truncated section".into()))
}

fn be16(b: &[u8], at: usize) -> Result<u16, Error> {
    b.get(at..at + 2).map(|v| u16::from_be_bytes(v.try_into().unwrap())).ok_or_else(|| Error::Format("PSO: truncated section".into()))
}

pub(super) fn parse(data: &[u8], names: &mut Names) -> Result<(Header, Schema, Blocks, u32), Error> {
    let mut sections = Vec::new();
    let (mut psin, mut pmap, mut psch) = (None, None, None);
    let mut chks_tail = 0x7970_7070; // "yppp"
    let mut at = 0;
    while at + 8 <= data.len() {
        let ident: [u8; 4] = data[at..at + 4].try_into().unwrap();
        let len = be32(data, at + 4)? as usize;
        let section = data.get(at..at + len).filter(|_| len >= 8).ok_or_else(|| Error::Format("PSO: truncated section".into()))?;
        match &ident {
            b"PSIN" => psin = Some(section),
            b"PMAP" => pmap = Some(section),
            b"CHKS" => chks_tail = be32(section, 16).unwrap_or(chks_tail),
            b"PSCH" => psch = Some(section),
            b"STRF" | b"STRS" => {
                for s in section[8..].split(|&b| b == 0).filter(|s| !s.is_empty()) {
                    names.add(&String::from_utf8_lossy(s));
                }
            }
            _ => {}
        }
        let keep = !matches!(&ident, b"PSIN" | b"PMAP" | b"CHKS");
        sections.push((ident, keep.then(|| section.to_vec())));
        at += len;
    }
    let missing = |s: &str| Error::Format(format!("PSO: no {s} section"));
    let (psin, pmap, psch) = (psin.ok_or(missing("PSIN"))?, pmap.ok_or(missing("PMAP"))?, psch.ok_or(missing("PSCH"))?);

    let root_id = be32(pmap, 8)?;
    let count = be16(pmap, 12)? as i16;
    if count <= 0 {
        return Err(Error::Unsupported("this PSO block map layout".into()));
    }
    let pmap_flags = be16(pmap, 14)?;
    let mut blocks = Vec::new();
    for i in 0..count as usize {
        let e = 16 + i * 16;
        let (ty, offset, len) = (be32(pmap, e)?, be32(pmap, e + 4)? as usize, be32(pmap, e + 12)? as usize);
        let bytes = psin.get(offset..offset + len).ok_or_else(|| Error::Format("PSO: block out of range".into()))?;
        blocks.push((ty, bytes.to_vec()));
    }

    let mut schema = Schema::default();
    for i in 0..be32(psch, 8)? as usize {
        let (name, offset) = (be32(psch, 12 + i * 8)?, be32(psch, 16 + i * 8)? as usize);
        let head = be32(psch, offset)?;
        match head >> 24 {
            0 => {
                let count = (head & 0xFFFF) as usize;
                let size = be32(psch, offset + 4)? as usize;
                let raw = (0..count)
                    .map(|j| {
                        let e = offset + 12 + j * 12;
                        Ok(Entry {
                            name: be32(psch, e)?,
                            ty: *psch.get(e + 4).ok_or_else(|| Error::Format("PSO: truncated schema".into()))?,
                            sub: psch[e + 5],
                            offset: be16(psch, e + 6)? as usize,
                            reference: be32(psch, e + 8)?,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                let mut fields = Vec::new();
                for e in raw.iter().filter(|e| e.name != ARRAYINFO) {
                    fields.push(Field { name: e.name, offset: e.offset, kind: kind(e, &raw)? });
                }
                schema.structs.insert(name, StructInfo { size, fields });
            }
            1 => {
                let count = (head & 0xFF_FFFF) as usize;
                let values = (0..count)
                    .map(|j| Ok((be32(psch, offset + 4 + j * 8)?, be32(psch, offset + 8 + j * 8)? as i32 as i64)))
                    .collect::<Result<_, Error>>()?;
                schema.enums.insert(name, values);
            }
            other => return Err(Error::Format(format!("PSO: unknown schema element {other}"))),
        }
    }
    let fill = if psin.get(8) == Some(&b'p') { b'p' } else { 0 };
    let block_types = blocks.iter().map(|b| b.0).collect();
    Ok((Header { sections, pmap_flags, chks_tail, fill, block_types }, schema, Blocks { list: blocks, big_endian: true }, root_id))
}

#[derive(Clone, Copy)]
struct Entry {
    name: u32,
    ty: u8,
    sub: u8,
    offset: usize,
    reference: u32,
}

fn kind(e: &Entry, all: &[Entry]) -> Result<Kind, Error> {
    let unsupported = || Error::Unsupported(format!("PSO field type {:#x}/{}", e.ty, e.sub));
    let info = |index: usize| all.get(index).filter(|i| i.name == ARRAYINFO).copied();
    Ok(match e.ty {
        0x00 => Kind::Bool,
        0x01 => Kind::I8,
        0x02 => Kind::U8,
        0x03 => Kind::I16,
        0x04 => Kind::U16,
        0x05 => Kind::I32,
        0x06 if e.sub == 1 => Kind::HexU32,
        0x06 => Kind::I32,
        0x07 => Kind::F32,
        0x08 => Kind::Vec2,
        0x09 | 0x14 => Kind::Vec3,
        0x0A | 0x15 => Kind::Vec4,
        0x0B => match e.sub {
            0 => Kind::Chars(((e.reference >> 16) & 0xFFFF) as usize),
            1 | 2 => Kind::StrPtr,
            3 => Kind::CharPtr,
            7 | 8 => Kind::Hash,
            _ => return Err(unsupported()),
        },
        0x0C => match e.sub {
            0 => Kind::Struct(e.reference),
            3 | 4 => Kind::StructPtr,
            _ => return Err(unsupported()),
        },
        0x0D => {
            let index = (e.reference & 0xFFFF) as usize;
            let elem = info(index).or_else(|| info((e.reference & 0xFFF) as usize)).ok_or_else(unsupported)?;
            let mut elem = kind(&elem, all)?;
            if elem == Kind::Vec3 {
                elem = Kind::Vec3Padded;
            }
            if let Kind::Enum { size, .. } = &mut elem {
                *size = 4;
            }
            let count = ((e.reference >> 16) & 0xFFFF) as usize;
            // atArray (16-bit counts), its 32-bit count variant, or inline.
            let (inline, wide) = match e.sub {
                0 => (None, false),
                6 => (None, true),
                4 if elem == Kind::StructPtr => (None, false),
                1 | 2 | 4 | 129 => (Some(count), false),
                _ => return Err(unsupported()),
            };
            Kind::Array { elem: Box::new(elem), inline, wide }
        }
        0x0E => Kind::Enum {
            size: match e.sub {
                0 => 4,
                1 => 2,
                2 => 1,
                _ => return Err(unsupported()),
            },
            enumeration: e.reference,
        },
        0x0F => {
            let size = match e.sub {
                0 => 4,
                1 => 2,
                2 => 1,
                _ => return Err(unsupported()),
            };
            let index = (e.reference & 0xFFF) as usize;
            let enumeration = if index == 0xFFF { None } else { info(index).map(|i| i.reference) };
            Kind::Flags { size, enumeration, sep: " " }
        }
        0x10 if e.sub == 1 => Kind::Map,
        0x1E => Kind::Half,
        0x20 => Kind::U64,
        _ => return Err(unsupported()),
    })
}

/// Block types CodeWalker's `PsoBuilder` uses.
pub(super) fn default_block(content: &str) -> u32 {
    match content {
        "U8" | "Bool" => 2,
        "U16" | "I16" => 4,
        "I32" => 5,
        "U32" | "HexU32" | "Hash" => 6,
        "F32" => 7,
        "pointer" => 12,
        _ => 1,
    }
}

/// The checksum in CHKS: a Jenkins hash of the whole file (with the checksum
/// and size fields zeroed), adding bytes as signed values.
fn checksum(data: &[u8]) -> u32 {
    let mut h: u32 = 0x3FAC_7125;
    for &b in data {
        h = h.wrapping_add(b as i8 as i32 as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

pub(super) fn write(h: &Header, blocks: &[(u32, Vec<u8>)], root_id: u32) -> Result<Vec<u8>, Error> {
    // PSIN: header, then 16-byte aligned blocks, gaps filled as in the original.
    let mut psin = b"PSIN\0\0\0\0".to_vec();
    let mut offsets = Vec::new();
    for (_, data) in blocks {
        psin.resize(psin.len().next_multiple_of(16), h.fill);
        offsets.push(psin.len());
        psin.extend_from_slice(data);
    }
    let len = psin.len() as u32;
    psin[4..8].copy_from_slice(&len.to_be_bytes());

    let mut pmap = b"PMAP".to_vec();
    pmap.extend_from_slice(&(16 + 16 * blocks.len() as u32).to_be_bytes());
    pmap.extend_from_slice(&root_id.to_be_bytes());
    pmap.extend_from_slice(&(blocks.len() as u16).to_be_bytes());
    pmap.extend_from_slice(&h.pmap_flags.to_be_bytes());
    for ((ty, data), offset) in blocks.iter().zip(&offsets) {
        for word in [*ty, *offset as u32, 0, data.len() as u32] {
            pmap.extend_from_slice(&word.to_be_bytes());
        }
    }

    let mut out = Vec::new();
    let mut chks_at = None;
    for (ident, kept) in &h.sections {
        match (ident, kept) {
            (b"PSIN", _) => out.extend_from_slice(&psin),
            (b"PMAP", _) => out.extend_from_slice(&pmap),
            (b"CHKS", _) => {
                chks_at = Some(out.len());
                out.extend_from_slice(b"CHKS");
                for word in [20, 0, 0, h.chks_tail] {
                    out.extend_from_slice(&u32::to_be_bytes(word));
                }
            }
            (_, Some(bytes)) => out.extend_from_slice(bytes),
            _ => {}
        }
    }
    if let Some(at) = chks_at {
        let sum = checksum(&out);
        let size = out.len() as u32;
        out[at + 8..at + 12].copy_from_slice(&size.to_be_bytes());
        out[at + 12..at + 16].copy_from_slice(&sum.to_be_bytes());
    }
    Ok(out)
}

pub(super) fn verify_checksum(data: &[u8]) -> Option<bool> {
    let at = data.windows(4).rposition(|w| w == b"CHKS")?;
    let mut copy = data.to_vec();
    let (size, sum) = (be32(data, at + 8).ok()?, be32(data, at + 12).ok()?);
    copy[at + 8..at + 16].fill(0);
    Some(size as usize == data.len() && checksum(&copy) == sum)
}
