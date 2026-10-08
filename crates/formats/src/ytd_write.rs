//! Rebuilding texture dictionaries: replacing, adding, removing and renaming
//! textures.
//!
//! Every structure is copied from the original file, so unknown fields keep
//! their values; only the edited textures' headers change. The blocks are
//! then laid out again into pages (see [`crate::pages`]) and recompressed.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

use crate::Error;
use crate::dds;
use crate::encode;
use crate::hash::joaat_lower;
use crate::pages::{self, MAX_PAGES};
use crate::rsc::{Resource, u16_at, u64_at};
use crate::ytd::Format;

const SYS: u64 = 0x5000_0000;
const GFX: u64 = 0x6000_0000;
/// CodeWalker reserves the pages-info block for 128 pages.
const PAGES_INFO_LEN: usize = 16 + 8 * 128;

/// A change to a texture dictionary. Names are matched case-insensitively.
pub enum Edit<'a> {
    /// A new image for a texture, from a DDS or PNG file. PNGs are compressed
    /// to `format`, or else to the texture's current format when possible.
    Replace { name: &'a str, file: &'a [u8], format: Option<Format> },
    /// A new texture from a DDS or PNG file. PNGs are compressed to `format`,
    /// or else to BC1 if opaque and BC3 if not.
    Add { name: &'a str, file: &'a [u8], format: Option<Format> },
    Remove(&'a str),
    Rename { from: &'a str, to: &'a str },
}

struct Tex {
    header: Vec<u8>,
    /// With its terminating NUL.
    name: Vec<u8>,
    hash: u32,
    data: Vec<u8>,
}

impl Tex {
    fn is(&self, name: &str) -> bool {
        self.name[..self.name.len() - 1].eq_ignore_ascii_case(name.as_bytes())
    }
}

/// Rebuilds a `.ytd` file, replacing the named textures with DDS files.
pub fn replace_textures(ytd: &[u8], replacements: &[(&str, &[u8])]) -> Result<Vec<u8>, Error> {
    let edits: Vec<Edit> = replacements.iter().map(|&(name, file)| Edit::Replace { name, file, format: None }).collect();
    edit_textures(ytd, &edits)
}

/// Rebuilds a `.ytd` file with the given changes, applied in order.
pub fn edit_textures(ytd: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
    let res = Resource::parse(ytd)?;
    let enhanced = match res.version {
        13 => false,
        5 => true,
        v => return Err(Error::Unsupported(format!("texture dictionary version {v}"))),
    };
    let header_len = if enhanced { 0x80 } else { 0x90 };
    let mut root = res.block(SYS, 64)?.to_vec();
    let hash_count = u16_at(&root, 0x28) as usize;
    let hashes = if hash_count == 0 { &[][..] } else { res.block(u64_at(&root, 0x20), hash_count * 4)? };
    let count = u16_at(&root, 0x38) as usize;
    let pointers = if count == 0 { &[][..] } else { res.block(u64_at(&root, 0x30), count * 8)? };

    let mut textures = Vec::new();
    for (i, p) in pointers.as_chunks::<8>().0.iter().enumerate() {
        let ptr = u64::from_le_bytes(*p);
        if ptr == 0 {
            continue;
        }
        let header = res.block(ptr, header_len)?.to_vec();
        if enhanced && u64_at(&header, 0x30) != ptr + 0x58 {
            return Err(Error::Unsupported("Enhanced texture with a separate shader view".into()));
        }
        let name_ptr = u64_at(&header, 0x28);
        let mut name = if name_ptr == 0 { Vec::new() } else { res.string(name_ptr)?.into_bytes() };
        let hash = match hashes.get(i * 4..i * 4 + 4) {
            Some(h) => u32::from_le_bytes(h.try_into().unwrap()),
            None => joaat_lower(&String::from_utf8_lossy(&name)),
        };
        name.push(0);
        let (format, width, height, levels, data_ptr) = fields(&header, enhanced)?;
        let len: usize = (0..levels.max(1)).map(|i| format.level_size((width >> i).max(1), (height >> i).max(1))).sum();
        let data = res.bytes(data_ptr, len)?.to_vec();
        textures.push(Tex { header, name, hash, data });
    }

    for edit in edits {
        let find = |textures: &[Tex], name: &str| {
            textures.iter().position(|t| t.is(name)).ok_or_else(|| Error::Edit(format!("no texture named {name}")))
        };
        let check_new = |textures: &[Tex], name: &str| {
            if name.is_empty() || name.contains(['\0', '/', '\\']) {
                Err(Error::Edit(format!("invalid texture name \"{name}\"")))
            } else if textures.iter().any(|t| t.is(name)) {
                Err(Error::Edit(format!("a texture named {name} already exists")))
            } else {
                Ok(())
            }
        };
        match *edit {
            Edit::Replace { name, file, format } => {
                let i = find(&textures, name)?;
                let tex = &mut textures[i];
                let (current, ..) = fields(&tex.header, enhanced)?;
                let mut image = encode::load(file, format.or(Some(current).filter(|f| f.can_encode())))?;
                if enhanced && !file.starts_with(b"DDS ") {
                    // A picture keeps the texture's colour space.
                    image.srgb = Format::from_enhanced(tex.header[0x1F]).is_some_and(|(_, srgb)| srgb);
                }
                set_image(&mut tex.header, &image, enhanced)?;
                tex.data = image.data;
            }
            Edit::Add { name, file, format } => {
                check_new(&textures, name)?;
                let image = encode::load(file, format)?;
                let mut header = new_header(name, enhanced);
                set_image(&mut header, &image, enhanced)?;
                textures.push(Tex { header, name: [name.as_bytes(), &[0]].concat(), hash: joaat_lower(name), data: image.data });
            }
            Edit::Remove(name) => {
                textures.remove(find(&textures, name)?);
            }
            Edit::Rename { from, to } => {
                let i = find(&textures, from)?;
                if !from.eq_ignore_ascii_case(to) {
                    check_new(&textures, to)?;
                }
                textures[i].name = [to.as_bytes(), &[0]].concat();
                textures[i].hash = joaat_lower(to);
            }
        }
    }
    // The game looks textures up by binary search on the hashes.
    textures.sort_by_key(|t| t.hash);
    let hashes: Vec<u8> = textures.iter().flat_map(|t| t.hash.to_le_bytes()).collect();

    // System blocks: root, pages info, hash list, pointer list, headers, names.
    let mut sys_sizes = vec![64, PAGES_INFO_LEN, hashes.len().max(1), textures.len().max(1) * 8];
    sys_sizes.extend(textures.iter().map(|t| t.header.len()));
    sys_sizes.extend(textures.iter().map(|t| t.name.len()));
    let gfx_sizes: Vec<usize> = textures.iter().map(|t| t.data.len().max(1)).collect();
    let sys = pages::layout(&sys_sizes, true, MAX_PAGES)?;
    let gfx = pages::layout(&gfx_sizes, false, MAX_PAGES - sys.pages)?;

    let n = textures.len();
    let at = |i: usize| SYS + sys.offsets[i] as u64;
    let (header_at, name_at) = (|i: usize| at(4 + i), |i: usize| at(4 + n + i));
    let mut system = vec![0u8; sys.size];
    let mut graphics = vec![0u8; gfx.size];
    let put = |seg: &mut Vec<u8>, ptr: u64, base: u64, bytes: &[u8]| {
        let o = (ptr - base) as usize;
        seg[o..o + bytes.len()].copy_from_slice(bytes);
    };

    root[0x08..0x10].copy_from_slice(&at(1).to_le_bytes());
    let list = |ptr: u64| if n == 0 { 0 } else { ptr };
    root[0x20..0x28].copy_from_slice(&list(at(2)).to_le_bytes());
    root[0x28..0x2C].copy_from_slice(&[(n as u16).to_le_bytes(), (n as u16).to_le_bytes()].concat());
    root[0x30..0x38].copy_from_slice(&list(at(3)).to_le_bytes());
    root[0x38..0x3C].copy_from_slice(&[(n as u16).to_le_bytes(), (n as u16).to_le_bytes()].concat());
    put(&mut system, at(0), SYS, &root);
    let mut info = [0u8; 16];
    info[8] = sys.pages as u8;
    info[9] = gfx.pages as u8;
    put(&mut system, at(1), SYS, &info);
    put(&mut system, at(2), SYS, &hashes);
    for (i, tex) in textures.iter_mut().enumerate() {
        put(&mut system, at(3) + i as u64 * 8, SYS, &header_at(i).to_le_bytes());
        let data_ptr = GFX + gfx.offsets[i] as u64;
        let name_ptr = if tex.name.len() > 1 { name_at(i) } else { 0 };
        tex.header[0x28..0x30].copy_from_slice(&name_ptr.to_le_bytes());
        if enhanced {
            tex.header[0x30..0x38].copy_from_slice(&(header_at(i) + 0x58).to_le_bytes());
            tex.header[0x38..0x40].copy_from_slice(&data_ptr.to_le_bytes());
        } else {
            tex.header[0x70..0x78].copy_from_slice(&data_ptr.to_le_bytes());
        }
        put(&mut system, header_at(i), SYS, &tex.header);
        put(&mut system, name_at(i), SYS, &tex.name);
        put(&mut graphics, data_ptr, GFX, &tex.data);
    }

    let version = res.version;
    let mut out = Vec::with_capacity(16 + system.len() / 2);
    for word in [0x3743_5352, version, sys.flags | ((version >> 4) << 28), gfx.flags | ((version & 0xF) << 28)] {
        out.extend_from_slice(&u32::to_le_bytes(word));
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&system)?;
    z.write_all(&graphics)?;
    Ok(z.finish()?)
}

/// A header for a new texture, with the values most common in the game's
/// files (Legacy) or CodeWalker's defaults (Enhanced).
fn new_header(name: &str, enhanced: bool) -> Vec<u8> {
    let lower = name.to_ascii_lowercase();
    // TextureUsage: NORMAL, SPECULAR or DIFFUSE, by the usual name suffixes.
    let kind = if lower.ends_with("_n") { 22 } else if lower.ends_with("_s") { 23 } else { 20 };
    let usage = 0x2000_0000u32 | kind;
    let mut h = vec![0u8; if enhanced { 0x80 } else { 0x90 }];
    let mut put = |at: usize, bytes: &[u8]| h[at..at + bytes.len()].copy_from_slice(bytes);
    put(0x04, &1u32.to_le_bytes());
    put(0x40, &usage.to_le_bytes());
    if enhanced {
        put(0x10, &0x0026_0208u32.to_le_bytes()); // flags
        put(0x1E, &[1]); // 2D
        put(0x20, &[0xFF]); // tile mode: auto
        put(0x23, &[0x28]);
        put(0x26, &1u16.to_le_bytes()); // usage count
        put(0x44, &2u32.to_le_bytes());
        // The embedded shader resource view: a 2D texture.
        put(0x58, &0x1_406B_77D8u64.to_le_bytes());
        put(0x68, &0x41u16.to_le_bytes());
        put(0x6A, &0xFFFFu16.to_le_bytes());
        put(0x6C, &u32::MAX.to_le_bytes());
    } else {
        put(0x00, &0x4061_7568u32.to_le_bytes());
        put(0x30, &1u16.to_le_bytes());
        put(0x32, &0x80u16.to_le_bytes());
    }
    h
}

/// Format, width, height, mip levels and data pointer of a texture header.
fn fields(h: &[u8], enhanced: bool) -> Result<(Format, u32, u32, u32, u64), Error> {
    let unsupported = |code: u32| Error::Unsupported(format!("texture format {code:#x}"));
    Ok(if enhanced {
        let format = Format::from_enhanced(h[0x1F]).ok_or(unsupported(h[0x1F] as u32))?.0;
        (format, u16_at(h, 0x18) as u32, u16_at(h, 0x1A) as u32, h[0x22] as u32, u64_at(h, 0x38))
    } else {
        let code = u32::from_le_bytes(h[0x58..0x5C].try_into().unwrap());
        let format = Format::from_legacy(code).ok_or(unsupported(code))?;
        (format, u16_at(h, 0x50) as u32, u16_at(h, 0x52) as u32, h[0x5D] as u32, u64_at(h, 0x70))
    })
}

/// Writes a new image's size and format into a texture header.
fn set_image(h: &mut [u8], image: &dds::Image, enhanced: bool) -> Result<(), Error> {
    let (w, h16) = ((image.width as u16).to_le_bytes(), (image.height as u16).to_le_bytes());
    let levels = image.levels.min(255) as u8;
    if enhanced {
        let code = image.format.enhanced_code(image.srgb);
        let (blocks, stride) = block_count(image.format, image.width, image.height, image.levels);
        h[0x08..0x0C].copy_from_slice(&blocks.to_le_bytes());
        h[0x0C..0x10].copy_from_slice(&stride.to_le_bytes());
        h[0x18..0x1A].copy_from_slice(&w);
        h[0x1A..0x1C].copy_from_slice(&h16);
        h[0x1C..0x1E].copy_from_slice(&1u16.to_le_bytes());
        h[0x1F] = code;
        h[0x22] = levels;
    } else {
        let code = image
            .format
            .legacy_code()
            .ok_or_else(|| Error::Unsupported(format!("{} textures in Legacy dictionaries", image.format.name())))?;
        h[0x50..0x52].copy_from_slice(&w);
        h[0x52..0x54].copy_from_slice(&h16);
        h[0x54..0x56].copy_from_slice(&1u16.to_le_bytes());
        h[0x56..0x58].copy_from_slice(&(image.format.stride(image.width) as u16).to_le_bytes());
        h[0x58..0x5C].copy_from_slice(&code.to_le_bytes());
        h[0x5D] = levels;
    }
    Ok(())
}

/// Enhanced headers count the data in blocks (pixels for uncompressed
/// formats); with mips, sizes are rounded up to powers of two first.
/// Follows CodeWalker's `TextureBase.GetBlockCount`.
fn block_count(format: Format, width: u32, height: u32, levels: u32) -> (u32, u32) {
    let (side, bytes) = format.block();
    let (mut w, mut h) = if levels > 1 { (width.next_power_of_two(), height.next_power_of_two()) } else { (width, height) };
    let mut count = 0;
    for _ in 0..levels {
        count += w.div_ceil(side).max(1) * h.div_ceil(side).max(1);
        w /= 2;
        h /= 2;
    }
    (count, bytes as u32)
}
