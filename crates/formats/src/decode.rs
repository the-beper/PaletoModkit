//! Texture data to 8-bit RGBA, for previews and PNG export.

use texture2ddecoder as t2d;

use crate::ytd::Format;

type BlockDecoder = fn(&[u8], &mut [u32]);

pub fn to_rgba(format: Format, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let block: Option<(usize, BlockDecoder)> = match format {
        Format::Bc1 => Some((8, t2d::decode_bc1a_block)),
        Format::Bc2 => Some((16, t2d::decode_bc2_block)),
        Format::Bc3 => Some((16, t2d::decode_bc3_block)),
        Format::Bc4 => Some((8, t2d::decode_bc4_block)),
        Format::Bc5 => Some((16, t2d::decode_bc5_block)),
        Format::Bc6h => Some((16, t2d::decode_bc6_block_unsigned)),
        Format::Bc7 => Some((16, t2d::decode_bc7_block)),
        _ => None,
    };
    if let Some((size, decode)) = block {
        let blocks_x = w.div_ceil(4);
        let mut pixels = [0u32; 16];
        for (i, chunk) in data.chunks_exact(size).enumerate() {
            pixels.fill(0xFF00_0000);
            decode(chunk, &mut pixels);
            let (bx, by) = (i % blocks_x * 4, i / blocks_x * 4);
            for (p, &px) in pixels.iter().enumerate() {
                let (x, y) = (bx + p % 4, by + p / 4);
                if x < w && y < h {
                    // The decoder writes B, G, R, A.
                    let [b, g, r, a] = px.to_le_bytes();
                    out[(y * w + x) * 4..][..4].copy_from_slice(&[r, g, b, a]);
                }
            }
        }
        match format {
            // Single channel: show it as grey.
            Format::Bc4 => out.as_chunks_mut::<4>().0.iter_mut().for_each(|p| (p[1], p[2]) = (p[0], p[0])),
            // Normal maps: rebuild Z so they look like the usual blue maps.
            Format::Bc5 => out.as_chunks_mut::<4>().0.iter_mut().for_each(|p| {
                let (x, y) = (p[0] as f32 / 127.5 - 1.0, p[1] as f32 / 127.5 - 1.0);
                p[2] = (((1.0 - x * x - y * y).max(0.0).sqrt() + 1.0) * 127.5) as u8;
            }),
            _ => {}
        }
        return out;
    }
    for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let rgba = match format {
            Format::Bgra8 => data.get(i * 4..i * 4 + 4).map(|s| [s[2], s[1], s[0], s[3]]),
            Format::Bgrx8 => data.get(i * 4..i * 4 + 4).map(|s| [s[2], s[1], s[0], 255]),
            Format::Rgba8 => data.get(i * 4..i * 4 + 4).map(|s| [s[0], s[1], s[2], s[3]]),
            Format::Bgr5a1 => data.get(i * 2..i * 2 + 2).map(|s| {
                let v = u16::from_le_bytes([s[0], s[1]]);
                let c = |shift: u16| (((v >> shift) & 0x1F) as u32 * 255 / 31) as u8;
                [c(10), c(5), c(0), if v & 0x8000 != 0 { 255 } else { 0 }]
            }),
            Format::A8 => data.get(i).map(|&a| [255, 255, 255, a]),
            Format::L8 => data.get(i).map(|&l| [l, l, l, 255]),
            _ => unreachable!(),
        };
        *px = rgba.unwrap_or([0; 4]);
    }
    out
}
