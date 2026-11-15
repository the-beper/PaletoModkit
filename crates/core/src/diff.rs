//! Two versions of a file compared: a mods copy and the game's original, or any two (a package's file before and after
//! an install, which exists only in memory). Text line by line; XML, and binary metadata shown as XML, once the
//! formatting is set aside; anything else byte by byte. Read-only.

use paleto_formats::MetaFile;
use paleto_rpf::Keys;
use sha2::{Digest, Sha256};

use crate::vfs::ArchivePath;
use crate::Error;

/// The most of one side that is read whole; above it, files are compared by size and stored bytes only.
pub const MAX_SIDE: u64 = 256 << 20;

/// Lines are aligned when the files differ in at most this many lines (between their common start and end); past it the
/// part that differs is shown whole, removed then added.
const MAX_EDITS: usize = 2000;

/// The differing stretches of bytes listed at most.
const MAX_RUNS: usize = 64;

/// How two files are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Xml,
    /// PSO, resource metadata or RBF, compared as the XML the app shows for them.
    Meta,
    Binary,
}

impl Kind {
    /// How a file is compared, from its name and its bytes. A `.ycd` is binary: its preview is a summary, not the file.
    pub fn of(name: &str, bytes: &[u8]) -> Kind {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".ycd") {
            Kind::Binary
        } else if MetaFile::is_meta(bytes) {
            Kind::Meta
        } else if let Some(text) = as_text(bytes) {
            if text.trim_start().starts_with('<') { Kind::Xml } else { Kind::Text }
        } else {
            Kind::Binary
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Xml => "XML",
            Kind::Meta => "metadata",
            Kind::Binary => "binary",
        }
    }
}

/// The outcome of a comparison, and how it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub kind: Kind,
    pub outcome: Outcome,
    /// What to know about how the files were compared: a side that is not valid XML, metadata that could not be read.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The same bytes.
    Identical,
    /// Only the modified side exists: a file the mods add (an add-on pack's, say).
    Added,
    /// Only the original exists.
    Deleted,
    /// Line by line: the text, or the XML once its formatting is set aside. `same_content`: no line differs, though the
    /// bytes do (the formatting, the line endings or, for metadata, the encoding); `lines` is empty then.
    Lines { lines: Vec<Line>, same_content: bool },
    Bytes(ByteDiff),
    /// A side over [`MAX_SIDE`]: compared by size, and by stored bytes when both are small enough (`None` if not).
    Large { sizes: [u64; 2], same_stored: Option<bool> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Same,
    Removed,
    Added,
}

/// A line of a line-by-line comparison, with its number in the original and in the modified file (from 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub op: Op,
    pub text: String,
    pub old: Option<usize>,
    pub new: Option<usize>,
}

/// Two binary files: their sizes, SHA-256 and where they differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteDiff {
    pub sizes: [u64; 2],
    pub sha256: [String; 2],
    /// The stretches that differ (offset, length), in order; past the shorter file, the rest of the longer is one.
    pub runs: Vec<(u64, u64)>,
    /// More stretches differ than are listed.
    pub more: bool,
}

/// Compares the `original` and the `modified` version of the file `name` (either may be missing).
pub fn compare(name: &str, original: Option<&[u8]>, modified: Option<&[u8]>) -> Comparison {
    let (a, b) = match (original, modified) {
        (Some(a), Some(b)) => (a, b),
        (None, Some(b)) => return Comparison { kind: Kind::of(name, b), outcome: Outcome::Added, notes: Vec::new() },
        (Some(a), None) => return Comparison { kind: Kind::of(name, a), outcome: Outcome::Deleted, notes: Vec::new() },
        (None, None) => return Comparison { kind: Kind::Binary, outcome: Outcome::Identical, notes: Vec::new() },
    };
    let (ka, kb) = (Kind::of(name, a), Kind::of(name, b));
    let kind = if ka == kb { ka } else { Kind::Binary };
    let mut notes = Vec::new();
    if ka != kb {
        notes.push(format!("The original reads as {} and the modified file as {}: compared as bytes", ka.label(), kb.label()));
    }
    if a == b {
        return Comparison { kind, outcome: Outcome::Identical, notes };
    }
    let outcome = match kind {
        Kind::Text => text_lines(as_text(a).unwrap_or_default(), as_text(b).unwrap_or_default(), &mut notes),
        Kind::Xml => xml_lines(as_text(a).unwrap_or_default(), as_text(b).unwrap_or_default(), &mut notes),
        Kind::Meta => match (MetaFile::parse(a), MetaFile::parse(b)) {
            (Ok(mut ma), Ok(mut mb)) => {
                // Names are learned per file: what one side names the other may know only by its hash.
                let (xa, xb) = (ma.to_xml(), mb.to_xml());
                ma.learn_names(&xb);
                mb.learn_names(&xa);
                xml_lines(&ma.to_xml(), &mb.to_xml(), &mut notes)
            }
            (ra, rb) => {
                for (side, r) in [("original", ra), ("modified file", rb)] {
                    if let Err(e) = r {
                        notes.push(format!("The {side} could not be read as metadata ({e}): compared as bytes"));
                    }
                }
                Outcome::Bytes(bytes(a, b))
            }
        },
        Kind::Binary => Outcome::Bytes(bytes(a, b)),
    };
    let kind = if matches!(outcome, Outcome::Bytes(_)) { Kind::Binary } else { kind };
    Comparison { kind, outcome, notes }
}

/// One side of a comparison as read from an archive or a disk.
pub enum Side {
    Whole(Vec<u8>),
    /// Over [`MAX_SIDE`]: its size, and its stored bytes if they are not over it.
    Large { size: u64, stored: Option<Vec<u8>> },
}

impl Side {
    /// Its size, and the bytes it can be compared by (the stored ones of a large file, if read).
    fn sized(&self) -> (u64, Option<&[u8]>) {
        match self {
            Side::Whole(data) => (data.len() as u64, Some(data.as_slice())),
            Side::Large { size, stored } => (*size, stored.as_deref()),
        }
    }
}

/// A comparison of two files on disk or in archives, with their bytes when it is a byte comparison (for a byte view).
pub struct Compared {
    pub comparison: Comparison,
    pub original: Option<Vec<u8>>,
    pub modified: Option<Vec<u8>>,
}

/// Compares the file at `modified` with the one at `original` (`None`, or a path where nothing is: the game has none).
pub fn compare_files(original: Option<&ArchivePath>, modified: &ArchivePath, keys: Option<&Keys>) -> Result<Compared, Error> {
    compare_files_within(original, modified, keys, MAX_SIDE)
}

/// [`compare_files`] with another limit than [`MAX_SIDE`] (tests use small ones).
pub(crate) fn compare_files_within(original: Option<&ArchivePath>, modified: &ArchivePath, keys: Option<&Keys>, cap: u64) -> Result<Compared, Error> {
    let name = modified.inner.last().map_or_else(|| modified.disk.file_name().unwrap_or_default().to_string_lossy().into_owned(), String::clone);
    let a = original.map(|p| read_within(p, keys, cap)).transpose()?.flatten();
    let b = read_within(modified, keys, cap)?.ok_or_else(|| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{}: not found", modified.disk.display()))))?;
    match (a, b) {
        (Some(Side::Whole(a)), Side::Whole(b)) => {
            // The bytes are kept only for a byte view: a side may be 256 MiB.
            let comparison = compare(&name, Some(&a), Some(&b));
            let bytes = matches!(comparison.outcome, Outcome::Bytes(_));
            Ok(Compared { comparison, original: bytes.then_some(a), modified: bytes.then_some(b) })
        }
        (None, Side::Whole(b)) => Ok(Compared { comparison: compare(&name, None, Some(&b)), original: None, modified: None }),
        (None, Side::Large { .. }) => Ok(Compared { comparison: Comparison { kind: Kind::Binary, outcome: Outcome::Added, notes: Vec::new() }, original: None, modified: None }),
        (Some(a), b) => {
            let ((sa, da), (sb, db)) = (a.sized(), b.sized());
            let same_stored = da.zip(db).map(|(x, y)| x == y);
            let note = format!("Over {}: compared by size and stored bytes only", crate::budget::size(cap));
            Ok(Compared { comparison: Comparison { kind: Kind::Binary, outcome: Outcome::Large { sizes: [sa, sb], same_stored }, notes: vec![note] }, original: None, modified: None })
        }
    }
}

/// What there is at `path`: `None` if nothing (the game has no such file).
pub fn read_side(path: &ArchivePath, keys: Option<&Keys>) -> Result<Option<Side>, Error> {
    read_within(path, keys, MAX_SIDE)
}

fn read_within(path: &ArchivePath, keys: Option<&Keys>, cap: u64) -> Result<Option<Side>, Error> {
    let missing = |e: &Error| matches!(e, Error::Rpf(paleto_rpf::Error::NotFound(_))) || matches!(e, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound);
    if path.inner.is_empty() {
        let size = match std::fs::metadata(&path.disk) {
            Ok(m) if m.is_file() => m.len(),
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        return Ok(Some(if size > cap { Side::Large { size, stored: None } } else { Side::Whole(std::fs::read(&path.disk)?) }));
    }
    if !path.disk.is_file() {
        return Ok(None);
    }
    let Some(parent) = path.parent() else { return Ok(None) };
    let (archive, dir) = match parent.open(keys) {
        Ok(found) => found,
        Err(e) if missing(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let name = path.inner.last().map(String::as_str).unwrap_or_default();
    let Some(idx) = archive.children(dir).find(|&i| archive.entries[i].name.eq_ignore_ascii_case(name)) else { return Ok(None) };
    let entry = &archive.entries[idx];
    if entry.size() <= cap {
        return Ok(Some(Side::Whole(archive.read(idx, keys)?)));
    }
    let stored = if entry.stored_len() <= cap { Some(archive.read_stored(idx)?) } else { None };
    Ok(Some(Side::Large { size: entry.size(), stored }))
}

/// The stretches of `lines` worth showing: each change with `context` unchanged lines around it, joined when they
/// touch. What lies between them is unchanged.
pub fn hunks(lines: &[Line], context: usize) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, _) in lines.iter().enumerate().filter(|(_, l)| l.op != Op::Same) {
        let (start, end) = (i.saturating_sub(context), (i + context + 1).min(lines.len()));
        match out.last_mut() {
            Some(last) if start <= last.end => last.end = last.end.max(end),
            _ => out.push(start..end),
        }
    }
    out
}

/// Text as the preview reads it: UTF-8 (a byte order mark left out) without NUL bytes.
fn as_text(bytes: &[u8]) -> Option<&str> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().filter(|t| !t.contains('\0'))
}

fn text_lines(a: &str, b: &str, notes: &mut Vec<String>) -> Outcome {
    // A final newline ends the last line, it does not start another; a difference only there is a line ending's.
    let split = |t: &str| -> Vec<String> { t.lines().map(String::from).collect() };
    let lines = diff_lines(&split(a), &split(b), notes);
    let same_content = lines.iter().all(|l| l.op == Op::Same);
    if same_content {
        notes.push("Only the line endings, the final newline or a byte order mark differ".into());
    }
    Outcome::Lines { lines: if same_content { Vec::new() } else { lines }, same_content }
}

fn xml_lines(a: &str, b: &str, notes: &mut Vec<String>) -> Outcome {
    match (canonical_xml(a), canonical_xml(b)) {
        (Ok(ca), Ok(cb)) if ca == cb => Outcome::Lines { lines: Vec::new(), same_content: true },
        (Ok(ca), Ok(cb)) => {
            notes.push("Line numbers are those of the XML as compared, its formatting set aside, not of the file".into());
            Outcome::Lines { lines: diff_lines(&ca, &cb, notes), same_content: false }
        }
        (ra, rb) => {
            for (side, r) in [("original", ra), ("modified file", rb)] {
                if let Err(e) = r {
                    notes.push(format!("The {side} could not be read as XML ({e}): compared as text"));
                }
            }
            text_lines(a, b, notes)
        }
    }
}

/// XML as lines that differ only where the document does: one element, text, comment or processing instruction a line,
/// indented by depth, attributes in name order. Only the indentation (whitespace-only text beside child elements, outside
/// `xml:space="preserve"`) and the declaration are left out: text in a leaf element or among other text is kept as it is.
/// A DTD is refused (no entity is expanded): such a file is compared as text.
fn canonical_xml(text: &str) -> Result<Vec<String>, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for child in doc.root().children().filter(|c| !is_blank(*c)) {
        canonical_node(child, 0, &mut out);
    }
    Ok(out)
}

fn is_blank(node: roxmltree::Node) -> bool {
    node.is_text() && node.text().unwrap_or_default().trim().is_empty()
}

/// The children of `element` that are not its indentation.
fn content<'a, 'i>(element: roxmltree::Node<'a, 'i>) -> Vec<roxmltree::Node<'a, 'i>> {
    let preserve = element.ancestors().find_map(|n| n.attribute((roxmltree::NS_XML_URI, "space"))) == Some("preserve");
    let elements = element.children().any(|c| c.is_element());
    let text = element.children().any(|c| c.is_text() && !is_blank(c));
    let indentation = elements && !text && !preserve;
    element.children().filter(|c| (c.is_element() || c.is_comment() || c.is_pi() || c.is_text()) && !(indentation && is_blank(*c))).collect()
}

fn canonical_node(node: roxmltree::Node, depth: usize, out: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    if node.is_comment() {
        out.push(format!("{pad}<!--{}-->", node.text().unwrap_or_default()));
        return;
    }
    if let Some(pi) = node.pi() {
        out.push(format!("{pad}<?{} {}?>", pi.target, pi.value.unwrap_or_default()));
        return;
    }
    if node.is_text() {
        out.push(format!("{pad}{}", escape(node.text().unwrap_or_default())));
        return;
    }
    if !node.is_element() {
        return;
    }
    let name = qualified(node);
    let mut attrs: Vec<(String, &str)> = node.attributes().map(|a| (a.namespace().map_or_else(|| a.name().to_string(), |ns| format!("{{{ns}}}{}", a.name())), a.value())).collect();
    attrs.sort();
    let attrs: String = attrs.iter().map(|(k, v)| format!(" {k}=\"{}\"", escape(v))).collect();
    let children = content(node);
    match children.as_slice() {
        [] => out.push(format!("{pad}<{name}{attrs} />")),
        [only] if only.is_text() => out.push(format!("{pad}<{name}{attrs}>{}</{name}>", escape(only.text().unwrap_or_default()))),
        _ => {
            out.push(format!("{pad}<{name}{attrs}>"));
            for child in children {
                canonical_node(child, depth + 1, out);
            }
            out.push(format!("{pad}</{name}>"));
        }
    }
}

fn qualified(node: roxmltree::Node) -> String {
    let tag = node.tag_name();
    tag.namespace().map_or_else(|| tag.name().to_string(), |ns| format!("{{{ns}}}{}", tag.name()))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\n', "&#10;").replace('\r', "&#13;")
}

/// The lines of `a` and `b`, aligned; when they differ in too many to align, the part between their common start and end
/// is shown removed then added, and a note says so.
fn diff_lines(a: &[String], b: &[String], notes: &mut Vec<String>) -> Vec<Line> {
    let start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let end = a[start..].iter().rev().zip(b[start..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (ma, mb) = (&a[start..a.len() - end], &b[start..b.len() - end]);
    let middle = myers(ma, mb, MAX_EDITS).unwrap_or_else(|| {
        notes.push(format!("The files differ in more than {MAX_EDITS} lines: the part that differs is shown whole"));
        ma.iter().map(|_| Op::Removed).chain(mb.iter().map(|_| Op::Added)).collect()
    });
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let (mut i, mut j) = (0, 0);
    let mut push = |op: Op, i: &mut usize, j: &mut usize| {
        let (old, new) = (matches!(op, Op::Same | Op::Removed).then_some(*i + 1), matches!(op, Op::Same | Op::Added).then_some(*j + 1));
        let text = if op == Op::Added { b[*j].clone() } else { a[*i].clone() };
        out.push(Line { op, text, old, new });
        if op != Op::Added {
            *i += 1;
        }
        if op != Op::Removed {
            *j += 1;
        }
    };
    for _ in 0..start {
        push(Op::Same, &mut i, &mut j);
    }
    for op in middle {
        push(op, &mut i, &mut j);
    }
    for _ in 0..end {
        push(Op::Same, &mut i, &mut j);
    }
    out
}

/// The shortest edit script from `a` to `b` (Myers' algorithm), or `None` if it takes more than `max` edits. Each
/// step of the search keeps only the diagonals it can reach, so memory grows with the square of the edits, not with
/// the length of the files.
fn myers<T: PartialEq>(a: &[T], b: &[T], max: usize) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = max.min(a.len() + b.len()) as isize;
    // v[k + offset]: the furthest x on diagonal k; trace[d]: v over -d..=d before step d.
    let offset = max + 1;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=max {
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let at = |k: isize| (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
        }
    }
    let depth = found?;
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=depth).rev() {
        let saved = &trace[d as usize];
        let at = |k: isize| saved[(k + d) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) { k + 1 } else { k - 1 };
        let (prev_x, prev_y) = if d == 0 { (0, 0) } else { (at(prev_k), at(prev_k) - prev_k) };
        while x > prev_x && y > prev_y {
            ops.push(Op::Same);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Added } else { Op::Removed });
        }
        (x, y) = (prev_x, prev_y);
    }
    ops.reverse();
    Some(ops)
}

fn bytes(a: &[u8], b: &[u8]) -> ByteDiff {
    let sha = |d: &[u8]| Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut runs = Vec::new();
    let mut more = false;
    let common = a.len().min(b.len());
    let mut i = 0;
    while i < common {
        if a[i] == b[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < common && a[i] != b[i] {
            i += 1;
        }
        if runs.len() == MAX_RUNS {
            more = true;
            break;
        }
        runs.push((start as u64, (i - start) as u64));
    }
    if a.len() != b.len() {
        if runs.len() < MAX_RUNS {
            runs.push((common as u64, (a.len().max(b.len()) - common) as u64));
        } else {
            more = true;
        }
    }
    ByteDiff { sizes: [a.len() as u64, b.len() as u64], sha256: [sha(a), sha(b)], runs, more }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(outcome: &Outcome) -> Vec<String> {
        let Outcome::Lines { lines, .. } = outcome else { panic!("{outcome:?}") };
        lines.iter().filter(|l| l.op != Op::Same).map(|l| format!("{}{}", if l.op == Op::Added { '+' } else { '-' }, l.text)).collect()
    }

    #[test]
    fn myers_finds_the_shortest_script() {
        let a: Vec<char> = "ABCABBA".chars().collect();
        let b: Vec<char> = "CBABAC".chars().collect();
        let ops = myers(&a, &b, 100).unwrap();
        assert_eq!(ops.iter().filter(|o| **o != Op::Same).count(), 5);
        // Applying the script to a gives b.
        let (mut i, mut j, mut out) = (0, 0, Vec::new());
        for op in &ops {
            match op {
                Op::Same => {
                    assert_eq!(a[i], b[j]);
                    out.push(a[i]);
                    i += 1;
                    j += 1;
                }
                Op::Removed => i += 1,
                Op::Added => {
                    out.push(b[j]);
                    j += 1;
                }
            }
        }
        assert_eq!(out, b);
        assert_eq!(myers(&a, &b, 4), None);
        assert_eq!(myers::<char>(&[], &[], 0), Some(Vec::new()));
        assert_eq!(myers(&['a'], &[], 0), None);
        assert_eq!(myers(&['a'], &[], 1), Some(vec![Op::Removed]));
    }

    #[test]
    fn identical_added_deleted() {
        assert_eq!(compare("a.txt", Some(b"x"), Some(b"x")).outcome, Outcome::Identical);
        assert_eq!(compare("a.txt", None, Some(b"x")).outcome, Outcome::Added);
        assert_eq!(compare("a.txt", Some(b"x"), None).outcome, Outcome::Deleted);
    }

    #[test]
    fn text_line_by_line_with_numbers() {
        let c = compare("a.ini", Some(b"one\ntwo\nthree\n"), Some(b"one\n2\nthree\nfour\n"));
        assert_eq!(c.kind, Kind::Text);
        assert_eq!(lines(&c.outcome), ["-two", "+2", "+four"]);
        let Outcome::Lines { lines, .. } = &c.outcome else { panic!() };
        let four = lines.iter().find(|l| l.text == "four").unwrap();
        assert_eq!((four.old, four.new), (None, Some(4)));
        // Only the line endings differ: the same content, said so.
        let crlf = compare("a.ini", Some(b"a\nb\n"), Some(b"a\r\nb\r\n"));
        assert_eq!(crlf.outcome, Outcome::Lines { lines: Vec::new(), same_content: true });
        assert_eq!(crlf.notes, ["Only the line endings, the final newline or a byte order mark differ"]);
    }

    #[test]
    fn differently_formatted_xml_is_the_same_and_a_change_is_shown() {
        let a = br#"<?xml version="1.0"?><CHandlingDataMgr><Item type="CHandlingData" b="2" a="1"><handlingName>ADDER</handlingName><fMass value="1800.0" /></Item></CHandlingDataMgr>"#;
        let b = "\u{feff}<CHandlingDataMgr>\r\n  <Item a=\"1\" b=\"2\" type=\"CHandlingData\">\r\n    <handlingName>ADDER</handlingName>\r\n    <fMass value=\"1800.0\"/>\r\n  </Item>\r\n</CHandlingDataMgr>\r\n";
        let same = compare("handling.meta", Some(a), Some(b.as_bytes()));
        assert_eq!((same.kind, same.outcome), (Kind::Xml, Outcome::Lines { lines: Vec::new(), same_content: true }));
        let changed = compare("handling.meta", Some(a), Some(b.replace("1800.0", "1500.0").as_bytes()));
        assert_eq!(lines(&changed.outcome), ["-    <fMass value=\"1800.0\" />", "+    <fMass value=\"1500.0\" />"]);
        // Text inside an element is kept as it is: a space there is a change.
        let spaced = compare("handling.meta", Some(a), Some(b.replace(">ADDER<", "> ADDER<").as_bytes()));
        assert_eq!(lines(&spaced.outcome).len(), 2, "{spaced:?}");
    }

    #[test]
    fn a_side_that_is_not_xml_is_compared_as_text_and_said_so() {
        let c = compare("a.xml", Some(b"<a><b/></a>"), Some(b"<a><b></a>"));
        assert_eq!(c.kind, Kind::Xml);
        assert_eq!(lines(&c.outcome), ["-<a><b/></a>", "+<a><b></a>"]);
        assert!(c.notes[0].starts_with("The modified file could not be read as XML"), "{:?}", c.notes);
    }

    #[test]
    fn bytes_say_sizes_hashes_and_where_they_differ() {
        let a = [0u8, 1, 2, 3, 4, 5, 6, 7];
        let b = [0u8, 9, 9, 3, 4, 5, 6, 7, 8, 8];
        let c = compare("a.bin", Some(&a), Some(&b));
        let Outcome::Bytes(d) = c.outcome else { panic!() };
        assert_eq!(d.sizes, [8, 10]);
        assert_eq!(d.runs, [(1, 2), (8, 2)]);
        assert!(!d.more);
        assert_eq!(d.sha256[0], "8a851ff82ee7048ad09ec3847f1ddf44944104d2cbd17ef4e3db22c6785a0d45");
        // Every other byte differs: the list stops, and says more follow.
        let many: Vec<u8> = (0..1000).map(|i| if i % 2 == 0 { 1 } else { 0 }).collect();
        let Outcome::Bytes(d) = compare("a.bin", Some(&vec![0; 1000]), Some(&many)).outcome else { panic!() };
        assert_eq!((d.runs.len(), d.more), (MAX_RUNS, true));
        // Corrupt metadata on one side: compared as bytes, said so.
        let c = compare("a.ymt", Some(b"PSIN broken"), Some(b"PSIN other"));
        assert!(matches!(c.outcome, Outcome::Bytes(_)) && c.notes.iter().any(|n| n.contains("could not be read as metadata")), "{c:?}");
        // A .ycd is bytes, whatever it holds.
        assert_eq!(compare("a.ycd", Some(b"<a/>"), Some(b"<b/>")).kind, Kind::Binary);
    }

    #[test]
    fn too_many_differences_show_the_part_whole() {
        let a: Vec<String> = (0..3000).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..3000).map(|i| format!("b{i}")).collect();
        let mut notes = Vec::new();
        let out = diff_lines(&a, &b, &mut notes);
        assert_eq!(out.len(), 6000);
        assert!(out[..3000].iter().all(|l| l.op == Op::Removed) && out[3000..].iter().all(|l| l.op == Op::Added));
        assert!(notes[0].contains("more than 2000 lines"), "{notes:?}");
    }

    #[test]
    fn hunks_keep_context_and_join() {
        let ops = "SSSSRSSSSSSSSASSSSSSSSSSARS";
        let lines: Vec<Line> = ops.chars().map(|c| Line { op: match c { 'R' => Op::Removed, 'A' => Op::Added, _ => Op::Same }, text: String::new(), old: None, new: None }).collect();
        assert_eq!(hunks(&lines, 3), [1..8, 10..17, 21..27]);
        assert_eq!(hunks(&lines, 5), vec![std::ops::Range { start: 0, end: 27 }]);
        assert!(hunks(&lines[..4], 3).is_empty());
    }

    /// A PSO file with `CPaletoTest { value: i32, model: hash }`, and a string table naming `names`.
    fn pso(value: i32, model: &str, names: &[&str]) -> Vec<u8> {
        use paleto_formats::hash::{joaat, joaat_lower};
        let be = |v: u32| v.to_be_bytes();
        let ty = joaat(b"CPaletoTest");
        let mut psin = b"PSIN".to_vec();
        psin.extend_from_slice(&be(32));
        psin.resize(16, 0);
        psin.extend_from_slice(&be(value as u32));
        psin.extend_from_slice(&be(joaat_lower(model)));
        psin.resize(32, 0);
        let mut pmap = b"PMAP".to_vec();
        for w in [32, 1] {
            pmap.extend_from_slice(&be(w));
        }
        pmap.extend_from_slice(&[0, 1, 0x70, 0x70]);
        for w in [ty, 16, 0, 16] {
            pmap.extend_from_slice(&be(w));
        }
        let mut psch = b"PSCH".to_vec();
        for w in [12 + 8 + 12 + 24, 1, ty, 20, 2, 16, 0] {
            psch.extend_from_slice(&be(w));
        }
        for (name, ty, sub, offset) in [(joaat(b"value"), 5u8, 0u8, 0u16), (joaat(b"model"), 0x0B, 7, 4)] {
            psch.extend_from_slice(&be(name));
            psch.extend_from_slice(&[ty, sub]);
            psch.extend_from_slice(&offset.to_be_bytes());
            psch.extend_from_slice(&be(0));
        }
        let mut out = [psin, pmap, psch].concat();
        if !names.is_empty() {
            let strings: Vec<u8> = names.iter().flat_map(|n| n.bytes().chain([0])).collect();
            out.extend_from_slice(b"STRF");
            out.extend_from_slice(&be(8 + strings.len() as u32));
            out.extend_from_slice(&strings);
        }
        out
    }

    #[test]
    fn metadata_is_compared_as_xml_with_the_names_either_side_knows() {
        let names = ["CPaletoTest", "value", "model", "paleto_probe_car"];
        // The original alone shows the model by its hash, the modified file by its name.
        assert!(!MetaFile::parse(&pso(5, "paleto_probe_car", &[])).unwrap().to_xml().contains("paleto_probe_car"));
        assert!(MetaFile::parse(&pso(5, "paleto_probe_car", &names)).unwrap().to_xml().contains("paleto_probe_car"));
        // Only the modified file names the model: the original would show its hash, not a change.
        let c = compare("probe.ymt", Some(&pso(5, "paleto_probe_car", &[])), Some(&pso(5, "paleto_probe_car", &names)));
        assert_eq!((c.kind, c.outcome), (Kind::Meta, Outcome::Lines { lines: Vec::new(), same_content: true }));
        let c = compare("probe.ymt", Some(&pso(5, "paleto_probe_car", &[])), Some(&pso(42, "paleto_probe_car", &names)));
        let changed = lines(&c.outcome);
        assert_eq!(changed.len(), 2, "{changed:?}");
        assert!(changed[0].starts_with('-') && changed[0].contains("5") && changed[1].contains("42"), "{changed:?}");
    }

    #[test]
    fn files_in_archives_and_on_disk() {
        use paleto_rpf::ArchiveEditor;
        let dir = tempfile::tempdir().unwrap();
        let (orig, mods) = (dir.path().join("update.rpf"), dir.path().join("mods.rpf"));
        let mut a = ArchiveEditor::new("update.rpf");
        a.add_file("common/data/a.txt", b"one\ntwo\n".to_vec()).unwrap();
        a.add_file("common/data/big.bin", vec![1; 64]).unwrap();
        a.save(&orig).unwrap();
        let mut b = ArchiveEditor::new("update.rpf");
        b.add_file("common/data/a.txt", b"one\n2\n".to_vec()).unwrap();
        b.add_file("common/data/new.txt", b"new".to_vec()).unwrap();
        b.add_file("common/data/big.bin", vec![2; 64]).unwrap();
        b.save(&mods).unwrap();
        let at = |disk: &std::path::Path, inner: &str| ArchivePath { disk: disk.to_path_buf(), inner: inner.split('/').map(String::from).collect() };
        let changed = compare_files(Some(&at(&orig, "common/data/a.txt")), &at(&mods, "common/data/a.txt"), None).unwrap();
        assert_eq!(lines(&changed.comparison.outcome), ["-two", "+2"]);
        // Bytes are kept for a byte view only.
        assert!(changed.original.is_none() && changed.modified.is_none());
        // No original: added, not an error; the original archive need not have the folder either.
        let added = compare_files(Some(&at(&orig, "common/data/new.txt")), &at(&mods, "common/data/new.txt"), None).unwrap();
        assert_eq!(added.comparison.outcome, Outcome::Added);
        let deeper = compare_files(Some(&at(&orig, "x64/none/new.txt")), &at(&mods, "common/data/new.txt"), None).unwrap();
        assert_eq!(deeper.comparison.outcome, Outcome::Added);
        // Over the limit: sizes and stored bytes only, said so.
        let large = compare_files_within(Some(&at(&orig, "common/data/big.bin")), &at(&mods, "common/data/big.bin"), None, 16).unwrap();
        assert!(matches!(large.comparison.outcome, Outcome::Large { sizes: [64, 64], .. }), "{:?}", large.comparison);
        assert!(large.comparison.notes[0].starts_with("Over 16 bytes"), "{:?}", large.comparison.notes);
        // Loose files.
        std::fs::write(dir.path().join("a.ini"), b"x=1\n").unwrap();
        std::fs::write(dir.path().join("b.ini"), b"x=2\n").unwrap();
        let loose = compare_files(Some(&ArchivePath::new(dir.path().join("a.ini"))), &ArchivePath::new(dir.path().join("b.ini")), None).unwrap();
        assert_eq!(lines(&loose.comparison.outcome), ["-x=1", "+x=2"]);
        assert!(compare_files(None, &ArchivePath::new(dir.path().join("none.ini")), None).is_err());
    }

    #[test]
    fn a_mods_copy_finds_its_original_in_any_case() {
        let dir = tempfile::tempdir().unwrap();
        let game = crate::fixtures::game_folder(dir.path(), crate::Edition::Enhanced);
        let mods = |rel: &str| ArchivePath { disk: game.mods_dir().join(rel), inner: vec!["common".into(), "data".into(), "a.txt".into()] };
        let found = mods("Update/UPDATE.rpf").original(&game).unwrap();
        assert_eq!((found.disk, found.inner), (game.root.join("update/update.rpf"), mods("x").inner));
        // An add-on pack's has none; nor has a path outside the mods folder.
        assert!(mods("update/x64/dlcpacks/addon/dlc.rpf").original(&game).is_none());
        assert!(ArchivePath::new(game.root.join("update/update.rpf")).original(&game).is_none());
    }

    #[test]
    fn what_xml_keeps_beside_the_indentation() {
        let same = |a: &str, b: &str| compare("a.xml", Some(a.as_bytes()), Some(b.as_bytes())).outcome == Outcome::Lines { lines: Vec::new(), same_content: true };
        // A space in a leaf element, between inline elements, or where xml:space keeps it, is content.
        assert!(!same("<a> </a>", "<a/>"));
        assert!(!same("<p>x <b>y</b> <i>z</i></p>", "<p>x <b>y</b><i>z</i></p>"));
        assert!(!same(r#"<a xml:space="preserve"><b/> <c/></a>"#, r#"<a xml:space="preserve"><b/><c/></a>"#));
        assert!(same("<a>\n  <b/>\n  <c/>\n</a>", "<a><b/><c/></a>"));
        // Processing instructions and comments count; CDATA is text.
        assert!(!same(r#"<?pi one?><a/>"#, r#"<?pi two?><a/>"#));
        assert!(!same("<a><!--x--></a>", "<a/>"));
        assert!(same("<a><![CDATA[1 < 2]]></a>", "<a>1 &lt; 2</a>"));
        // A DTD is not expanded: compared as text, said so.
        let dtd = compare("a.xml", Some(br#"<!DOCTYPE a [<!ENTITY e "x">]><a>&e;</a>"#), Some(b"<a>x</a>"));
        assert!(dtd.notes.iter().any(|n| n.contains("could not be read as XML")), "{:?}", dtd.notes);
    }
}
