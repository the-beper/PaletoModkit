//! Texture dictionaries (`.ytd`), Legacy (resource version 13) and Enhanced
//! (resource version 5).
//!
//! Layouts follow CodeWalker's `Texture.cs` (MIT).

use crate::Error;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc6h,
    Bc7,
    /// D3DFMT_A8R8G8B8: bytes B, G, R, A.
    Bgra8,
    /// D3DFMT_X8R8G8B8: bytes B, G, R, unused.
    Bgrx8,
    /// D3DFMT_A8B8G8R8: bytes R, G, B, A.
    Rgba8,
    /// D3DFMT_A1R5G5B5.
    Bgr5a1,
    A8,
    L8,
}

impl Format {
    pub const ALL: [Format; 13] = [
        Self::Bc1,
        Self::Bc2,
        Self::Bc3,
        Self::Bc4,
        Self::Bc5,
        Self::Bc6h,
        Self::Bc7,
        Self::Bgra8,
        Self::Bgrx8,
        Self::Rgba8,
        Self::Bgr5a1,
        Self::A8,
        Self::L8,
    ];

    /// Whether pictures can be compressed to this format (all but BC6H, which is for HDR).
    pub fn can_encode(self) -> bool {
        self != Self::Bc6h
    }

    /// Lowercase identifier, as accepted by `from_str`: "bc1", "a8r8g8b8"...
    pub fn id(self) -> &'static str {
        match self {
            Self::Bc1 => "bc1",
            Self::Bc2 => "bc2",
            Self::Bc3 => "bc3",
            Self::Bc4 => "bc4",
            Self::Bc5 => "bc5",
            Self::Bc6h => "bc6h",
            Self::Bc7 => "bc7",
            Self::Bgra8 => "a8r8g8b8",
            Self::Bgrx8 => "x8r8g8b8",
            Self::Rgba8 => "a8b8g8r8",
            Self::Bgr5a1 => "a1r5g5b5",
            Self::A8 => "a8",
            Self::L8 => "l8",
        }
    }

    pub(crate) fn from_legacy(code: u32) -> Option<Self> {
        Some(match code {
            21 => Self::Bgra8,
            22 => Self::Bgrx8,
            25 => Self::Bgr5a1,
            28 => Self::A8,
            32 => Self::Rgba8,
            50 => Self::L8,
            0x3154_5844 => Self::Bc1, // DXT1
            0x3354_5844 => Self::Bc2, // DXT3
            0x3554_5844 => Self::Bc3, // DXT5
            0x3149_5441 => Self::Bc4, // ATI1
            0x3249_5441 => Self::Bc5, // ATI2
            0x2037_4342 => Self::Bc7, // "BC7 "
            _ => return None,
        })
    }

    /// Enhanced edition formats (rage::sga::BufferFormat), with their sRGB flag.
    pub(crate) fn from_enhanced(code: u8) -> Option<(Self, bool)> {
        Some(match code {
            0x1C => (Self::Rgba8, false),
            0x1D => (Self::Rgba8, true),
            0x3D => (Self::L8, false),
            0x41 => (Self::A8, false),
            0x47 => (Self::Bc1, false),
            0x48 => (Self::Bc1, true),
            0x4A => (Self::Bc2, false),
            0x4B => (Self::Bc2, true),
            0x4D => (Self::Bc3, false),
            0x4E => (Self::Bc3, true),
            0x50 => (Self::Bc4, false),
            0x53 => (Self::Bc5, false),
            0x56 => (Self::Bgr5a1, false),
            0x57 => (Self::Bgra8, false),
            0x5B => (Self::Bgra8, true),
            0x5F => (Self::Bc6h, false),
            0x62 => (Self::Bc7, false),
            0x63 => (Self::Bc7, true),
            _ => return None,
        })
    }

    /// Legacy D3DFMT code; BC6H has none.
    pub(crate) fn legacy_code(self) -> Option<u32> {
        Some(match self {
            Self::Bgra8 => 21,
            Self::Bgrx8 => 22,
            Self::Bgr5a1 => 25,
            Self::A8 => 28,
            Self::Rgba8 => 32,
            Self::L8 => 50,
            Self::Bc1 => 0x3154_5844,
            Self::Bc2 => 0x3354_5844,
            Self::Bc3 => 0x3554_5844,
            Self::Bc4 => 0x3149_5441,
            Self::Bc5 => 0x3249_5441,
            Self::Bc7 => 0x2037_4342,
            Self::Bc6h => return None,
        })
    }

    /// Enhanced edition format code; X8R8G8B8 is stored as B8G8R8A8.
    pub(crate) fn enhanced_code(self, srgb: bool) -> u8 {
        match (self, srgb) {
            (Self::Rgba8, false) => 0x1C,
            (Self::Rgba8, true) => 0x1D,
            (Self::L8, _) => 0x3D,
            (Self::A8, _) => 0x41,
            (Self::Bc1, false) => 0x47,
            (Self::Bc1, true) => 0x48,
            (Self::Bc2, false) => 0x4A,
            (Self::Bc2, true) => 0x4B,
            (Self::Bc3, false) => 0x4D,
            (Self::Bc3, true) => 0x4E,
            (Self::Bc4, _) => 0x50,
            (Self::Bc5, _) => 0x53,
            (Self::Bgr5a1, _) => 0x56,
            (Self::Bgra8 | Self::Bgrx8, false) => 0x57,
            (Self::Bgra8 | Self::Bgrx8, true) => 0x5B,
            (Self::Bc6h, _) => 0x5F,
            (Self::Bc7, false) => 0x62,
            (Self::Bc7, true) => 0x63,
        }
    }

    /// The Legacy header's "stride": bytes per row of pixels (a row of
    /// blocks divided by the block height).
    pub(crate) fn stride(self, width: u32) -> u32 {
        let (side, bytes) = self.block();
        width.div_ceil(side).max(1) * bytes as u32 / side
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bc1 => "BC1 (DXT1)",
            Self::Bc2 => "BC2 (DXT3)",
            Self::Bc3 => "BC3 (DXT5)",
            Self::Bc4 => "BC4 (ATI1)",
            Self::Bc5 => "BC5 (ATI2)",
            Self::Bc6h => "BC6H",
            Self::Bc7 => "BC7",
            Self::Bgra8 => "A8R8G8B8",
            Self::Bgrx8 => "X8R8G8B8",
            Self::Rgba8 => "A8B8G8R8",
            Self::Bgr5a1 => "A1R5G5B5",
            Self::A8 => "A8",
            Self::L8 => "L8",
        }
    }

    /// Pixels per block side and bytes per block.
    pub(crate) fn block(self) -> (u32, usize) {
        match self {
            Self::Bc1 | Self::Bc4 => (4, 8),
            Self::Bc2 | Self::Bc3 | Self::Bc5 | Self::Bc6h | Self::Bc7 => (4, 16),
            Self::Bgra8 | Self::Bgrx8 | Self::Rgba8 => (1, 4),
            Self::Bgr5a1 => (1, 2),
            Self::A8 | Self::L8 => (1, 1),
        }
    }

    /// Bytes taken by one mip level.
    pub fn level_size(self, width: u32, height: u32) -> usize {
        let (side, bytes) = self.block();
        (width.div_ceil(side).max(1) * height.div_ceil(side).max(1)) as usize * bytes
    }
}

impl std::str::FromStr for Format {
    type Err = String;

    /// A format by its [`Format::id`], or its DXT/ATI name ("dxt5", "ati2").
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.to_ascii_lowercase();
        let alias = match s.as_str() {
            "dxt1" => Some(Self::Bc1),
            "dxt3" => Some(Self::Bc2),
            "dxt5" => Some(Self::Bc3),
            "ati1" => Some(Self::Bc4),
            "ati2" => Some(Self::Bc5),
            _ => None,
        };
        alias.or_else(|| Self::ALL.into_iter().find(|f| f.id() == s)).ok_or_else(|| {
            let ids: Vec<&str> = Self::ALL.iter().filter(|f| f.can_encode()).map(|f| f.id()).collect();
            format!("unknown texture format {s} (one of {})", ids.join(", "))
        })
    }
}

pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Mip levels whose data is present in `data`.
    pub levels: u32,
    pub format: Format,
    pub srgb: bool,
    /// All mip levels, largest first.
    pub data: Vec<u8>,
}

pub struct TextureDictionary {
    /// Enhanced edition layout.
    pub enhanced: bool,
    pub textures: Vec<Texture>,
}

impl TextureDictionary {
    /// Parses a `.ytd` file as extracted from an archive.
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        let res = Resource::parse(data)?;
        let enhanced = match res.version {
            13 => false,
            5 => true,
            v => return Err(Error::Unsupported(format!("texture dictionary version {v}"))),
        };
        Self::read(&res, 0x5000_0000, enhanced)
    }

    /// A dictionary at an address of a resource (embedded in models too).
    pub(crate) fn read(res: &Resource, ptr: u64, enhanced: bool) -> Result<Self, Error> {
        let root = res.block(ptr, 64)?;
        let (list, count) = (u64_at(root, 0x30), u16_at(root, 0x38) as usize);
        // Placeholder dictionaries have no texture list at all.
        let pointers = if count == 0 { &[][..] } else { res.block(list, count * 8)? };
        let textures = pointers
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| u64::from_le_bytes(*p))
            .filter(|&ptr| ptr != 0)
            .map(|ptr| if enhanced { read_enhanced(res, ptr) } else { read_legacy(res, ptr) })
            .collect::<Result<_, _>>()?;
        Ok(TextureDictionary { enhanced, textures })
    }
}

fn read_legacy(res: &Resource, ptr: u64) -> Result<Texture, Error> {
    let b = res.block(ptr, 0x90)?;
    let code = u32_at(b, 0x58);
    let format = Format::from_legacy(code).ok_or_else(|| Error::Unsupported(format!("texture format {code:#x}")))?;
    let header = Header {
        name: u64_at(b, 0x28),
        width: u16_at(b, 0x50) as u32,
        height: u16_at(b, 0x52) as u32,
        levels: b[0x5D] as u32,
        data: u64_at(b, 0x70),
        data_len: None,
    };
    header.load(res, format, false)
}

fn read_enhanced(res: &Resource, ptr: u64) -> Result<Texture, Error> {
    let b = res.block(ptr, 0x50)?;
    let code = b[0x1F];
    let (format, srgb) =
        Format::from_enhanced(code).ok_or_else(|| Error::Unsupported(format!("texture format {code:#x}")))?;
    let block_count = u32_at(b, 0x08) as usize;
    let block_stride = u32_at(b, 0x0C) as usize;
    let header = Header {
        name: u64_at(b, 0x28),
        width: u16_at(b, 0x18) as u32,
        height: u16_at(b, 0x1A) as u32,
        levels: b[0x22] as u32,
        data: u64_at(b, 0x38),
        data_len: Some(block_count * block_stride).filter(|&n| n > 0),
    };
    header.load(res, format, srgb)
}

struct Header {
    name: u64,
    width: u32,
    height: u32,
    levels: u32,
    data: u64,
    data_len: Option<usize>,
}

impl Header {
    fn load(self, res: &Resource, format: Format, srgb: bool) -> Result<Texture, Error> {
        let name = if self.name == 0 { String::new() } else { res.string(self.name)? };
        let sizes: Vec<usize> = (0..self.levels.max(1))
            .map(|i| format.level_size((self.width >> i).max(1), (self.height >> i).max(1)))
            .collect();
        let wanted = self.data_len.unwrap_or(0).max(sizes.iter().sum());
        let available = res.bytes(self.data, wanted)?;
        // Keep the levels that are fully present.
        let mut len = 0;
        let mut levels = 0;
        for size in sizes {
            if len + size > available.len() {
                break;
            }
            len += size;
            levels += 1;
        }
        if levels == 0 {
            return Err(Error::Format(format!("texture {name}: data is truncated")));
        }
        Ok(Texture { name, width: self.width, height: self.height, levels, format, srgb, data: available[..len].to_vec() })
    }
}

impl Texture {
    /// File name for the texture exported as PNG or DDS. Path separators in
    /// the name become `_`, so the file stays in the folder it is written to.
    pub fn file_name(&self, png: bool) -> String {
        let name = if self.name.is_empty() { "texture" } else { self.name.as_str() };
        format!("{}.{}", name.replace(['/', '\\'], "_"), if png { "png" } else { "dds" })
    }

    /// Data and size of a mip level.
    pub fn level(&self, level: u32) -> Option<(&[u8], u32, u32)> {
        if level >= self.levels {
            return None;
        }
        let dims = |i: u32| ((self.width >> i).max(1), (self.height >> i).max(1));
        let start: usize = (0..level).map(|i| self.format.level_size(dims(i).0, dims(i).1)).sum();
        let (w, h) = dims(level);
        Some((&self.data[start..start + self.format.level_size(w, h)], w, h))
    }

    /// Decodes a mip level to 8-bit RGBA.
    pub fn rgba(&self, level: u32) -> Option<(u32, u32, Vec<u8>)> {
        let (data, w, h) = self.level(level)?;
        Some((w, h, crate::decode::to_rgba(self.format, data, w, h)))
    }

    /// The texture as a DDS file, with all its mip levels.
    pub fn to_dds(&self) -> Vec<u8> {
        crate::dds::write(self)
    }
}
