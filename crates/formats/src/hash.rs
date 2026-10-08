//! Jenkins one-at-a-time hashes, which the game uses for names everywhere.

/// The hash of the exact bytes (metadata field and structure names).
pub fn joaat(data: &[u8]) -> u32 {
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

/// The hash of the lowercase text, as the game hashes asset names and text labels.
pub fn joaat_lower(text: &str) -> u32 {
    joaat(text.to_ascii_lowercase().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hashes() {
        assert_eq!(joaat(b"CMapData"), 3545841574);
        assert_eq!(joaat_lower("ADDER"), 0xB779A091);
    }
}
