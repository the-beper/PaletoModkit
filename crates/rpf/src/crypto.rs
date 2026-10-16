//! RPF decryption: Rockstar's custom "NG" cipher and AES-256-ECB.

use aes::Aes256;
use aes::cipher::{BlockCipherDecrypt, KeyInit};

use crate::keys::{Keys, NG_KEY_COUNT, NgKey, NgTables};

/// Rockstar's Jenkins-like hash, used to choose the NG key for a file name.
pub fn name_hash(lut: &[u8; 256], name: &str) -> u32 {
    let mut result: u32 = 0;
    for &b in name.as_bytes() {
        let temp = (lut[b as usize] as u32).wrapping_add(result).wrapping_mul(1025);
        result = (temp >> 6) ^ temp;
    }
    let r9 = result.wrapping_mul(9);
    ((r9 >> 11) ^ r9).wrapping_mul(32769)
}

/// The NG key for an archive or file, chosen from its name and size.
pub fn ng_key<'a>(keys: &'a Keys, name: &str, length: u32) -> &'a NgKey {
    let hash = name_hash(&keys.lut, name);
    let idx = hash.wrapping_add(length).wrapping_add(NG_KEY_COUNT as u32 - 40) % NG_KEY_COUNT as u32;
    &keys.ng_keys[idx as usize]
}

/// Decrypts whole 16-byte blocks in place; a trailing partial block is left as is.
pub fn decrypt_ng(tables: &NgTables, key: &NgKey, data: &mut [u8]) {
    for b in data.as_chunks_mut::<16>().0 {
        for round in 0..17 {
            let k = &key[round * 4..round * 4 + 4];
            let t = &tables[round];
            *b = if round <= 1 || round == 16 { round_a(b, k, t) } else { round_b(b, k, t) };
        }
    }
}

fn round_a(d: &[u8; 16], k: &[u32], t: &[[u32; 256]; 16]) -> [u8; 16] {
    let x = |i: usize| t[i][d[i] as usize];
    words_to_bytes([
        x(0) ^ x(1) ^ x(2) ^ x(3) ^ k[0],
        x(4) ^ x(5) ^ x(6) ^ x(7) ^ k[1],
        x(8) ^ x(9) ^ x(10) ^ x(11) ^ k[2],
        x(12) ^ x(13) ^ x(14) ^ x(15) ^ k[3],
    ])
}

fn round_b(d: &[u8; 16], k: &[u32], t: &[[u32; 256]; 16]) -> [u8; 16] {
    let x = |i: usize| t[i][d[i] as usize];
    words_to_bytes([
        x(0) ^ x(7) ^ x(10) ^ x(13) ^ k[0],
        x(1) ^ x(4) ^ x(11) ^ x(14) ^ k[1],
        x(2) ^ x(5) ^ x(8) ^ x(15) ^ k[2],
        x(3) ^ x(6) ^ x(9) ^ x(12) ^ k[3],
    ])
}

fn words_to_bytes(w: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (c, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(w) {
        c.copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// AES-256-ECB without padding; a trailing partial block is left as is.
pub fn decrypt_aes(key: &[u8; 32], data: &mut [u8]) {
    let cipher = Aes256::new(&(*key).into());
    for block in data.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(block.into());
    }
}
