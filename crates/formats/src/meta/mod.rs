//! Metadata files, shown and edited as XML: resource metadata (RSC7 version
//! 2: `.ymap`, `.ytyp` and many `.ymt`), PSO files (other `.ymt`, `.pso`)
//! and RBF files (a few `.ymt`; see [`rbf`], which needs no schema).
//!
//! Both kinds carry their own schema (structures and enums, named by hash)
//! and keep their data in typed blocks, addressed by (block id, offset)
//! pointers. A file is read into a tree of values guided by its schema and
//! written as XML in CodeWalker's style (MIT; `MetaXml.cs`, `Meta.cs`,
//! `Pso.cs`). XML is turned back into a file with the schema of the file it
//! came from: every block is laid out again, reusing the original block
//! types.

mod pso;
mod rbf;
mod rsc;
mod xml;

use std::collections::HashMap;

use crate::Error;
use crate::names::Names;

/// A field, array element or map part: how its bytes are laid out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    /// Shown in hexadecimal.
    HexU32,
    U64,
    F32,
    /// A half float, kept as its raw bits.
    Half,
    Vec2,
    Vec3,
    /// A 3-component vector stored in 16 bytes.
    Vec3Padded,
    Vec4,
    Hash,
    /// An enum value of `size` bytes, shown by name.
    Enum { size: u8, enumeration: u32 },
    /// Bits of an enum's values, shown by name (`sep`-separated) when known.
    Flags { size: u8, enumeration: Option<u32>, sep: &'static str },
    /// A fixed-size inline character array.
    Chars(usize),
    /// Pointer and lengths (16 bytes) to a string.
    CharPtr,
    /// Pointer (8 bytes) to a NUL-terminated string.
    StrPtr,
    Struct(u32),
    /// Pointer (8 bytes) to a structure; its block names the type.
    StructPtr,
    /// Elements inline (`Some(count)`), or behind a 16-byte pointer header
    /// with 16-bit counts, or a 32-bit count when `wide`.
    Array { elem: Box<Kind>, inline: Option<usize>, wide: bool },
    /// Pointer (8 bytes) to a whole data block.
    Bytes,
    /// A PSO map: a 24-byte header pointing to key/item structures.
    Map,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub name: u32,
    pub offset: usize,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub(crate) struct StructInfo {
    pub size: usize,
    pub fields: Vec<Field>,
}

#[derive(Default, Debug)]
pub(crate) struct Schema {
    pub structs: HashMap<u32, StructInfo>,
    /// Enum values: (name hash, value).
    pub enums: HashMap<u32, Vec<(u32, i64)>>,
}

impl Schema {
    fn info(&self, ty: u32, names: &Names) -> Result<&StructInfo, Error> {
        self.structs.get(&ty).ok_or_else(|| Error::Format(format!("structure {} is not described in the file", names.name(ty))))
    }

    fn size(&self, kind: &Kind) -> usize {
        match kind {
            Kind::Bool | Kind::I8 | Kind::U8 => 1,
            Kind::I16 | Kind::U16 | Kind::Half => 2,
            Kind::I32 | Kind::U32 | Kind::HexU32 | Kind::F32 | Kind::Hash => 4,
            Kind::U64 | Kind::Vec2 | Kind::StrPtr | Kind::StructPtr | Kind::Bytes => 8,
            Kind::Vec3 => 12,
            Kind::Vec3Padded | Kind::Vec4 | Kind::CharPtr => 16,
            Kind::Map => 24,
            Kind::Enum { size, .. } | Kind::Flags { size, .. } => *size as usize,
            Kind::Chars(n) => *n,
            Kind::Struct(t) => self.structs.get(t).map_or(0, |s| s.size),
            Kind::Array { inline: Some(n), elem, .. } => n.saturating_mul(self.size(elem)),
            Kind::Array { inline: None, .. } => 16,
        }
    }

    /// The alignment of a field of this kind. PSO files hold their data
    /// packed at these alignments, not on 16-byte boundaries.
    fn align(&self, kind: &Kind) -> usize {
        match kind {
            Kind::Bool | Kind::I8 | Kind::U8 | Kind::Chars(_) => 1,
            Kind::I16 | Kind::U16 | Kind::Half => 2,
            Kind::I32 | Kind::U32 | Kind::HexU32 | Kind::F32 | Kind::Hash | Kind::Vec2 | Kind::Vec3 => 4,
            Kind::U64 | Kind::StrPtr | Kind::StructPtr | Kind::Bytes | Kind::CharPtr | Kind::Map | Kind::Array { inline: None, .. } => 8,
            Kind::Vec3Padded | Kind::Vec4 => 16,
            Kind::Enum { size, .. } | Kind::Flags { size, .. } => (*size as usize).max(1),
            Kind::Struct(t) => self.struct_align(*t),
            Kind::Array { inline: Some(_), elem, .. } => self.align(elem),
        }
    }

    fn struct_align(&self, ty: u32) -> usize {
        self.structs.get(&ty).map_or(1, |s| s.fields.iter().map(|f| self.align(&f.kind)).max().unwrap_or(1))
    }
}

/// A value in the tree. Floats are kept as bits so values compare exactly.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Bool(bool),
    Int(i64),
    Float(u32),
    Vec([u32; 4]),
    Hash(u32),
    /// None for a null pointer (empty strings are stored as null).
    Str(Option<String>),
    /// One value per field.
    Struct(Vec<Value>),
    /// A structure pointer: the target's type and value.
    Ptr(Option<(u32, Box<Value>)>),
    Array(Vec<Value>),
    Bytes(Vec<u8>),
    /// Entry structures (key and item) of a PSO map, with their type.
    Map(u32, Vec<Value>),
}

/// Details of the original file reused when writing: block types by kind
/// of content, and how string lengths were counted.
#[derive(Default, Debug)]
pub(crate) struct Hints {
    blocks: HashMap<String, u32>,
    /// Whether CharPtr lengths include the terminating NUL.
    nul_in_count: Option<bool>,
    /// Header of PSO maps (first 8 bytes) and entry types, by structure and field.
    map_heads: HashMap<(u32, usize), ([u8; 8], u32)>,
}

/// A name for the content of a pointed-to block, for block type hints.
fn content_key(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Struct(_) => return None,
        Kind::StructPtr => "pointer".into(),
        Kind::Array { .. } => "array".into(),
        other => format!("{other:?}"),
    })
}

/// The blocks of a file: (type, data) by 1-based id.
pub(crate) struct Blocks {
    pub list: Vec<(u32, Vec<u8>)>,
    pub big_endian: bool,
}

impl Blocks {
    fn get(&self, id: u32) -> Result<&(u32, Vec<u8>), Error> {
        (id as usize).checked_sub(1).and_then(|i| self.list.get(i)).ok_or_else(|| Error::Format(format!("missing data block {id}")))
    }

    fn uint(&self, data: &[u8], at: usize, size: usize) -> Result<u64, Error> {
        // PSO 64-bit values (pointers mostly) are two big-endian halves, low half first.
        if size == 8 && self.big_endian {
            return Ok(self.uint(data, at, 4)? | (self.uint(data, at + 4, 4)? << 32));
        }
        let b = data.get(at..at + size).ok_or_else(|| Error::Format("data is truncated".into()))?;
        let mut v = 0u64;
        for i in 0..size {
            let byte = if self.big_endian { b[i] } else { b[size - 1 - i] };
            v = (v << 8) | byte as u64;
        }
        Ok(v)
    }
}

/// Splits a block pointer into (block id, offset).
fn split_ptr(ptr: u64) -> (u32, usize) {
    ((ptr & 0xFFF) as u32, ((ptr >> 12) & 0xFFFFF) as usize)
}

fn make_ptr(id: u32, offset: usize) -> u64 {
    id as u64 | ((offset as u64) << 12)
}

struct Reader<'a> {
    schema: &'a Schema,
    blocks: &'a Blocks,
    names: &'a Names,
    hints: Hints,
}

impl Reader<'_> {
    fn read_struct(&mut self, ty: u32, id: u32, offset: usize, depth: u32) -> Result<Value, Error> {
        if depth > 64 {
            return Err(Error::Format("structures nest too deeply".into()));
        }
        let info = self.schema.info(ty, self.names)?;
        let len = self.blocks.get(id)?.1.len();
        // As in CodeWalker: some PSO pointers keep the offset shifted left by 8.
        let offset = if offset >= len && offset >> 8 < len { offset >> 8 } else { offset };
        if offset + info.size > len {
            return Err(Error::Format(format!("structure {} is truncated", self.names.name(ty))));
        }
        let mut values = Vec::with_capacity(info.fields.len());
        for (i, field) in info.fields.iter().enumerate() {
            let value = if field.kind == Kind::Map {
                self.read_map(ty, i, id, offset + field.offset, depth)?
            } else {
                self.read(&field.kind, id, offset + field.offset, depth)?
            };
            values.push(value);
        }
        Ok(Value::Struct(values))
    }

    fn read(&mut self, kind: &Kind, id: u32, at: usize, depth: u32) -> Result<Value, Error> {
        let blocks = self.blocks;
        let data = &blocks.get(id)?.1;
        let uint = |at: usize, size: usize| blocks.uint(data, at, size);
        Ok(match kind {
            Kind::Bool => Value::Bool(uint(at, 1)? != 0),
            Kind::I8 => Value::Int(uint(at, 1)? as i8 as i64),
            Kind::U8 => Value::Int(uint(at, 1)? as i64),
            Kind::I16 => Value::Int(uint(at, 2)? as i16 as i64),
            Kind::U16 | Kind::Half => Value::Int(uint(at, 2)? as i64),
            Kind::I32 => Value::Int(uint(at, 4)? as i32 as i64),
            Kind::U32 | Kind::HexU32 => Value::Int(uint(at, 4)? as i64),
            Kind::U64 => Value::Int(uint(at, 8)? as i64),
            Kind::F32 => Value::Float(uint(at, 4)? as u32),
            Kind::Vec2 | Kind::Vec3 | Kind::Vec3Padded | Kind::Vec4 => {
                let n = vec_len(kind);
                let mut v = [0u32; 4];
                for (i, c) in v.iter_mut().enumerate().take(n) {
                    *c = uint(at + i * 4, 4)? as u32;
                }
                Value::Vec(v)
            }
            Kind::Hash => Value::Hash(uint(at, 4)? as u32),
            Kind::Enum { size, .. } => {
                let v = uint(at, *size as usize)?;
                Value::Int(if *size == 4 { v as u32 as i32 as i64 } else { v as i64 })
            }
            Kind::Flags { size, .. } => Value::Int(uint(at, *size as usize)? as i64),
            Kind::Chars(n) => {
                let b = data.get(at..at + n).ok_or_else(|| Error::Format("data is truncated".into()))?;
                Value::Str(non_empty(&b[..b.iter().position(|&c| c == 0).unwrap_or(*n)]))
            }
            Kind::CharPtr => {
                let (ptr, count) = (uint(at, 8)?, uint(at + 8, 2)? as usize);
                let (bid, off) = split_ptr(ptr);
                if bid == 0 || count == 0 {
                    return Ok(Value::Str(None));
                }
                let (ty, block) = blocks.get(bid)?;
                self.hints.blocks.entry("chars".into()).or_insert(*ty);
                let b = block.get(off..(off + count).min(block.len())).unwrap_or_default();
                self.hints.nul_in_count.get_or_insert(b.last() == Some(&0));
                Value::Str(non_empty(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]))
            }
            Kind::StrPtr => {
                let (bid, off) = split_ptr(uint(at, 8)?);
                if bid == 0 {
                    return Ok(Value::Str(None));
                }
                let (ty, block) = blocks.get(bid)?;
                self.hints.blocks.entry("string".into()).or_insert(*ty);
                let b = block.get(off..).unwrap_or_default();
                Value::Str(non_empty(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]))
            }
            Kind::Struct(ty) => self.read_struct(*ty, id, at, depth + 1)?,
            Kind::StructPtr => {
                let ptr = uint(at, 8)?;
                let (bid, off) = split_ptr(ptr);
                if ptr == 0 || bid == 0 {
                    return Ok(Value::Ptr(None));
                }
                let ty = blocks.get(bid)?.0;
                Value::Ptr(Some((ty, Box::new(self.read_struct(ty, bid, off, depth + 1)?))))
            }
            Kind::Array { elem, inline: Some(count), .. } => {
                let stride = self.schema.size(elem);
                // Elements that read nothing (empty structures) never run out of data.
                if stride == 0 && *count > blocks.get(id)?.1.len() {
                    return Err(Error::Format("array longer than its data".into()));
                }
                let items = (0..*count).map(|i| self.read(elem, id, at + i * stride, depth + 1)).collect::<Result<_, _>>()?;
                Value::Array(items)
            }
            Kind::Array { elem, inline: None, wide } => {
                let (ptr, count) = (uint(at, 8)?, uint(at + 8, if *wide { 4 } else { 2 })? as usize);
                let (mut bid, mut off) = split_ptr(ptr);
                if ptr == 0 || count == 0 || bid == 0 {
                    return Ok(Value::Array(Vec::new()));
                }
                if let Some(key) = content_key(elem) {
                    let ty = blocks.get(bid)?.0;
                    self.hints.blocks.entry(key).or_insert(ty);
                }
                let empty = self.schema.size(elem) == 0;
                let stride = self.schema.size(elem).max(1);
                // The count comes from the file: memory follows the data.
                let mut items = Vec::with_capacity(count.min(blocks.get(bid)?.1.len() / stride + 1));
                for _ in 0..count {
                    // Long arrays may continue in the next block (resource metadata).
                    if off + stride > blocks.get(bid)?.1.len() && blocks.get(bid + 1).is_ok() {
                        (bid, off) = (bid + 1, 0);
                    }
                    // Elements that read nothing (empty structures) never run out of data.
                    if empty && off >= blocks.get(bid)?.1.len() {
                        return Err(Error::Format("array past the end of its data".into()));
                    }
                    items.push(self.read(elem, bid, off, depth + 1)?);
                    off += stride;
                }
                Value::Array(items)
            }
            Kind::Bytes => {
                let ptr = uint(at, 8)?;
                let (bid, _) = split_ptr(ptr);
                if ptr == 0 || bid == 0 {
                    return Ok(Value::Bytes(Vec::new()));
                }
                let (ty, block) = blocks.get(bid)?;
                self.hints.blocks.entry("bytes".into()).or_insert(*ty);
                Value::Bytes(block.clone())
            }
            Kind::Map => return Err(Error::Format("map outside a structure".into())),
        })
    }

    /// A PSO map: an 8-byte header, then an array header pointing to entries.
    fn read_map(&mut self, owner: u32, field: usize, id: u32, at: usize, depth: u32) -> Result<Value, Error> {
        let blocks = self.blocks;
        let data = &blocks.get(id)?.1;
        let head: [u8; 8] = data.get(at..at + 8).and_then(|b| b.try_into().ok()).ok_or_else(|| Error::Format("data is truncated".into()))?;
        if blocks.uint(data, at, 4)? != 0x0100_0000 {
            return Err(Error::Unsupported("this PSO map layout".into()));
        }
        let (ptr, count) = (blocks.uint(data, at + 8, 8)?, blocks.uint(data, at + 16, 2)? as usize);
        let (bid, off) = split_ptr(ptr);
        if ptr == 0 || count == 0 || bid == 0 {
            let ty = self.hints.map_heads.get(&(owner, field)).map_or(0, |h| h.1);
            return Ok(Value::Map(ty, Vec::new()));
        }
        let ty = blocks.get(bid)?.0;
        self.hints.map_heads.insert((owner, field), (head, ty));
        let size = self.schema.info(ty, self.names)?.size;
        let entries = (0..count).map(|i| self.read_struct(ty, bid, off + i * size, depth + 1)).collect::<Result<_, _>>()?;
        Ok(Value::Map(ty, entries))
    }
}

fn non_empty(bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned())
}

fn vec_len(kind: &Kind) -> usize {
    match kind {
        Kind::Vec2 => 2,
        Kind::Vec3 | Kind::Vec3Padded => 3,
        _ => 4,
    }
}

/// Largest offset a block pointer can hold.
const MAX_BLOCK: usize = 0xFFFFF;

struct Writer<'a> {
    schema: &'a Schema,
    names: &'a Names,
    hints: &'a Hints,
    big_endian: bool,
    /// Pack each allocation at its type's alignment (PSO files), instead of
    /// on 16-byte boundaries (resources).
    packed: bool,
    /// Block types to use when the original file had no such block.
    default_block: fn(&str) -> u32,
    blocks: Vec<(u32, Vec<u8>)>,
}

impl Writer<'_> {
    /// Room for `len` bytes in a block of type `ty`, aligned to `align`
    /// (16 bytes in resources).
    fn alloc(&mut self, ty: u32, len: usize, align: usize) -> Result<(u32, usize), Error> {
        let align = if self.packed { align.max(1) } else { 16 };
        let fits = |d: &Vec<u8>| d.len().next_multiple_of(align) + len <= MAX_BLOCK;
        let index = match self.blocks.iter().rposition(|(t, d)| *t == ty && fits(d)) {
            Some(i) => i,
            None if len <= MAX_BLOCK => {
                self.blocks.push((ty, Vec::new()));
                self.blocks.len() - 1
            }
            None => return Err(Error::Unsupported("a data block larger than 1 MB".into())),
        };
        let data = &mut self.blocks[index].1;
        let offset = data.len().next_multiple_of(align);
        data.resize(offset + len, 0);
        Ok((index as u32 + 1, offset))
    }

    fn block_type(&self, key: &str) -> u32 {
        self.hints.blocks.get(key).copied().unwrap_or_else(|| (self.default_block)(key))
    }

    fn put_uint(&mut self, id: u32, at: usize, size: usize, value: u64) {
        if size == 8 && self.big_endian {
            self.put_uint(id, at, 4, value & 0xFFFF_FFFF);
            return self.put_uint(id, at + 4, 4, value >> 32);
        }
        let data = &mut self.blocks[id as usize - 1].1;
        for i in 0..size {
            let byte = (value >> (8 * i)) as u8;
            let pos = if self.big_endian { at + size - 1 - i } else { at + i };
            data[pos] = byte;
        }
    }

    fn put_bytes(&mut self, id: u32, at: usize, bytes: &[u8]) {
        self.blocks[id as usize - 1].1[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn write_struct(&mut self, ty: u32, value: &Value, id: u32, at: usize) -> Result<(), Error> {
        let info = self.schema.info(ty, self.names)?.clone();
        let Value::Struct(values) = value else { return Err(mismatch(&self.names.name(ty))) };
        for (i, (field, value)) in info.fields.iter().zip(values).enumerate() {
            if field.kind == Kind::Map {
                self.write_map(ty, i, value, id, at + field.offset)?;
            } else {
                self.write(&field.kind, value, id, at + field.offset)?;
            }
        }
        Ok(())
    }

    fn write(&mut self, kind: &Kind, value: &Value, id: u32, at: usize) -> Result<(), Error> {
        match (kind, value) {
            (Kind::Bool, Value::Bool(b)) => self.put_uint(id, at, 1, *b as u64),
            (Kind::I8 | Kind::U8, Value::Int(v)) => self.put_uint(id, at, 1, *v as u64),
            (Kind::I16 | Kind::U16 | Kind::Half, Value::Int(v)) => self.put_uint(id, at, 2, *v as u64),
            (Kind::I32 | Kind::U32 | Kind::HexU32, Value::Int(v)) => self.put_uint(id, at, 4, *v as u64),
            (Kind::U64, Value::Int(v)) => self.put_uint(id, at, 8, *v as u64),
            (Kind::Enum { size, .. } | Kind::Flags { size, .. }, Value::Int(v)) => self.put_uint(id, at, *size as usize, *v as u64),
            (Kind::F32, Value::Float(bits)) => self.put_uint(id, at, 4, *bits as u64),
            (Kind::Vec2 | Kind::Vec3 | Kind::Vec3Padded | Kind::Vec4, Value::Vec(v)) => {
                for (i, c) in v.iter().take(vec_len(kind)).enumerate() {
                    self.put_uint(id, at + i * 4, 4, *c as u64);
                }
            }
            (Kind::Hash, Value::Hash(h)) => self.put_uint(id, at, 4, *h as u64),
            (Kind::Chars(n), Value::Str(s)) => {
                let s = s.as_deref().unwrap_or("");
                if s.len() >= *n {
                    return Err(Error::Edit(format!("\"{s}\" is longer than {} characters", n - 1)));
                }
                self.put_bytes(id, at, s.as_bytes());
            }
            (Kind::CharPtr, Value::Str(s)) => {
                if let Some(s) = s {
                    let (bid, off) = self.alloc(self.block_type("chars"), s.len() + 1, 1)?;
                    self.put_bytes(bid, off, s.as_bytes());
                    let count = s.len() as u64 + self.hints.nul_in_count.unwrap_or(false) as u64;
                    self.put_uint(id, at, 8, make_ptr(bid, off));
                    self.put_uint(id, at + 8, 2, count);
                    self.put_uint(id, at + 10, 2, count);
                }
            }
            (Kind::StrPtr, Value::Str(s)) => {
                if let Some(s) = s {
                    let (bid, off) = self.alloc(self.block_type("string"), s.len() + 1, 1)?;
                    self.put_bytes(bid, off, s.as_bytes());
                    self.put_uint(id, at, 8, make_ptr(bid, off));
                }
            }
            (Kind::Struct(ty), v @ Value::Struct(_)) => self.write_struct(*ty, v, id, at)?,
            (Kind::StructPtr, Value::Ptr(p)) => {
                if let Some((ty, v)) = p {
                    let size = self.schema.info(*ty, self.names)?.size.max(1);
                    let (bid, off) = self.alloc(*ty, size, self.schema.struct_align(*ty))?;
                    self.write_struct(*ty, v, bid, off)?;
                    self.put_uint(id, at, 8, make_ptr(bid, off));
                }
            }
            (Kind::Array { elem, inline: Some(count), .. }, Value::Array(items)) => {
                if items.len() > *count {
                    return Err(Error::Edit(format!("an array holds at most {count} items")));
                }
                let stride = self.schema.size(elem);
                for (i, item) in items.iter().enumerate() {
                    self.write(elem, item, id, at + i * stride)?;
                }
            }
            (Kind::Array { elem, inline: None, wide }, Value::Array(items)) => {
                if !items.is_empty() {
                    let stride = self.schema.size(elem).max(1);
                    let ty = match &**elem {
                        Kind::Struct(ty) => *ty,
                        other => self.block_type(&content_key(other).unwrap()),
                    };
                    let (bid, off) = self.alloc(ty, stride * items.len(), self.schema.align(elem))?;
                    for (i, item) in items.iter().enumerate() {
                        self.write(elem, item, bid, off + i * stride)?;
                    }
                    self.put_uint(id, at, 8, make_ptr(bid, off));
                    let n = if *wide { 4 } else { 2 };
                    self.put_uint(id, at + 8, n, items.len() as u64);
                    self.put_uint(id, at + 8 + n, n, items.len() as u64);
                }
            }
            (Kind::Bytes, Value::Bytes(bytes)) => {
                if !bytes.is_empty() {
                    // A block of its own.
                    self.blocks.push((self.block_type("bytes"), bytes.clone()));
                    let bid = self.blocks.len() as u32;
                    self.put_uint(id, at, 8, make_ptr(bid, 0));
                }
            }
            _ => return Err(mismatch(&format!("{kind:?}"))),
        }
        Ok(())
    }

    fn write_map(&mut self, owner: u32, field: usize, value: &Value, id: u32, at: usize) -> Result<(), Error> {
        let Value::Map(ty, entries) = value else { return Err(mismatch("map")) };
        let head = self.hints.map_heads.get(&(owner, field)).map(|h| h.0);
        let head = head.unwrap_or(if self.big_endian { [1, 0, 0, 0, 0, 0, 0, 0] } else { [0, 0, 0, 1, 0, 0, 0, 0] });
        self.put_bytes(id, at, &head);
        if entries.is_empty() {
            return Ok(());
        }
        let size = self.schema.info(*ty, self.names)?.size.max(1);
        let (bid, off) = self.alloc(*ty, size * entries.len(), self.schema.struct_align(*ty))?;
        for (i, entry) in entries.iter().enumerate() {
            self.write_struct(*ty, entry, bid, off + i * size)?;
        }
        self.put_uint(id, at + 8, 8, make_ptr(bid, off));
        self.put_uint(id, at + 16, 2, entries.len() as u64);
        self.put_uint(id, at + 18, 2, entries.len() as u64);
        Ok(())
    }
}

fn mismatch(what: &str) -> Error {
    Error::Format(format!("value does not match its type ({what})"))
}

enum Container {
    Resource(rsc::Header),
    Pso(pso::Header),
    Rbf(rbf::Node),
}

/// A metadata file, ready to convert to XML and back.
pub struct MetaFile {
    container: Container,
    schema: Schema,
    root_type: u32,
    root: Value,
    hints: Hints,
    names: Names,
    data_len: usize,
}

impl MetaFile {
    /// Whether the data looks like resource metadata, a PSO or an RBF file.
    pub fn is_meta(data: &[u8]) -> bool {
        data.starts_with(b"PSIN") || data.starts_with(b"RBF0") || (data.starts_with(b"RSC7") && data.get(4..8) == Some(&2u32.to_le_bytes()[..]))
    }

    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        let mut names = Names::new();
        let (container, schema, blocks, root_id) = if data.starts_with(b"PSIN") {
            let (header, schema, blocks, root) = pso::parse(data, &mut names)?;
            (Container::Pso(header), schema, blocks, root)
        } else if data.starts_with(b"RSC7") && Self::is_meta(data) {
            let (header, schema, blocks, root) = rsc::parse(data)?;
            (Container::Resource(header), schema, blocks, root)
        } else if data.starts_with(b"RBF0") {
            let (schema, root, hints, data_len) = (Schema::default(), Value::Struct(Vec::new()), Hints::default(), data.len());
            return Ok(MetaFile { container: Container::Rbf(rbf::parse(data)?), schema, root_type: 0, root, hints, names, data_len });
        } else {
            return Err(Error::Format("not a metadata file".into()));
        };
        let root_type = blocks.get(root_id)?.0;
        let data_len = blocks.list.iter().map(|(_, d)| d.len()).sum();
        let mut reader = Reader { schema: &schema, blocks: &blocks, names: &names, hints: Hints::default() };
        let root = reader.read_struct(root_type, root_id, 0, 0)?;
        let hints = reader.hints;
        Ok(MetaFile { container, schema, root_type, root, hints, names, data_len })
    }

    /// Bytes of data in the file's blocks (for checks: a rebuilt file much
    /// smaller than the original would have lost data).
    #[doc(hidden)]
    pub fn data_len(&self) -> usize {
        self.data_len
    }

    /// "PSO", "resource metadata" or "RBF".
    pub fn kind(&self) -> &'static str {
        match self.container {
            Container::Pso(_) => "PSO",
            Container::Resource(_) => "resource metadata",
            Container::Rbf(_) => "RBF",
        }
    }

    /// Learns names from a text (an XPath, an XML snippet), so values that
    /// hash them show by name.
    pub fn learn_names(&mut self, text: &str) {
        self.names.add_words(text);
    }

    /// Whether the CHKS checksum of a PSO file is right (None without one).
    #[doc(hidden)]
    pub fn pso_checksum_ok(data: &[u8]) -> Option<bool> {
        pso::verify_checksum(data)
    }

    pub fn to_xml(&self) -> String {
        if let Container::Rbf(root) = &self.container {
            return rbf::to_xml(root);
        }
        xml::write(&self.schema, &self.names, self.root_type, &self.root)
    }

    /// Builds a file of the same kind from XML, using this file's schema.
    pub fn rebuild_from_xml(&self, text: &str) -> Result<Vec<u8>, Error> {
        if let Container::Rbf(_) = self.container {
            return rbf::write(&rbf::from_xml(text)?);
        }
        let map_types: Vec<u32> = self.hints.map_heads.values().map(|h| h.1).collect();
        let root = xml::read(&self.schema, &self.names, self.root_type, text, &map_types)?;
        self.write(&root)
    }

    fn write(&self, root: &Value) -> Result<Vec<u8>, Error> {
        let big_endian = matches!(self.container, Container::Pso(_));
        let default_block = match self.container {
            Container::Pso(_) => pso::default_block,
            _ => rsc::default_block,
        };
        // A PSO keeps its blocks in the order of the original file, when each of them is still used.
        let original: Vec<(u32, Vec<u8>)> = match &self.container {
            Container::Pso(h) => h.block_types.iter().map(|&t| (t, Vec::new())).collect(),
            _ => Vec::new(),
        };
        let fill = |blocks: Vec<(u32, Vec<u8>)>| -> Result<(Writer, u32), Error> {
            let mut w = Writer { schema: &self.schema, names: &self.names, hints: &self.hints, big_endian, packed: big_endian, default_block, blocks };
            let size = self.schema.info(self.root_type, &self.names)?.size.max(1);
            let (id, at) = w.alloc(self.root_type, size, self.schema.struct_align(self.root_type))?;
            w.write_struct(self.root_type, root, id, at)?;
            Ok((w, id))
        };
        let (mut w, mut id) = fill(original)?;
        if w.blocks.iter().any(|(_, data)| data.is_empty()) {
            (w, id) = fill(Vec::new())?;
        }
        match &self.container {
            Container::Pso(h) => pso::write(h, &w.blocks, id),
            Container::Resource(h) => rsc::write(h, &w.blocks, id),
            Container::Rbf(root) => rbf::write(root),
        }
    }
}
