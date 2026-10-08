//! BC7 compression with two of its eight modes, keeping the better per block:
//! mode 6 (one RGBA endpoint pair, 7-bit endpoints with a shared bit each,
//! 4-bit indices) handles colour and alpha alike; for opaque blocks, mode 1
//! (two RGB endpoint pairs over one of 64 partitions of the block, 6-bit
//! endpoints, 3-bit indices) handles blocks mixing two unrelated colours.

use rayon::prelude::*;

/// Interpolation weights of 4-bit indices, out of 64.
const WEIGHTS: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Interpolation weights of 3-bit indices (mode 1), out of 64.
const WEIGHTS3: [i32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
/// Partitions tried in full in mode 1, out of the 64.
const MODE1_TRIES: usize = 6;

// The BC7 two-subset partitions (bit i: subset of pixel i) and the second
// subset's anchor pixel, from the BC7 specification (as in texture2ddecoder).
const PARTITIONS: [u16; 64] = [
    0xCCCC, 0x8888, 0xEEEE, 0xECC8, 0xC880, 0xFEEC, 0xFEC8, 0xEC80,
    0xC800, 0xFFEC, 0xFE80, 0xE800, 0xFFE8, 0xFF00, 0xFFF0, 0xF000,
    0xF710, 0x008E, 0x7100, 0x08CE, 0x008C, 0x7310, 0x3100, 0x8CCE,
    0x088C, 0x3110, 0x6666, 0x366C, 0x17E8, 0x0FF0, 0x718E, 0x399C,
    0xAAAA, 0xF0F0, 0x5A5A, 0x33CC, 0x3C3C, 0x55AA, 0x9696, 0xA55A,
    0x73CE, 0x13C8, 0x324C, 0x3BDC, 0x6996, 0xC33C, 0x9966, 0x0660,
    0x0272, 0x04E4, 0x4E40, 0x2720, 0xC936, 0x936C, 0x39C6, 0x639C,
    0x9336, 0x9CC6, 0x817E, 0xE718, 0xCCF0, 0x0FCC, 0x7744, 0xEE22,
];
const ANCHORS: [u8; 64] = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 2, 8, 2, 2, 8, 8, 15, 2, 8, 2, 2, 8, 8, 2, 2,
    15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6,
    6, 2, 6, 8, 15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
];

type Pixels = [[u8; 4]; 16];

/// Appends the BC7 blocks of an RGBA image.
pub fn compress(rgba: &[u8], width: usize, height: usize, out: &mut Vec<u8>) {
    let (bw, bh) = (width.div_ceil(4), height.div_ceil(4));
    let start = out.len();
    out.resize(start + bw * bh * 16, 0);
    out[start..].par_chunks_mut(bw * 16).enumerate().for_each(|(by, row)| {
        for (bx, block) in row.chunks_mut(16).enumerate() {
            let mut px: Pixels = [[0; 4]; 16];
            for (i, p) in px.iter_mut().enumerate() {
                // Pixels past the edge repeat the last row or column.
                let x = (bx * 4 + i % 4).min(width - 1);
                let y = (by * 4 + i / 4).min(height - 1);
                let at = (y * width + x) * 4;
                p.copy_from_slice(&rgba[at..at + 4]);
            }
            block.copy_from_slice(&encode_block(&px));
        }
    });
}

fn encode_block(px: &Pixels) -> [u8; 16] {
    let (error6, block6) = encode_mode6(px);
    if px.iter().all(|p| p[3] == 255)
        && let Some((error1, block1)) = encode_mode1(px)
        && error1 < error6
    {
        return block1;
    }
    block6
}

fn encode_mode6(px: &Pixels) -> (i64, [u8; 16]) {
    let (mut lo, mut hi) = principal_endpoints(px);
    let mut best: Option<(i64, Candidate)> = None;
    // Endpoints along the principal axis, then refitted to the chosen indices.
    for _ in 0..3 {
        let cand = quantize(px, lo, hi);
        let better = best.as_ref().is_none_or(|(err, _)| cand.error < *err);
        let indices = cand.indices;
        if better {
            best = Some((cand.error, cand));
        }
        match refit(px, &indices) {
            Some((a, b)) => (lo, hi) = (a, b),
            None => break,
        }
    }
    let (error, cand) = best.unwrap();
    (error, pack(cand))
}

/// Mode 1: the partitions whose subsets fit a line best, quantized and
/// refitted; None when nothing beats a single pair.
fn encode_mode1(px: &Pixels) -> Option<(i64, [u8; 16])> {
    let subset = |p: usize, s: u16| -> Vec<[u8; 4]> { (0..16).filter(|&i| (PARTITIONS[p] >> i) & 1 == s).map(|i| px[i]).collect() };
    // Rank partitions by how far pixels lie from each subset's line.
    let mut ranked: Vec<(f32, usize)> = (0..64).map(|p| (line_error(&subset(p, 0)) + line_error(&subset(p, 1)), p)).collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(i64, [u8; 16])> = None;
    for &(_, p) in ranked.iter().take(MODE1_TRIES) {
        let fits = [0, 1].map(|s| fit_rgb(&subset(p, s)));
        let error = fits[0].3 + fits[1].3;
        if best.as_ref().is_none_or(|(e, _)| error < *e) {
            best = Some((error, pack_mode1(p, &fits)));
        }
    }
    best
}

/// Squared distance of the pixels (RGB) from their principal line.
fn line_error(px: &[[u8; 4]]) -> f32 {
    if px.len() < 2 {
        return 0.0;
    }
    let (lo, hi) = principal_rgb(px);
    let d: [f32; 3] = std::array::from_fn(|c| hi[c] - lo[c]);
    let len2: f32 = d.iter().map(|v| v * v).sum();
    px.iter()
        .map(|p| {
            let v: [f32; 3] = std::array::from_fn(|c| p[c] as f32 - lo[c]);
            let t = if len2 > 0.0 { (0..3).map(|c| v[c] * d[c]).sum::<f32>() / len2 } else { 0.0 };
            (0..3).map(|c| (v[c] - d[c] * t).powi(2)).sum::<f32>()
        })
        .sum()
}

/// The extremes of RGB pixels along their main direction of variation.
fn principal_rgb(px: &[[u8; 4]]) -> ([f32; 3], [f32; 3]) {
    let mut full: Pixels = [[0, 0, 0, 255]; 16];
    for (i, p) in px.iter().enumerate() {
        full[i] = [p[0], p[1], p[2], 255];
    }
    // Repeat the subset to fill a block: the same line, with the same weight per pixel.
    for i in px.len()..16 {
        full[i] = full[i % px.len().max(1)];
    }
    let (lo, hi) = principal_endpoints(&full);
    ([lo[0], lo[1], lo[2]], [hi[0], hi[1], hi[2]])
}

/// A subset's 6-bit endpoints, shared bit, 3-bit indices and error.
type Fit = ([[u8; 3]; 2], u8, Vec<u8>, i64);

fn fit_rgb(px: &[[u8; 4]]) -> Fit {
    let (mut lo, mut hi) = principal_rgb(px);
    let mut best: Option<Fit> = None;
    for _ in 0..3 {
        let fit = quantize_rgb(px, lo, hi);
        // Least-squares endpoints for these indices.
        let (mut aa, mut ab, mut bb, mut ax, mut bx) = (0f32, 0f32, 0f32, [0f32; 3], [0f32; 3]);
        for (p, &i) in px.iter().zip(&fit.2) {
            let w = WEIGHTS3[i as usize] as f32 / 64.0;
            let (a, b) = (1.0 - w, w);
            (aa, ab, bb) = (aa + a * a, ab + a * b, bb + b * b);
            for c in 0..3 {
                ax[c] += a * p[c] as f32;
                bx[c] += b * p[c] as f32;
            }
        }
        let better = best.as_ref().is_none_or(|b| fit.3 < b.3);
        if better {
            best = Some(fit);
        }
        let det = aa * bb - ab * ab;
        if det.abs() < 1e-6 {
            break;
        }
        lo = std::array::from_fn(|c| ((ax[c] * bb - bx[c] * ab) / det).clamp(0.0, 255.0));
        hi = std::array::from_fn(|c| ((bx[c] * aa - ax[c] * ab) / det).clamp(0.0, 255.0));
    }
    best.unwrap()
}

/// 6-bit endpoint plus shared bit, expanded to 8 bits.
fn expand6(q: u8, p: u8) -> i32 {
    let c7 = ((q as i32) << 1) | p as i32;
    (c7 << 1) | (c7 >> 6)
}

fn quantize_rgb(px: &[[u8; 4]], lo: [f32; 3], hi: [f32; 3]) -> Fit {
    let mut best: Option<Fit> = None;
    for pbit in [0u8, 1] {
        let q = |v: f32| -> u8 {
            // The 6-bit value whose expansion is nearest.
            let guess = ((v / 255.0 * 127.0 - pbit as f32) / 2.0).round().clamp(0.0, 63.0) as i32;
            let candidates = [guess - 1, guess, guess + 1].map(|g| g.clamp(0, 63) as u8);
            candidates.into_iter().min_by_key(|&c| (expand6(c, pbit) - v.round() as i32).abs()).unwrap()
        };
        let ends = [lo.map(q), hi.map(q)];
        let (a, b) = (ends[0].map(|v| expand6(v, pbit)), ends[1].map(|v| expand6(v, pbit)));
        let palette: [[i32; 3]; 8] = std::array::from_fn(|i| std::array::from_fn(|c| (a[c] * (64 - WEIGHTS3[i]) + b[c] * WEIGHTS3[i] + 32) >> 6));
        let mut indices = Vec::with_capacity(px.len());
        let mut error = 0i64;
        for p in px {
            let (i, e) = palette
                .iter()
                .enumerate()
                .map(|(i, q)| (i, (0..3).map(|c| ((p[c] as i32 - q[c]) as i64).pow(2)).sum::<i64>()))
                .min_by_key(|&(_, e)| e)
                .unwrap();
            indices.push(i as u8);
            error += e;
        }
        if best.as_ref().is_none_or(|b| error < b.3) {
            best = Some((ends, pbit, indices, error));
        }
    }
    best.unwrap()
}

fn pack_mode1(partition: usize, fits: &[Fit; 2]) -> [u8; 16] {
    let mut ends = [fits[0].0, fits[1].0];
    let mut indices = [0u8; 16];
    let mut next = [0usize; 2];
    for (i, index) in indices.iter_mut().enumerate() {
        let s = ((PARTITIONS[partition] >> i) & 1) as usize;
        *index = fits[s].2[next[s]];
        next[s] += 1;
    }
    // Each subset's anchor index is stored with 2 bits: its top bit must be 0.
    for (s, anchor) in [(0usize, 0usize), (1, ANCHORS[partition] as usize)] {
        if indices[anchor] >= 4 {
            ends[s].swap(0, 1);
            for (i, index) in indices.iter_mut().enumerate() {
                if ((PARTITIONS[partition] >> i) & 1) as usize == s {
                    *index = 7 - *index;
                }
            }
        }
    }
    let mut bits = 0u128;
    let mut at = 0;
    let mut put = |value: u128, count: u32| {
        bits |= value << at;
        at += count;
    };
    put(0b10, 2); // mode 1
    put(partition as u128, 6);
    for channel in 0..3 {
        for end in [ends[0][0], ends[0][1], ends[1][0], ends[1][1]] {
            put(end[channel] as u128, 6);
        }
    }
    put(fits[0].1 as u128, 1);
    put(fits[1].1 as u128, 1);
    let anchor = ANCHORS[partition] as usize;
    for (i, &index) in indices.iter().enumerate() {
        put(index as u128, if i == 0 || i == anchor { 2 } else { 3 });
    }
    bits.to_le_bytes()
}

/// The extremes of the pixels along their main direction of variation.
fn principal_endpoints(px: &Pixels) -> ([f32; 4], [f32; 4]) {
    let mut mean = [0f32; 4];
    for p in px {
        for c in 0..4 {
            mean[c] += p[c] as f32 / 16.0;
        }
    }
    let mut cov = [[0f32; 4]; 4];
    for p in px {
        let d: [f32; 4] = std::array::from_fn(|c| p[c] as f32 - mean[c]);
        for i in 0..4 {
            for j in 0..4 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    // Power iteration, from the channel with the widest spread.
    let widest = (0..4).max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b])).unwrap();
    let mut axis = [0f32; 4];
    axis[widest] = 1.0;
    for _ in 0..8 {
        let next: [f32; 4] = std::array::from_fn(|i| (0..4).map(|j| cov[i][j] * axis[j]).sum());
        let len = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-6 {
            break;
        }
        axis = next.map(|v| v / len);
    }
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for p in px {
        let t: f32 = (0..4).map(|c| (p[c] as f32 - mean[c]) * axis[c]).sum();
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let at = |t: f32| std::array::from_fn(|c| (mean[c] + axis[c] * t).clamp(0.0, 255.0));
    (at(tmin), at(tmax))
}

struct Candidate {
    /// 7-bit endpoint values and the shared bit of each endpoint.
    ends: [[u8; 4]; 2],
    pbits: [u8; 2],
    indices: [u8; 16],
    error: i64,
}

/// The best quantization of two endpoints, over the four shared-bit choices.
fn quantize(px: &Pixels, lo: [f32; 4], hi: [f32; 4]) -> Candidate {
    let mut best: Option<Candidate> = None;
    for pbits in [[0, 0], [0, 1], [1, 0], [1, 1]] {
        let q = |v: f32, p: u8| (((v - p as f32) / 2.0).round() as i32).clamp(0, 127) as u8;
        let ends = [lo.map(|v| q(v, pbits[0])), hi.map(|v| q(v, pbits[1]))];
        let full = |e: usize| -> [i32; 4] { ends[e].map(|v| ((v as i32) << 1) | pbits[e] as i32) };
        let (a, b) = (full(0), full(1));
        let palette: [[i32; 4]; 16] = std::array::from_fn(|i| std::array::from_fn(|c| (a[c] * (64 - WEIGHTS[i]) + b[c] * WEIGHTS[i] + 32) >> 6));
        let mut indices = [0u8; 16];
        let mut error = 0i64;
        for (n, p) in px.iter().enumerate() {
            let (i, e) = palette
                .iter()
                .enumerate()
                .map(|(i, q)| (i, (0..4).map(|c| ((p[c] as i32 - q[c]) as i64).pow(2)).sum::<i64>()))
                .min_by_key(|&(_, e)| e)
                .unwrap();
            indices[n] = i as u8;
            error += e;
        }
        if best.as_ref().is_none_or(|b| error < b.error) {
            best = Some(Candidate { ends, pbits, indices, error });
        }
    }
    best.unwrap()
}

/// Least-squares endpoints for fixed indices; None when the indices do not
/// determine them (all the same).
fn refit(px: &Pixels, indices: &[u8; 16]) -> Option<([f32; 4], [f32; 4])> {
    let (mut aa, mut ab, mut bb) = (0f32, 0f32, 0f32);
    let (mut ax, mut bx) = ([0f32; 4], [0f32; 4]);
    for (p, &i) in px.iter().zip(indices) {
        let w = WEIGHTS[i as usize] as f32 / 64.0;
        let (a, b) = (1.0 - w, w);
        aa += a * a;
        ab += a * b;
        bb += b * b;
        for c in 0..4 {
            ax[c] += a * p[c] as f32;
            bx[c] += b * p[c] as f32;
        }
    }
    let det = aa * bb - ab * ab;
    if det.abs() < 1e-6 {
        return None;
    }
    let lo = std::array::from_fn(|c| ((ax[c] * bb - bx[c] * ab) / det).clamp(0.0, 255.0));
    let hi = std::array::from_fn(|c| ((bx[c] * aa - ax[c] * ab) / det).clamp(0.0, 255.0));
    Some((lo, hi))
}

fn pack(mut c: Candidate) -> [u8; 16] {
    // The first index is stored with 3 bits: its top bit must be 0.
    if c.indices[0] >= 8 {
        c.ends.swap(0, 1);
        c.pbits.swap(0, 1);
        c.indices = c.indices.map(|i| 15 - i);
    }
    let mut bits = 0u128;
    let mut at = 0;
    let mut put = |value: u128, count: u32| {
        bits |= value << at;
        at += count;
    };
    put(1 << 6, 7); // mode 6
    for channel in 0..4 {
        put(c.ends[0][channel] as u128, 7);
        put(c.ends[1][channel] as u128, 7);
    }
    put(c.pbits[0] as u128, 1);
    put(c.pbits[1] as u128, 1);
    put(c.indices[0] as u128, 3);
    for &i in &c.indices[1..] {
        put(i as u128, 4);
    }
    bits.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_colour_blocks_use_mode_1() {
        // Red and green on the left, blue and white on the right: no single
        // line fits the four colours, two lines do.
        let colours: [[u8; 4]; 4] = [[220, 30, 30, 255], [30, 200, 40, 255], [20, 40, 200, 255], [250, 250, 250, 255]];
        let mut rgba = Vec::new();
        for _ in 0..4 {
            for c in &colours {
                rgba.extend_from_slice(c);
            }
        }
        let mut out = Vec::new();
        compress(&rgba, 4, 4, &mut out);
        assert_eq!(out[0] & 0b11, 0b10, "mode 1");
        let decoded = crate::decode::to_rgba(crate::Format::Bc7, &out, 4, 4);
        for (got, want) in decoded.chunks(4).zip(rgba.chunks(4)) {
            assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(*w) <= 4), "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn round_trip_through_a_decoder() {
        // Gradients with varying alpha, 8x8: four blocks, each along a line in RGBA.
        let mut rgba = Vec::new();
        for y in 0..8u8 {
            for x in 0..8u8 {
                let t = x + y;
                rgba.extend_from_slice(&[t * 15, t * 8, 200 - t * 10, 255 - t * 12]);
            }
        }
        let mut out = Vec::new();
        compress(&rgba, 8, 8, &mut out);
        assert_eq!(out.len(), 4 * 16);
        let decoded = crate::decode::to_rgba(crate::Format::Bc7, &out, 8, 8);
        for (i, (got, want)) in decoded.chunks(4).zip(rgba.chunks(4)).enumerate() {
            assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(*w) <= 6), "pixel {i}: {got:?} vs {want:?}");
        }
    }
}
