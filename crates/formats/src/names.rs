//! Names for hashes, so metadata can be shown and edited as readable XML.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::hash::{joaat, joaat_lower};

/// Built-in names by hash, and the few names that do not hash to their value.
struct Builtin(HashMap<u32, &'static str>, HashMap<&'static str, u32>);

static BUILTIN: LazyLock<Builtin> = LazyLock::new(|| {
    let (mut by_hash, mut odd) = (HashMap::new(), HashMap::new());
    for line in include_str!("../data/meta_names.txt").lines().filter(|l| !l.starts_with('#')) {
        match line.split_once('\t') {
            Some((name, hex)) => {
                let hash = u32::from_str_radix(hex, 16).expect("valid hash in meta_names.txt");
                by_hash.insert(hash, name);
                odd.insert(name, hash);
            }
            None => {
                by_hash.insert(joaat(line.as_bytes()), line);
            }
        }
    }
    Builtin(by_hash, odd)
});

/// Hash ↔ name lookups: the built-in dictionary plus names found in the file
/// being converted (and, for packages, in the package itself).
///
/// `name` always returns text that `hash` turns back into the same value, so
/// converting to XML and back is lossless; unknown hashes print as
/// `hash_XXXXXXXX`.
#[derive(Default, Clone)]
pub struct Names {
    extra: HashMap<u32, String>,
}

impl Names {
    pub fn new() -> Self {
        Self::default()
    }

    /// Learns a string, under the hash of its exact text and of its lowercase form.
    pub fn add(&mut self, text: &str) {
        if text.is_empty() || text.starts_with("hash_") {
            return;
        }
        self.extra.entry(joaat(text.as_bytes())).or_insert_with(|| text.to_string());
        let lower = text.to_ascii_lowercase();
        self.extra.entry(joaat(lower.as_bytes())).or_insert(lower);
    }

    /// Learns every word-like token of a text (an XPath, an XML snippet).
    pub fn add_words(&mut self, text: &str) {
        for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if word.len() > 1 {
                self.add(word);
            }
        }
    }

    pub fn name(&self, hash: u32) -> Cow<'_, str> {
        if hash == 0 {
            return Cow::Borrowed("");
        }
        let known = self.extra.get(&hash).map(String::as_str).or_else(|| BUILTIN.0.get(&hash).copied());
        match known {
            Some(name) if self.hash(name) == hash => Cow::Borrowed(name),
            _ => Cow::Owned(format!("hash_{hash:08X}")),
        }
    }

    /// The name for a hash, or nothing when it is unknown: the [`Self::name`]
    /// fallback of `hash_XXXXXXXX` is not reported as a name.
    pub fn lookup(&self, hash: u32) -> Option<Cow<'_, str>> {
        if hash == 0 {
            return None;
        }
        let known = self.extra.get(&hash).map(String::as_str).or_else(|| BUILTIN.0.get(&hash).copied());
        known.into_iter().find(|name| self.hash(name) == hash).map(Cow::Borrowed)
    }

    /// Every distinct known name for a hash: the dictionaries collide at times, so
    /// several names may claim one value (built-in first, then names learnt from
    /// files or the user names store, sorted case-insensitively; names that differ
    /// only in case are the same word for resolution, the first is kept). Terms as
    /// text are matched in their forms that hash: the exact bytes and the lowercase
    /// form.
    pub fn candidates(&self, hash: u32) -> Vec<Cow<'_, str>> {
        if hash == 0 {
            return Vec::new();
        }
        // The names of both dictionaries: the odd pairs (a name and the hash it
        // points at differ) and every built-in name — the odd map holds the names
        // with their own hash, the first map holds all of them.
        let mut names: Vec<(&str, String)> = BUILTIN.0.values().map(|name| (*name, name.to_lowercase())).collect();
        names.extend(BUILTIN.1.keys().map(|name| (*name, name.to_lowercase())));
        names.extend(self.extra.values().map(|name| (name.as_str(), name.to_lowercase())));
        names.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        names.dedup_by(|a, b| a.1 == b.1);
        names
            .into_iter()
            .filter(|(name, _)| self.hash(name) == hash || joaat_lower(name) == hash)
            .map(|(name, _)| Cow::Borrowed(name))
            .collect()
    }

    pub fn hash(&self, name: &str) -> u32 {
        let name = name.trim();
        if name.is_empty() {
            return 0;
        }
        if let Some(hex) = name.strip_prefix("hash_")
            && let Ok(h) = u32::from_str_radix(hex, 16)
        {
            return h;
        }
        BUILTIN.1.get(name).copied().unwrap_or_else(|| joaat(name.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut names = Names::new();
        assert_eq!(names.name(joaat(b"CMapData")), "CMapData");
        assert_eq!(names.name(0x1234_5678), "hash_12345678");
        assert_eq!(names.hash("hash_12345678"), 0x1234_5678);
        assert_eq!(names.hash(&names.name(0x3518_c7d8)), 0x3518_c7d8);
        names.add("Prop_Tree");
        assert_eq!(names.name(joaat(b"prop_tree")), "prop_tree");
        assert_eq!(names.name(joaat(b"Prop_Tree")), "Prop_Tree");
    }

    // Two names of one hash ("puwaaa"/"thaeaa" collide in joaat_lower; found once by search and pinned here:
    // the built-in dictionary has none, so the synthetic pair stands in).
    #[test]
    fn candidates_report_every_name_that_claims_a_hash() {
        use crate::hash::joaat_lower;
        assert_eq!(joaat_lower("puwaaa"), joaat_lower("thaeaa"));
        let mut names = Names::new();
        names.add("Puwaaa");
        names.add("Thaeaa");
        let hash = joaat_lower("puwaaa");
        let found = names.candidates(hash);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|n| n == "Puwaaa") && found.iter().any(|n| n == "Thaeaa"));
        // Names that differ only in case are one word for resolution: the first is kept.
        names.add("pUwaaa");
        assert_eq!(names.candidates(hash).len(), 2);
    }

    #[test]
    fn lookup_says_unknown_when_no_name_holds_the_hash() {
        let names = Names::new();
        assert!(names.lookup(0x1234_5678).is_none());
        assert!(names.candidates(0x1234_5678).is_empty());
        assert!(names.lookup(0).is_none());
        assert_eq!(names.candidates(joaat(b"CMapData")), ["CMapData"]);
        assert_eq!(names.lookup(joaat(b"CMapData")).map(|n| n.into_owned()), Some("CMapData".into()));
    }
}
