//! Text tables (`.gxt2`): label hashes and UTF-8 strings, as in CodeWalker's
//! `Gxt2File.cs` (MIT).
//!
//! As text, a table is one entry per line, `0xHASH = text`. A line may name
//! the label instead of its hash (`MY_LABEL = text`) to add an entry. Line
//! breaks and backslashes inside a text are written `\n` and `\\`.

use crate::Error;
use crate::hash::joaat_lower;

const MAGIC: &[u8; 4] = b"2TXG";

/// Entries of a `.gxt2` file, as (label hash, text).
pub fn parse(data: &[u8]) -> Result<Vec<(u32, String)>, Error> {
    let bad = |what: &str| Error::Format(format!("GXT2: {what}"));
    if data.len() < 16 || &data[..4] != MAGIC {
        return Err(bad("not a text table"));
    }
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let count = u32_at(4).unwrap() as usize;
    let end_marker = 8 + count * 8;
    if data.get(end_marker..end_marker + 4) != Some(&MAGIC[..]) {
        return Err(bad("truncated table"));
    }
    let end = (u32_at(end_marker + 4).unwrap() as usize).min(data.len());
    (0..count)
        .map(|i| {
            let (hash, offset) = (u32_at(8 + i * 8).unwrap(), u32_at(12 + i * 8).unwrap() as usize);
            let text = data.get(offset..end).ok_or_else(|| bad("text out of range"))?;
            let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())];
            Ok((hash, String::from_utf8_lossy(text).into_owned()))
        })
        .collect()
}

/// Builds a `.gxt2` file; entries are sorted by hash, as the game expects.
pub fn write(entries: &[(u32, String)]) -> Vec<u8> {
    let mut entries: Vec<&(u32, String)> = entries.iter().collect();
    entries.sort_by_key(|(hash, _)| *hash);
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offset = 16 + entries.len() * 8;
    for (hash, text) in &entries {
        out.extend_from_slice(&hash.to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += text.len() + 1;
    }
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    for (_, text) in &entries {
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }
    out
}

/// The entries as editable text, one per line.
pub fn to_text(entries: &[(u32, String)]) -> String {
    let mut out = String::new();
    for (hash, text) in entries {
        let text = text.replace('\\', "\\\\").replace('\r', "\\r").replace('\n', "\\n");
        out.push_str(&format!("0x{hash:08X} = {text}\n"));
    }
    out
}

/// Parses text written by [`to_text`] (or edited) into a `.gxt2` file.
pub fn from_text(text: &str) -> Result<Vec<u8>, Error> {
    let mut entries: Vec<(u32, String)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = |what: &str| Error::Edit(format!("line {}: {what}", n + 1));
        let (label, value) = line.split_once(" = ").or_else(|| line.split_once('=')).ok_or_else(|| bad("expected LABEL = text"))?;
        let label = label.trim();
        let hash = match label.strip_prefix("0x").or_else(|| label.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).map_err(|_| bad("invalid hash"))?,
            None if !label.is_empty() && !label.contains(char::is_whitespace) => joaat_lower(label),
            None => return Err(bad("invalid label")),
        };
        if entries.iter().any(|(h, _)| *h == hash) {
            return Err(bad(&format!("label {label} appears twice")));
        }
        entries.push((hash, unescape(value)));
    }
    Ok(write(&entries))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            ('\\', Some('r')) => {
                out.push('\r');
                chars.next();
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let entries = vec![(0x10, "Hello ~n~world".to_string()), (0x02, "two\nlines \\ ok".to_string())];
        let file = write(&entries);
        let parsed = parse(&file).unwrap();
        assert_eq!(parsed, [(0x02, "two\nlines \\ ok".to_string()), (0x10, "Hello ~n~world".to_string())]);
        assert_eq!(parse(&from_text(&to_text(&parsed)).unwrap()).unwrap(), parsed);

        // Labels by name, and errors with line numbers.
        let added = parse(&from_text("MY_LABEL = Hi\n").unwrap()).unwrap();
        assert_eq!(added, [(joaat_lower("my_label"), "Hi".to_string())]);
        assert_eq!(from_text("0x1 = a\n0x00000001 = b").unwrap_err().to_string(), "line 2: label 0x00000001 appears twice");
        assert!(from_text("no separator").is_err());
    }
}
