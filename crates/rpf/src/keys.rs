//! Cryptographic material for RPF archives.
//!
//! Nothing secret ships with this crate. The AES key is found by scanning the
//! user's game executable for a range whose SHA-1 matches a known hash. Recent
//! executables keep the NG keys and tables obfuscated, so those come from
//! CodeWalker's `magic.dat`, which is encrypted with that AES key. Every value
//! is checked against the hashes in [`crate::hashes`].

use std::io::Read;

use rayon::prelude::*;
use sha1::{Digest, Sha1};

use crate::Error;
use crate::hashes::{AES_KEY_HASH, LUT_HASH, NG_KEY_HASHES, NG_TABLE_HASHES, Sha1 as Hash};

pub const NG_KEY_COUNT: usize = 101;
const NG_KEY_LEN: usize = 0x110;
const NG_TABLE_LEN: usize = 0x400;
const SERIALIZED_LEN: usize = 32 + NG_KEY_COUNT * NG_KEY_LEN + 17 * 16 * NG_TABLE_LEN + 256;
/// The AWC audio key follows the LUT in `magic.dat` and in newer `keys.bin` files.
const AWC_KEY_LEN: usize = 16;

/// Expanded NG key: 17 round keys of 4 words each.
pub type NgKey = [u32; 68];
/// NG decryption tables: 17 rounds x 16 tables x 256 entries.
pub type NgTables = [[[u32; 256]; 16]; 17];

pub struct Keys {
    pub aes: [u8; 32],
    pub ng_keys: Vec<NgKey>,
    pub ng_tables: Box<NgTables>,
    pub lut: [u8; 256],
    /// Key of encrypted audio (`.awc`); missing from older `keys.bin` files.
    pub awc: Option<[u32; 4]>,
}

impl Keys {
    /// Builds the keys from a game executable (GTA5.exe or GTA5_Enhanced.exe)
    /// and CodeWalker's `magic.dat`.
    pub fn from_exe_and_magic(exe: &[u8], magic: &[u8]) -> Result<Self, Error> {
        let aes = find_aes_key(exe).ok_or(Error::KeyNotFound("AES key"))?;
        let mut data = unmask_magic(&aes, magic);
        crate::crypto::decrypt_aes(&aes, &mut data);
        let mut plain = Vec::new();
        flate2::read::DeflateDecoder::new(&data[..]).read_to_end(&mut plain).map_err(|_| Error::InvalidKeys)?;
        // magic.dat layout: NG keys, NG tables, LUT, AWC key (unused for now).
        let body = plain.get(..SERIALIZED_LEN - 32 + AWC_KEY_LEN).ok_or(Error::InvalidKeys)?;
        Self::from_bytes(&[&aes[..], body].concat())
    }

    /// Loads keys previously written by [`Keys::to_bytes`], verifying every hash.
    pub fn from_bytes(data: &[u8]) -> Result<Self, Error> {
        let (data, awc) = match data.len() {
            SERIALIZED_LEN => (data, None),
            n if n == SERIALIZED_LEN + AWC_KEY_LEN => {
                let (data, awc) = data.split_at(SERIALIZED_LEN);
                let mut key = [0u32; 4];
                words(awc, &mut key);
                (data, Some(key))
            }
            _ => return Err(Error::InvalidKeys),
        };
        let (aes, rest) = data.split_at(32);
        let (ng, rest) = rest.split_at(NG_KEY_COUNT * NG_KEY_LEN);
        let (tables, lut) = rest.split_at(17 * 16 * NG_TABLE_LEN);

        let ok = sha1(aes) == AES_KEY_HASH
            && sha1(lut) == LUT_HASH
            && ng.chunks(NG_KEY_LEN).zip(&NG_KEY_HASHES).all(|(c, h)| sha1(c) == *h)
            && tables.chunks(NG_TABLE_LEN).zip(&NG_TABLE_HASHES).all(|(c, h)| sha1(c) == *h);
        if !ok {
            return Err(Error::InvalidKeys);
        }

        let ng_keys = ng
            .chunks(NG_KEY_LEN)
            .map(|c| {
                let mut k = [0u32; 68];
                words(c, &mut k);
                k
            })
            .collect();
        let mut ng_tables: Box<NgTables> = Box::new([[[0; 256]; 16]; 17]);
        for (i, c) in tables.chunks(NG_TABLE_LEN).enumerate() {
            words(c, &mut ng_tables[i / 16][i % 16]);
        }
        Ok(Keys {
            aes: aes.try_into().unwrap(),
            ng_keys,
            ng_tables,
            lut: lut.try_into().unwrap(),
            awc,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SERIALIZED_LEN);
        buf.extend_from_slice(&self.aes);
        for w in self.ng_keys.iter().flatten().chain(self.ng_tables.iter().flatten().flatten()) {
            buf.extend_from_slice(&w.to_le_bytes());
        }
        buf.extend_from_slice(&self.lut);
        for w in self.awc.iter().flatten() {
            buf.extend_from_slice(&w.to_le_bytes());
        }
        buf
    }
}

fn words(bytes: &[u8], out: &mut [u32]) {
    for (w, c) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *w = u32::from_le_bytes(*c);
    }
}

fn sha1(data: &[u8]) -> Hash {
    Sha1::digest(data).into()
}

/// The AES key is stored 8-byte aligned in the executable.
fn find_aes_key(exe: &[u8]) -> Option<[u8; 32]> {
    // Every 8-byte aligned 32-byte window (none in a file shorter than that).
    let windows = (exe.len() + 8).saturating_sub(32) / 8;
    (0..windows)
        .into_par_iter()
        .map(|i| &exe[i * 8..i * 8 + 32])
        .find_any(|c| sha1(c) == AES_KEY_HASH)
        .map(|c| c.try_into().unwrap())
}

/// Removes CodeWalker's masking: four streams from .NET's legacy `System.Random`,
/// seeded with the Jenkins hash of the AES key, added byte by byte.
fn unmask_magic(aes: &[u8; 32], magic: &[u8]) -> Vec<u8> {
    let mut rng = DotNetRandom::new(jenkins(aes) as i32);
    let mut out = magic.to_vec();
    for _ in 0..4 {
        for b in out.iter_mut() {
            *b = b.wrapping_sub(rng.next() as u8);
        }
    }
    out
}

fn jenkins(data: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &b in data {
        h = h.wrapping_add(b as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

/// .NET's seeded `System.Random` (Knuth's subtractive generator).
struct DotNetRandom {
    seeds: [i32; 56],
    next: usize,
    nextp: usize,
}

impl DotNetRandom {
    const MBIG: i32 = i32::MAX;

    fn new(seed: i32) -> Self {
        let mut seeds = [0i32; 56];
        let mut mj = 161803398 - if seed == i32::MIN { i32::MAX } else { seed.abs() };
        seeds[55] = mj;
        let mut mk = 1;
        for i in 1..55 {
            let ii = (21 * i) % 55;
            seeds[ii] = mk;
            mk = mj - mk;
            if mk < 0 {
                mk += Self::MBIG;
            }
            mj = seeds[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                seeds[i] = seeds[i].wrapping_sub(seeds[1 + (i + 30) % 55]);
                if seeds[i] < 0 {
                    seeds[i] = seeds[i].wrapping_add(Self::MBIG);
                }
            }
        }
        DotNetRandom { seeds, next: 0, nextp: 21 }
    }

    fn next(&mut self) -> i32 {
        self.next = if self.next + 1 >= 56 { 1 } else { self.next + 1 };
        self.nextp = if self.nextp + 1 >= 56 { 1 } else { self.nextp + 1 };
        let mut v = self.seeds[self.next].wrapping_sub(self.seeds[self.nextp]);
        if v == Self::MBIG {
            v -= 1;
        }
        if v < 0 {
            v = v.wrapping_add(Self::MBIG);
        }
        self.seeds[self.next] = v;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_in_short_files() {
        assert_eq!(find_aes_key(&[]), None);
        assert_eq!(find_aes_key(&[0; 31]), None);
        assert_eq!(find_aes_key(&[0; 40]), None);
    }
}
