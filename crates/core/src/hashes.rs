//! The hashes of names, as the app and the CLI print and read them: the modes
//! the game uses and the textual forms of a value. Resolution is dictionary
//! based ([`paleto_formats::names`]): a hash is never inverted, it is only
//! looked up.

use std::fmt;
use std::str::FromStr;

use paleto_formats::hash::{joaat, joaat_lower};

/// The 29 bits an `.awc` container keeps of the hash of each stream's name
/// (`crates/formats/src/awc.rs`): the channel count rides in the top bits.
const AWC_MASK: u32 = (1 << 29) - 1;

/// How a name is turned into a hash. The game hashes asset names and text
/// labels lowercase, metadata field and structure names byte by byte, and keeps
/// the low 29 bits of the hash of an audio stream's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// `joaat_lower` ("ADDER" as "adder").
    #[default]
    Lower,
    /// `joaat` of the exact bytes (the case matters: "CMapData").
    Exact,
    /// The 29-bit id an `.awc` names its sounds with.
    Awc,
}

impl Mode {
    /// The modes of the tool, the default one first.
    pub const ALL: [Mode; 3] = [Mode::Lower, Mode::Exact, Mode::Awc];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Lower => "lower",
            Mode::Exact => "exact",
            Mode::Awc => "awc",
        }
    }

    /// The hash of a text, in this mode.
    pub fn hash(self, name: &str) -> u32 {
        match self {
            Mode::Lower => joaat_lower(name),
            Mode::Exact => joaat(name.as_bytes()),
            Mode::Awc => joaat_lower(name) & AWC_MASK,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "lower" | "lowercase" => Ok(Mode::Lower),
            "exact" => Ok(Mode::Exact),
            "awc" => Ok(Mode::Awc),
            _ => Err("expected lower, exact or awc"),
        }
    }
}

/// A value as hexadecimal, the form metadata and the game's text show.
pub fn as_hex(hash: u32) -> String {
    format!("0x{hash:08X}")
}

/// A value as unsigned decimal.
pub fn as_unsigned(hash: u32) -> String {
    format!("{hash}")
}

/// A value as signed decimal (the 32 bits read the other way).
pub fn as_signed(hash: u32) -> String {
    format!("{}", hash as i32)
}

/// A value in its three forms, as the tool prints it: hexadecimal, unsigned decimal, signed decimal.
pub fn value_line(hash: u32) -> String {
    format!("{}  {}  {}", as_hex(hash), as_unsigned(hash), as_signed(hash))
}

/// Parses a hash as text: hexadecimal with `0x` or `hash_` in front (as metadata
/// prints it), a bare sequence of at most eight hex digits (a hex letter tells it
/// from the decimal form), an unsigned or a signed decimal form, all read
/// modulo 2³² (`-1` is `0xFFFFFFFF`). Nothing for text that is none of these
/// forms.
pub fn parse_value(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(hex) = hex_text(text) {
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return u32::from_str_radix(hex, 16).ok();
    }
    let (minus, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) || digits.len() > 20 {
        return None;
    }
    let value: u128 = digits.parse().ok()?;
    let wrapped = (value % (1 << 32)) as u32;
    Some(if minus { wrapped.wrapping_neg() } else { wrapped })
}

/// The hexadecimal part of a value's text (None: the text names no hex form).
fn hex_text(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return Some(rest);
    }
    if let (Some(rest), Some(prefix)) = (text.get(5..), text.get(..5))
        && prefix.eq_ignore_ascii_case("hash_")
    {
        return Some(rest);
    }
    if text.chars().all(|c| c.is_ascii_hexdigit()) && text.chars().any(|c| c.is_ascii_alphabetic()) && text.chars().count() <= 8 {
        return Some(text);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use paleto_formats::hash::{joaat, joaat_lower};

    #[test]
    fn known_vectors_per_mode() {
        assert_eq!(Mode::Exact.hash("CMapData"), 3545841574);
        assert_eq!(Mode::Lower.hash("ADDER"), 0xB779A091);
        assert_eq!(Mode::Lower.hash("adder"), 0xB779A091);
        assert_eq!(Mode::Exact.hash("adder"), 0xB779A091, "already lowercase bytes hash the same way");
        assert_ne!(Mode::Exact.hash("ADDER"), 0xB779A091, "the case matters in the exact mode");
        assert_eq!(Mode::Awc.hash("ADDER"), 0x1779A091);
        assert_eq!(Mode::Awc.hash("ADDER"), Mode::Lower.hash("ADDER") & AWC_MASK);
        assert_eq!(Mode::Exact.hash("CMapData"), joaat(b"CMapData"));
        assert_mode_names_ok();
    }

    // The names of the modes round-trip through their text form (the CLI reads --mode).
    fn assert_mode_names_ok() {
        for mode in Mode::ALL {
            assert_eq!(mode.as_str().parse::<Mode>(), Ok(mode));
        }
        assert!("box".parse::<Mode>().is_err());
    }

    #[test]
    fn an_empty_string_hashes_to_zero() {
        assert_eq!(joaat(b""), 0);
        assert_eq!(joaat_lower(""), 0);
        for mode in Mode::ALL {
            assert_eq!(mode.hash(""), 0);
        }
    }

    #[test]
    fn non_ascii_bytes_are_hashed_and_only_ascii_is_lowercased() {
        assert_eq!(Mode::Lower.hash("Ådder"), Mode::Exact.hash("Ådder"));
        assert_eq!(Mode::Lower.hash("Ådder"), joaat("Ådder".as_bytes()));
        assert_ne!(Mode::Lower.hash("Ådder"), Mode::Lower.hash("ådder"));
    }

    #[test]
    fn a_value_is_read_in_every_textual_form() {
        for text in ["0xB779A091", "0xb779a091", "0XB779A091", "B779A091", "b779a091", "hash_B779A091", "hash_b779a091", "HASH_B779A091"] {
            assert_eq!(parse_value(text), Some(0xB779A091), "{text}");
        }
        assert_eq!(parse_value("3078201489"), Some(0xB779A091));
        assert_eq!(parse_value("-1216765807"), Some(0xB779A091));
        assert_eq!(parse_value("3078201489"), Some(3078201489));
        assert_eq!(parse_value("-3078201489"), Some((-3078201489i64 as u128 & 0xFFFF_FFFF) as u32));
        assert_eq!(parse_value("4294967295"), Some(u32::MAX));
        assert_eq!(parse_value("-1"), Some(u32::MAX));
        assert_eq!(parse_value("4294967296"), Some(0));
        assert_eq!(parse_value("-4294967295"), Some(1));
        assert_eq!(parse_value("0"), Some(0));
        assert_eq!(parse_value("-0"), Some(0));
        assert_eq!(parse_value("123456789"), Some(123456789), "a decimal longer than 8 digits is not a hex form");
    }

    #[test]
    fn text_that_is_not_a_value_is_not_read_as_one() {
        for text in ["", " ", "adder", "ADDER!", "0x", "hash_", "0x100000000", "feedaher", "-", "- 12", "1 2", "12+3", "h", "  adder  "] {
            assert_eq!(parse_value(text), None, "{text:?}");
        }
        assert_eq!(parse_value(" adder "), None, "names are not values");
        assert_eq!(parse_value(" FEED A LION "), None);
    }
}
