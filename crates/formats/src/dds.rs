//! DDS export.

use crate::Error;
use crate::ytd::{Format, Texture};

const DDSD_CAPS: u32 = 0x1;
const DDSD_HEIGHT: u32 = 0x2;
const DDSD_WIDTH: u32 = 0x4;
const DDSD_PIXELFORMAT: u32 = 0x1000;
const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
const DDSD_LINEARSIZE: u32 = 0x8_0000;
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x2_0000;
const DDSCAPS_COMPLEX: u32 = 0x8;
const DDSCAPS_TEXTURE: u32 = 0x1000;
const DDSCAPS_MIPMAP: u32 = 0x40_0000;

/// Pixel format: flags, FourCC, bit count and R, G, B, A masks.
type PixelFormat = (u32, &'static [u8; 4], u32, [u32; 4]);

pub fn write(tex: &Texture) -> Vec<u8> {
    let none = &[0u8; 4];
    let fourcc = |cc| (DDPF_FOURCC, cc, 0, [0; 4]);
    // sRGB and BC6H/BC7 need the DX10 extension header.
    let dxgi = match (tex.format, tex.srgb) {
        (Format::Bc1, true) => Some(72),
        (Format::Bc2, true) => Some(75),
        (Format::Bc3, true) => Some(78),
        (Format::Bc6h, _) => Some(95),
        (Format::Bc7, false) => Some(98),
        (Format::Bc7, true) => Some(99),
        _ => None,
    };
    let pf: PixelFormat = match tex.format {
        _ if dxgi.is_some() => fourcc(b"DX10"),
        Format::Bc1 => fourcc(b"DXT1"),
        Format::Bc2 => fourcc(b"DXT3"),
        Format::Bc3 => fourcc(b"DXT5"),
        Format::Bc4 => fourcc(b"ATI1"),
        Format::Bc5 => fourcc(b"ATI2"),
        Format::Bgra8 => (DDPF_RGB | DDPF_ALPHAPIXELS, none, 32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000]),
        Format::Bgrx8 => (DDPF_RGB, none, 32, [0xFF_0000, 0xFF00, 0xFF, 0]),
        Format::Rgba8 => (DDPF_RGB | DDPF_ALPHAPIXELS, none, 32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000]),
        Format::Bgr5a1 => (DDPF_RGB | DDPF_ALPHAPIXELS, none, 16, [0x7C00, 0x3E0, 0x1F, 0x8000]),
        Format::A8 => (DDPF_ALPHA, none, 8, [0, 0, 0, 0xFF]),
        Format::L8 => (DDPF_LUMINANCE, none, 8, [0xFF, 0, 0, 0]),
        Format::Bc6h | Format::Bc7 => unreachable!(),
    };

    let mut out = Vec::with_capacity(148 + tex.data.len());
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    let mipmapped = tex.levels > 1;
    put(u32::from_le_bytes(*b"DDS "));
    put(124);
    put(DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT | DDSD_MIPMAPCOUNT | DDSD_LINEARSIZE);
    put(tex.height);
    put(tex.width);
    put(tex.format.level_size(tex.width, tex.height) as u32);
    put(0); // depth
    put(tex.levels);
    (0..11).for_each(|_| put(0));
    let (flags, cc, bits, masks) = pf;
    put(32);
    put(flags);
    put(u32::from_le_bytes(*cc));
    put(bits);
    masks.into_iter().for_each(&mut put);
    put(DDSCAPS_TEXTURE | if mipmapped { DDSCAPS_COMPLEX | DDSCAPS_MIPMAP } else { 0 });
    (0..4).for_each(|_| put(0));
    if let Some(format) = dxgi {
        put(format);
        put(3); // D3D10_RESOURCE_DIMENSION_TEXTURE2D
        put(0);
        put(1); // array size
        put(0);
    }
    out.extend_from_slice(&tex.data);
    out
}

/// A texture read from a DDS file.
pub struct Image {
    pub format: Format,
    pub srgb: bool,
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    /// All mip levels, largest first.
    pub data: Vec<u8>,
}

/// Reads a 2D DDS texture (no cube maps, arrays or volumes).
pub fn read(dds: &[u8]) -> Result<Image, Error> {
    let bad = |what: &str| Error::Format(format!("DDS: {what}"));
    if dds.len() < 128 || &dds[..4] != b"DDS " || u32_at(dds, 4) != 124 {
        return Err(bad("not a DDS file"));
    }
    let flags = u32_at(dds, 8);
    let (height, width) = (u32_at(dds, 12), u32_at(dds, 16));
    let levels = if flags & DDSD_MIPMAPCOUNT != 0 { u32_at(dds, 28).max(1) } else { 1 };
    let (pf_flags, fourcc, bits) = (u32_at(dds, 80), &dds[84..88], u32_at(dds, 88));
    let masks = [u32_at(dds, 92), u32_at(dds, 96), u32_at(dds, 100), u32_at(dds, 104)];
    if u32_at(dds, 112) & 0x200 != 0 || u32_at(dds, 24) > 1 {
        return Err(Error::Unsupported("DDS cube maps and volume textures".into()));
    }
    let mut start = 128;
    let (format, srgb) = if pf_flags & DDPF_FOURCC != 0 {
        match fourcc {
            b"DXT1" => (Format::Bc1, false),
            b"DXT2" | b"DXT3" => (Format::Bc2, false),
            b"DXT4" | b"DXT5" => (Format::Bc3, false),
            b"ATI1" | b"BC4U" => (Format::Bc4, false),
            b"ATI2" | b"BC5U" => (Format::Bc5, false),
            b"DX10" => {
                if dds.len() < 148 {
                    return Err(bad("truncated header"));
                }
                start = 148;
                if u32_at(dds, 128 + 12) > 1 || u32_at(dds, 128 + 8) & 0x4 != 0 {
                    return Err(Error::Unsupported("DDS texture arrays and cube maps".into()));
                }
                match u32_at(dds, 128) {
                    28 => (Format::Rgba8, false),
                    29 => (Format::Rgba8, true),
                    61 => (Format::L8, false),
                    65 => (Format::A8, false),
                    71 => (Format::Bc1, false),
                    72 => (Format::Bc1, true),
                    74 => (Format::Bc2, false),
                    75 => (Format::Bc2, true),
                    77 => (Format::Bc3, false),
                    78 => (Format::Bc3, true),
                    80 => (Format::Bc4, false),
                    83 => (Format::Bc5, false),
                    86 => (Format::Bgr5a1, false),
                    87 => (Format::Bgra8, false),
                    88 => (Format::Bgrx8, false),
                    91 => (Format::Bgra8, true),
                    95 => (Format::Bc6h, false),
                    98 => (Format::Bc7, false),
                    99 => (Format::Bc7, true),
                    other => return Err(Error::Unsupported(format!("DXGI format {other} in DDS"))),
                }
            }
            other => return Err(Error::Unsupported(format!("DDS format {}", String::from_utf8_lossy(other)))),
        }
    } else {
        let alpha = pf_flags & DDPF_ALPHAPIXELS != 0;
        if (bits, masks) == (24, [0xFF_0000, 0xFF00, 0xFF, 0]) {
            return read_rgb24(dds, width, height, levels);
        }
        let format = match (bits, masks, alpha) {
            (32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000], true) => Format::Bgra8,
            (32, [0xFF_0000, 0xFF00, 0xFF, _], false) => Format::Bgrx8,
            (32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000], true) => Format::Rgba8,
            (16, [0x7C00, 0x3E0, 0x1F, 0x8000], true) => Format::Bgr5a1,
            (8, _, _) if pf_flags & DDPF_ALPHA != 0 => Format::A8,
            (8, [0xFF, ..], _) if pf_flags & DDPF_LUMINANCE != 0 => Format::L8,
            _ => return Err(Error::Unsupported("this uncompressed DDS pixel format".into())),
        };
        (format, false)
    };
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(bad("invalid size"));
    }
    let len: usize = (0..levels).map(|i| format.level_size((width >> i).max(1), (height >> i).max(1))).sum();
    let data = dds.get(start..start + len).ok_or_else(|| bad("data is shorter than its mip levels"))?;
    Ok(Image { format, srgb, width, height, levels, data: data.to_vec() })
}

/// 24-bit RGB has no game equivalent: it is widened to A8R8G8B8.
fn read_rgb24(dds: &[u8], width: u32, height: u32, levels: u32) -> Result<Image, Error> {
    let pixels: usize = (0..levels).map(|i| ((width >> i).max(1) * (height >> i).max(1)) as usize).sum();
    let rgb = dds.get(128..128 + pixels * 3).ok_or_else(|| Error::Format("DDS: data is shorter than its mip levels".into()))?;
    let data = rgb.as_chunks::<3>().0.iter().flat_map(|&[b, g, r]| [b, g, r, 255]).collect();
    Ok(Image { format: Format::Bgra8, srgb: false, width, height, levels, data })
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
