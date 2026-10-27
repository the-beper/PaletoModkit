//! Importing pictures: PNG decoding, mipmap generation and compression.

use image::RgbaImage;
use image::imageops::{self, FilterType};

use crate::Error;
use crate::dds::{self, Image};
use crate::ytd::Format;

/// Reads a texture from a DDS or PNG file. PNGs are compressed to `format`,
/// or when it is None to BC1 if opaque and BC3 if not; DDS files are used as
/// they are.
pub fn load(file: &[u8], format: Option<Format>) -> Result<Image, Error> {
    if file.starts_with(b"DDS ") {
        return dds::read(file);
    }
    if !file.starts_with(b"\x89PNG") {
        return Err(Error::Format("expected a DDS or PNG file".into()));
    }
    let rgba = image::load_from_memory_with_format(file, image::ImageFormat::Png)
        .map_err(|e| Error::Format(format!("PNG: {e}")))?
        .to_rgba8();
    let format = format.unwrap_or(if rgba.pixels().all(|p| p[3] == 255) { Format::Bc1 } else { Format::Bc3 });
    compress(&rgba, format)
}

/// Compresses a picture with its mip levels.
pub fn compress(rgba: &RgbaImage, format: Format) -> Result<Image, Error> {
    if !format.can_encode() {
        return Err(Error::Unsupported(format!("compressing to {}", format.name())));
    }
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(Error::Format(format!("invalid picture size {width}x{height}")));
    }
    let levels = mip_levels(width, height);
    let mut data = Vec::new();
    encode_level(rgba, format, &mut data);
    let mut level = rgba.clone();
    for i in 1..levels {
        // Each level from the previous one: a 2x2 average.
        level = imageops::resize(&level, width >> i, height >> i, FilterType::Triangle);
        encode_level(&level, format, &mut data);
    }
    Ok(Image { format, srgb: false, width, height, levels, faces: 1, data })
}

/// Down to 4 pixels on the shorter side, as in the game's files. Sizes that
/// are not powers of two get no mipmaps.
fn mip_levels(width: u32, height: u32) -> u32 {
    let short = width.min(height);
    if !width.is_power_of_two() || !height.is_power_of_two() || short < 4 { 1 } else { (short / 4).ilog2() + 1 }
}

fn encode_level(img: &RgbaImage, format: Format, out: &mut Vec<u8>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut bc = |f: texpresso::Format| {
        let start = out.len();
        out.resize(start + f.compressed_size(w, h), 0);
        f.compress(img.as_raw(), w, h, texpresso::Params::default(), &mut out[start..]);
    };
    match format {
        Format::Bc1 => bc(texpresso::Format::Bc1),
        Format::Bc2 => bc(texpresso::Format::Bc2),
        Format::Bc3 => bc(texpresso::Format::Bc3),
        Format::Bc4 => bc(texpresso::Format::Bc4),
        Format::Bc5 => bc(texpresso::Format::Bc5),
        Format::Rgba8 => out.extend_from_slice(img.as_raw()),
        Format::Bgra8 => out.extend(img.pixels().flat_map(|p| [p[2], p[1], p[0], p[3]])),
        Format::Bgrx8 => out.extend(img.pixels().flat_map(|p| [p[2], p[1], p[0], 255])),
        Format::Bgr5a1 => out.extend(img.pixels().flat_map(|p| {
            let c = |v: u8| (v >> 3) as u16;
            (((p[3] >= 128) as u16) << 15 | c(p[0]) << 10 | c(p[1]) << 5 | c(p[2])).to_le_bytes()
        })),
        Format::A8 => out.extend(img.pixels().map(|p| p[3])),
        Format::L8 => out.extend(img.pixels().map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8)),
        Format::Bc7 => crate::bc7::compress(img.as_raw(), w, h, out),
        Format::Bc6h => unreachable!("checked by compress"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_counts() {
        assert_eq!(mip_levels(1024, 512), 8); // 512 down to 4
        assert_eq!(mip_levels(4, 4), 1);
        assert_eq!(mip_levels(2, 2), 1);
        assert_eq!(mip_levels(300, 200), 1);
    }
}
