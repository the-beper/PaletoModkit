//! Metadata as XML, on a PSO file built by hand.

use paleto_formats::MetaFile;
use paleto_formats::hash::{joaat, joaat_lower};

/// A PSO file with one structure:
/// `CPaletoTest { value: i32, speed: f32, model: hash, items: atArray<u32> }`.
fn pso() -> Vec<u8> {
    let be = |v: u32| v.to_be_bytes();
    // Root structure (32 bytes) in block 1, the array's three values in block 2.
    let mut root = Vec::new();
    root.extend_from_slice(&be(5));
    root.extend_from_slice(&1.5f32.to_bits().to_be_bytes());
    root.extend_from_slice(&be(joaat_lower("adder")));
    root.extend_from_slice(&be(0));
    // Pointer to block 2, offset 0: like all 64-bit PSO values, two
    // big-endian halves with the low one first.
    root.extend_from_slice(&be(2));
    root.extend_from_slice(&be(0));
    root.extend_from_slice(&3u16.to_be_bytes());
    root.extend_from_slice(&3u16.to_be_bytes());
    root.extend_from_slice(&be(0));
    let items: Vec<u8> = [10u32, 20, 30].iter().flat_map(|v| v.to_be_bytes()).collect();

    let mut psin = b"PSIN".to_vec();
    psin.extend_from_slice(&be(0));
    psin.resize(16, 0);
    psin.extend_from_slice(&root);
    let items_at = psin.len();
    psin.extend_from_slice(&items);
    let len = psin.len() as u32;
    psin[4..8].copy_from_slice(&be(len));

    let ty = joaat(b"CPaletoTest");
    let mut pmap = b"PMAP".to_vec();
    pmap.extend_from_slice(&be(16 + 32));
    pmap.extend_from_slice(&be(1));
    pmap.extend_from_slice(&2u16.to_be_bytes());
    pmap.extend_from_slice(&0x7070u16.to_be_bytes());
    for (t, offset, len) in [(ty, 16, 32), (6, items_at as u32, 12)] {
        for w in [t, offset, 0, len] {
            pmap.extend_from_slice(&be(w));
        }
    }

    // Entries: name, type, subtype, offset, reference.
    let entries: [(u32, u8, u8, u16, u32); 5] = [
        (joaat(b"value"), 0x05, 0, 0, 0),
        (joaat(b"speed"), 0x07, 0, 4, 0),
        (joaat(b"model"), 0x0B, 7, 8, 0),
        (0x100, 0x06, 0, 0, 0),
        (joaat(b"items"), 0x0D, 0, 16, 3),
    ];
    let mut element = Vec::new();
    element.extend_from_slice(&be(entries.len() as u32));
    element.extend_from_slice(&be(32));
    element.extend_from_slice(&be(0));
    for (name, t, sub, offset, reference) in entries {
        element.extend_from_slice(&be(name));
        element.extend_from_slice(&[t, sub]);
        element.extend_from_slice(&offset.to_be_bytes());
        element.extend_from_slice(&be(reference));
    }
    let mut psch = b"PSCH".to_vec();
    psch.extend_from_slice(&be(12 + 8 + element.len() as u32));
    psch.extend_from_slice(&be(1));
    psch.extend_from_slice(&be(ty));
    psch.extend_from_slice(&be(20));
    psch.extend_from_slice(&element);

    [psin, pmap, psch].concat()
}

/// Names of the test structure, which the built-in dictionary does not know.
const NAMES: &str = "CPaletoTest value speed model items adder";

#[test]
fn pso_to_xml_and_back() {
    let mut meta = MetaFile::parse(&pso()).unwrap();
    assert_eq!(meta.kind(), "PSO");
    meta.learn_names(NAMES);
    let xml = meta.to_xml();
    for line in ["<CPaletoTest>", "<value value=\"5\" />", "<speed value=\"1.5\" />", "<model>adder</model>", "<items>10 20 30</items>"] {
        assert!(xml.contains(line), "{line} missing in\n{xml}");
    }

    // Unchanged XML gives the same content back.
    let mut same = MetaFile::parse(&meta.rebuild_from_xml(&xml).unwrap()).unwrap();
    same.learn_names(NAMES);
    assert_eq!(same.to_xml(), xml);

    // Edits: a value, a hash by name and a longer array.
    let edited = xml.replace("value=\"5\"", "value=\"7\"").replace(">adder<", ">zentorno<").replace("10 20 30", "1 2 3 4");
    let mut rebuilt = MetaFile::parse(&meta.rebuild_from_xml(&edited).unwrap()).unwrap();
    rebuilt.learn_names(NAMES);
    rebuilt.learn_names("zentorno");
    let xml = rebuilt.to_xml();
    for line in ["<value value=\"7\" />", "<model>zentorno</model>", "<items>1 2 3 4</items>"] {
        assert!(xml.contains(line), "{line} missing in\n{xml}");
    }
}

#[test]
fn xml_errors() {
    let mut meta = MetaFile::parse(&pso()).unwrap();
    meta.learn_names(NAMES);
    let xml = meta.to_xml();
    let err = |text: &str| meta.rebuild_from_xml(text).unwrap_err().to_string();
    assert!(err("<Other />").contains("root element must be <CPaletoTest>"));
    assert!(err(&xml.replace("<speed", "<sped")).contains("no such field"));
    assert!(err(&xml.replace("value=\"5\"", "value=\"five\"")).contains("invalid value"));
    assert!(err("<CPaletoTest>").contains("XML"));
    assert!(MetaFile::parse(b"not metadata").is_err());
}

/// A PSO file `CPaletoTwo { first: atArray<u32>, second: atArray<u32> }` whose arrays
/// follow each other in one block (the second starts at 8, not on a 16-byte boundary),
/// with gaps filled with 'p' like files made by Rockstar and CodeWalker.
fn pso_two_arrays() -> Vec<u8> {
    let be = |v: u32| v.to_be_bytes();
    let mut root = Vec::new();
    for (offset, count) in [(0u32, 2u16), (8, 3)] {
        root.extend_from_slice(&be((offset << 12) | 2));
        root.extend_from_slice(&be(0));
        root.extend_from_slice(&count.to_be_bytes());
        root.extend_from_slice(&count.to_be_bytes());
        root.extend_from_slice(&be(0));
    }
    let items: Vec<u8> = [1u32, 2, 3, 4, 5].iter().flat_map(|v| v.to_be_bytes()).collect();

    let mut psin = b"PSIN".to_vec();
    psin.extend_from_slice(&be(0));
    psin.resize(16, b'p');
    psin.extend_from_slice(&root);
    psin.resize(psin.len().next_multiple_of(16), b'p');
    let items_at = psin.len();
    psin.extend_from_slice(&items);
    let len = psin.len() as u32;
    psin[4..8].copy_from_slice(&be(len));

    let ty = joaat(b"CPaletoTwo");
    let mut pmap = b"PMAP".to_vec();
    pmap.extend_from_slice(&be(16 + 32));
    pmap.extend_from_slice(&be(1));
    pmap.extend_from_slice(&2u16.to_be_bytes());
    pmap.extend_from_slice(&0x7070u16.to_be_bytes());
    for (t, offset, len) in [(ty, 16, 32), (6, items_at as u32, 20)] {
        for w in [t, offset, 0, len] {
            pmap.extend_from_slice(&be(w));
        }
    }

    let entries: [(u32, u8, u8, u16, u32); 3] = [(0x100, 0x06, 0, 0, 0), (joaat(b"first"), 0x0D, 0, 0, 0), (joaat(b"second"), 0x0D, 0, 16, 0)];
    let mut element = Vec::new();
    element.extend_from_slice(&be(entries.len() as u32));
    element.extend_from_slice(&be(32));
    element.extend_from_slice(&be(0));
    for (name, t, sub, offset, reference) in entries {
        element.extend_from_slice(&be(name));
        element.extend_from_slice(&[t, sub]);
        element.extend_from_slice(&offset.to_be_bytes());
        element.extend_from_slice(&be(reference));
    }
    let mut psch = b"PSCH".to_vec();
    psch.extend_from_slice(&be(12 + 8 + element.len() as u32));
    psch.extend_from_slice(&be(1));
    psch.extend_from_slice(&be(ty));
    psch.extend_from_slice(&be(20));
    psch.extend_from_slice(&element);

    [psin, pmap, psch].concat()
}

#[test]
fn pso_keeps_its_layout() {
    // The game's own files pack arrays at their alignment and fill gaps with 'p':
    // rebuilding unchanged must give back the same bytes.
    let original = pso_two_arrays();
    let mut meta = MetaFile::parse(&original).unwrap();
    meta.learn_names("CPaletoTwo first second");
    let xml = meta.to_xml();
    assert!(xml.contains("<first>1 2</first>") && xml.contains("<second>3 4 5</second>"), "{xml}");
    assert_eq!(meta.rebuild_from_xml(&xml).unwrap(), original);

    // A longer first array pushes the second one back by its items only (no padding).
    let longer = meta.rebuild_from_xml(&xml.replace("1 2", "1 2 6")).unwrap();
    assert_eq!(longer.len(), original.len() + 4);
    let mut again = MetaFile::parse(&longer).unwrap();
    again.learn_names("CPaletoTwo first second");
    let xml = again.to_xml();
    assert!(xml.contains("<first>1 2 6</first>") && xml.contains("<second>3 4 5</second>"), "{xml}");
}

/// A schema entry: name, type, subtype, offset, reference.
type SchemaEntry = (u32, u8, u8, u16, u32);
/// A structure: name, size, entries.
type SchemaStruct<'a> = (u32, u32, &'a [SchemaEntry]);

/// A PSO file with the given structures and data blocks (type, bytes); block
/// 1 is the root.
fn pso_with(structs: &[SchemaStruct], blocks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let be = |v: u32| v.to_be_bytes();
    let mut psin = b"PSIN".to_vec();
    psin.extend_from_slice(&be(0));
    psin.resize(16, 0);
    let mut places = Vec::new();
    for (_, data) in blocks {
        places.push(psin.len() as u32);
        psin.extend_from_slice(data);
    }
    let len = psin.len() as u32;
    psin[4..8].copy_from_slice(&be(len));
    let mut pmap = b"PMAP".to_vec();
    pmap.extend_from_slice(&be(16 + 16 * blocks.len() as u32));
    pmap.extend_from_slice(&be(1));
    pmap.extend_from_slice(&(blocks.len() as u16).to_be_bytes());
    pmap.extend_from_slice(&0x7070u16.to_be_bytes());
    for ((ty, data), at) in blocks.iter().zip(places) {
        for w in [*ty, at, 0, data.len() as u32] {
            pmap.extend_from_slice(&be(w));
        }
    }
    let mut elements = Vec::new();
    let mut index = Vec::new();
    let table_end = 12 + 8 * structs.len() as u32;
    for (name, size, entries) in structs {
        index.extend_from_slice(&be(*name));
        index.extend_from_slice(&be(table_end + elements.len() as u32));
        elements.extend_from_slice(&be(entries.len() as u32));
        elements.extend_from_slice(&be(*size));
        elements.extend_from_slice(&be(0));
        for &(name, t, sub, offset, reference) in *entries {
            elements.extend_from_slice(&be(name));
            elements.extend_from_slice(&[t, sub]);
            elements.extend_from_slice(&offset.to_be_bytes());
            elements.extend_from_slice(&be(reference));
        }
    }
    let mut psch = b"PSCH".to_vec();
    psch.extend_from_slice(&be(table_end + elements.len() as u32));
    psch.extend_from_slice(&be(structs.len() as u32));
    psch.extend_from_slice(&index);
    psch.extend_from_slice(&elements);
    [psin, pmap, psch].concat()
}

/// Damaged files give an error, never a stack overflow, a panic or a huge allocation.
#[test]
fn damaged_pso() {
    // An array whose element is described as the array itself.
    let mut data = pso();
    let info = [0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0, 0];
    let at = data.windows(12).position(|w| w == info).unwrap();
    data[at + 4] = 0x0D;
    data[at + 8..at + 12].copy_from_slice(&3u32.to_be_bytes());
    assert!(MetaFile::parse(&data).is_err());

    // A schema cut between an entry's type and subtype (PSCH is the last section).
    let mut data = pso();
    let psch = data.windows(4).position(|w| w == b"PSCH").unwrap();
    let cut = 20 + 12 + 4 * 12 + 5;
    data[psch + 4..psch + 8].copy_from_slice(&(cut as u32).to_be_bytes());
    data.truncate(psch + cut);
    assert!(MetaFile::parse(&data).is_err());

    // Four billion empty structures in an array of 16 bytes (two read fine).
    let (root, empty) = (joaat(b"CPaletoRoot"), joaat(b"CPaletoEmpty"));
    let file = |count: u32| {
        let mut root_data = 2u32.to_be_bytes().to_vec(); // block 2, offset 0
        root_data.extend_from_slice(&[0; 4]);
        root_data.extend_from_slice(&count.to_be_bytes());
        root_data.resize(16, 0);
        pso_with(
            &[(root, 16, &[(0x100, 0x0C, 0, 0, empty), (joaat(b"items"), 0x0D, 6, 0, 0)]), (empty, 0, &[])],
            &[(root, root_data), (empty, vec![0; 16])],
        )
    };
    let mut meta = MetaFile::parse(&file(2)).unwrap();
    meta.learn_names("CPaletoRoot CPaletoEmpty items");
    assert_eq!(meta.to_xml().matches("<Item").count(), 2, "{}", meta.to_xml());
    assert!(MetaFile::parse(&file(u32::MAX)).is_err());
}

// Hostile input: each bound on work, memory and rebuilt size gives an error,
// never a stack overflow, a hang, a huge allocation, a panic or a silent truncation.

/// The error of a result, as text.
fn error<T>(result: Result<T, paleto_formats::Error>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

/// A 64-bit PSO pointer to `offset` in block `id`: two big-endian halves, low first.
fn pso_ptr(id: u32, offset: u32) -> [u8; 8] {
    let mut b = [0; 8];
    b[..4].copy_from_slice(&(id | (offset << 12)).to_be_bytes());
    b
}

/// A PSO file whose root structure `CPaletoR` has the given entries and size,
/// with `data` as its block; `others` are further structures.
fn pso_root(entries: &[SchemaEntry], size: u32, data: Vec<u8>, others: &[SchemaStruct]) -> Vec<u8> {
    let root = joaat(b"CPaletoR");
    let structs: Vec<SchemaStruct> = [(root, size, entries)].into_iter().chain(others.iter().copied()).collect();
    pso_with(&structs, &[(root, data)])
}

/// An RBF file: `depth` nested elements named `name`, the innermost holding
/// `flags` boolean values that reuse the same name.
fn rbf(name: &str, depth: usize, flags: usize) -> Vec<u8> {
    let mut out = b"RBF0".to_vec();
    for level in 0..depth {
        out.extend_from_slice(&[0, 0x00]);
        if level == 0 {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(&[0; 6]);
    }
    for _ in 0..flags {
        out.extend_from_slice(&[0, 0x20]);
    }
    for _ in 0..depth {
        out.extend_from_slice(&[0xFF, 0xFF]);
    }
    out
}

#[test]
fn rbf_nesting_is_bounded() {
    // The tree is converted and dropped recursively: deep files would overflow the stack.
    assert!(MetaFile::parse(&rbf("a", 64, 0)).is_ok());
    assert!(error(MetaFile::parse(&rbf("a", 65, 0))).contains("nests too deeply"));
    assert!(error(MetaFile::parse(&rbf("a", 100_000, 0))).contains("nests too deeply"));

    // The same bound when the XML is turned back into RBF.
    let meta = MetaFile::parse(&rbf("a", 1, 0)).unwrap();
    let nested = |depth: usize| "<a>".repeat(depth) + &"</a>".repeat(depth);
    assert!(meta.rebuild_from_xml(&nested(64)).is_ok());
    assert!(error(meta.rebuild_from_xml(&nested(65))).contains("nest more than 64 deep"));
    assert!(error(meta.rebuild_from_xml(&nested(100_000))).contains("nest more than 64 deep"));
}

#[test]
fn rbf_repeated_names_are_bounded() {
    // Every record copies its name: one long name reused by many two-byte
    // records would multiply the file's size in memory.
    let long = "n".repeat(60_000);
    let meta = MetaFile::parse(&rbf(&long, 1, 10)).unwrap();
    assert_eq!(meta.to_xml().matches("=\"True\"").count(), 10);
    assert!(error(MetaFile::parse(&rbf(&long, 1, 100))).contains("repeats its names too much"));
}

#[test]
fn xml_nesting_is_bounded() {
    // Pointed-to structures nest without end in XML: `<next type="CPaletoR">` in each other.
    let meta = MetaFile::parse(&pso_root(&[(joaat(b"next"), 0x0C, 3, 0, 0)], 8, vec![0; 8], &[])).unwrap();
    let nested = |depth: usize| format!("<CPaletoR>{}{}</CPaletoR>", "<next type=\"CPaletoR\">".repeat(depth), "</next>".repeat(depth));
    assert!(meta.rebuild_from_xml(&nested(64)).is_ok());
    assert!(error(meta.rebuild_from_xml(&nested(65))).contains("structures nest more than 64 deep"));
    assert!(error(meta.rebuild_from_xml(&nested(10_000))).contains("structures nest more than 64 deep"));
}

#[test]
fn xml_nesting_matches_the_binary_reader() {
    // Each `<next>` is one element and one level of structures: the XML must
    // refuse what the binary reader would refuse, or Paleto writes a file it
    // cannot open again.
    let meta = MetaFile::parse(&pso_root(&[(joaat(b"next"), 0x0C, 3, 0, 0)], 8, vec![0; 8], &[])).unwrap();
    let nested = |depth: usize| format!("<CPaletoR>{}{}</CPaletoR>", "<next type=\"CPaletoR\">".repeat(depth), "</next>".repeat(depth));
    let mut back = MetaFile::parse(&meta.rebuild_from_xml(&nested(64)).unwrap()).unwrap();
    back.learn_names("CPaletoR next");
    assert_eq!(back.to_xml().matches("<next type=").count(), 64);
    assert!(error(meta.rebuild_from_xml(&nested(65))).contains("structures nest more than 64 deep"));
}

#[test]
fn pso_reading_work_is_bounded() {
    // A chain of structures, each with two pointers to the next one: the
    // last of `n` is read 2^(n-1) times.
    let node = |n: u32| {
        let mut data = Vec::new();
        for i in 0..n {
            let next = if i + 1 < n { pso_ptr(1, 16 * (i + 1)) } else { [0; 8] };
            data.extend_from_slice(&next);
            data.extend_from_slice(&next);
        }
        pso_root(&[(joaat(b"left"), 0x0C, 3, 0, 0), (joaat(b"right"), 0x0C, 3, 8, 0)], 16, data, &[])
    };
    let mut meta = MetaFile::parse(&node(4)).unwrap();
    meta.learn_names("CPaletoR left right");
    assert_eq!(meta.to_xml().matches("<left type=").count(), 7, "{}", meta.to_xml());
    assert!(error(MetaFile::parse(&node(20))).contains("referenced too many times"));
}

#[test]
fn xml_reading_work_is_bounded() {
    // Structure k holds two of structure k-1 by value: one empty element
    // asks for 2^k default values.
    let level = |k: u32| 0x5000 + k;
    let mut entries = vec![vec![(joaat(b"x"), 0x05, 0, 0, 0)]];
    for k in 1..=20 {
        entries.push(vec![(joaat(b"a"), 0x0C, 0, 0, level(k - 1)), (joaat(b"b"), 0x0C, 0, 0, level(k - 1))]);
    }
    let structs: Vec<SchemaStruct> = entries.iter().enumerate().map(|(k, e)| (level(k as u32), 4, &e[..])).collect();
    let meta = MetaFile::parse(&pso_root(&[(joaat(b"opt"), 0x0C, 3, 0, 0)], 8, vec![0; 8], &structs)).unwrap();
    let xml = |k: u32| format!("<CPaletoR><opt type=\"hash_{:08X}\" /></CPaletoR>", level(k));
    assert!(meta.rebuild_from_xml(&xml(4)).is_ok());
    assert!(error(meta.rebuild_from_xml(&xml(20))).contains("too many values"));
}

#[test]
fn pso_overlapping_blocks_are_bounded() {
    // Block map entries that all cover the same kilobyte: memory follows the
    // data, not the number of entries.
    let file = pso_root(&[(joaat(b"value"), 0x05, 0, 0, 0)], 4, vec![0; 1024], &[]);
    let overlapping = |copies: usize| {
        let at = file.windows(4).position(|w| w == b"PMAP").unwrap();
        let len = u32::from_be_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        let mut pmap = file[at..at + len].to_vec();
        let first = pmap[16..32].to_vec();
        for _ in 0..copies {
            pmap.extend_from_slice(&first);
        }
        let new_len = pmap.len() as u32;
        pmap[4..8].copy_from_slice(&new_len.to_be_bytes());
        pmap[12..14].copy_from_slice(&(1 + copies as u16).to_be_bytes());
        [&file[..at], &pmap, &file[at + len..]].concat()
    };
    // PSIN holds 1040 bytes: four copies of the block fit in four times that, five do not.
    assert!(MetaFile::parse(&overlapping(3)).is_ok());
    assert!(error(MetaFile::parse(&overlapping(4))).contains("blocks overlap too much"));
}

/// A resource metadata entry: name, offset, type, array info index, reference.
type RscEntry = (u32, u32, u8, i16, u32);

/// Resource metadata (RSC7 version 2) with the given structures (name, size,
/// entries) and data blocks (type, bytes; block 1 is the root), plus
/// `overlapping` block infos that each cover all the data before them.
fn rsc_with(structs: &[(u32, u32, &[RscEntry])], blocks: &[(u32, Vec<u8>)], overlapping: usize) -> Vec<u8> {
    use std::io::Write;
    const SYS: u64 = 0x5000_0000;
    fn place(system: &mut Vec<u8>, bytes: &[u8]) -> u64 {
        system.resize(system.len().next_multiple_of(16), 0);
        let at = system.len();
        system.extend_from_slice(bytes);
        SYS + at as u64
    }
    let mut system = vec![0u8; 112];
    let mut infos = Vec::new();
    for (name, size, entries) in structs {
        let mut bytes = Vec::new();
        for &(n, offset, ty, index, reference) in *entries {
            bytes.extend_from_slice(&n.to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&[ty, 0]);
            bytes.extend_from_slice(&index.to_le_bytes());
            bytes.extend_from_slice(&reference.to_le_bytes());
        }
        let ptr = if bytes.is_empty() { 0 } else { place(&mut system, &bytes) };
        let mut info = [0u8; 32];
        info[0..4].copy_from_slice(&name.to_le_bytes());
        info[0x10..0x18].copy_from_slice(&ptr.to_le_bytes());
        info[0x18..0x1C].copy_from_slice(&size.to_le_bytes());
        info[0x1E..0x20].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        infos.extend_from_slice(&info);
    }
    let struct_infos = place(&mut system, &infos);
    let mut infos = Vec::new();
    for (ty, data) in blocks {
        let ptr = place(&mut system, data);
        infos.extend_from_slice(&ty.to_le_bytes());
        infos.extend_from_slice(&(data.len() as u32).to_le_bytes());
        infos.extend_from_slice(&ptr.to_le_bytes());
    }
    let before = system.len() as u32;
    for _ in 0..overlapping {
        infos.extend_from_slice(&0x11u32.to_le_bytes());
        infos.extend_from_slice(&before.to_le_bytes());
        infos.extend_from_slice(&SYS.to_le_bytes());
    }
    let block_infos = place(&mut system, &infos);
    system[0x1C..0x20].copy_from_slice(&1u32.to_le_bytes());
    system[0x20..0x28].copy_from_slice(&struct_infos.to_le_bytes());
    system[0x30..0x38].copy_from_slice(&block_infos.to_le_bytes());
    system[0x48..0x4A].copy_from_slice(&(structs.len() as u16).to_le_bytes());
    system[0x4C..0x4E].copy_from_slice(&((blocks.len() + overlapping) as u16).to_le_bytes());

    // One system page, large enough.
    let shift = (0..16).find(|s| 0x200usize << s >= system.len()).unwrap();
    let mut out: Vec<u8> = [0x3743_5352u32, 2, 0x0800_0000 | shift, 0].iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&system).unwrap();
    out.extend(z.finish().unwrap());
    out
}

#[test]
fn rsc_overlapping_blocks_are_bounded() {
    // Block infos that all point at the same page: memory follows the
    // resource's size, not the count.
    let root = joaat(b"CPaletoR");
    let file = |overlapping: usize| rsc_with(&[(root, 4, &[(joaat(b"value"), 0, 0x14, 0, 0)])], &[(root, vec![0; 16])], overlapping);
    assert!(MetaFile::parse(&file(1)).is_ok());
    assert!(error(MetaFile::parse(&file(8))).contains("blocks overlap too much"));
}

#[test]
fn rebuilt_size_is_bounded() {
    // An empty array of pointers to a structure the schema says takes 1 MB:
    // a few lines of XML would ask for gigabytes.
    let big = joaat(b"CPaletoBig");
    let meta = MetaFile::parse(&pso_root(&[(0x100, 0x0C, 3, 0, 0), (joaat(b"items"), 0x0D, 0, 0, 0)], 16, vec![0; 16], &[(big, 0xFFFFF, &[])])).unwrap();
    let xml = |n: usize| format!("<CPaletoR><items>{}</items></CPaletoR>", "<Item type=\"CPaletoBig\" />".repeat(n));
    assert!(meta.rebuild_from_xml(&xml(2)).is_ok());
    assert!(error(meta.rebuild_from_xml(&xml(100))).contains("far larger than the original"));
}

#[test]
fn self_containing_structures_are_refused() {
    let (a, b) = (joaat(b"CPaletoA"), joaat(b"CPaletoB"));
    let file = |structs: &[SchemaStruct]| pso_with(structs, &[(a, vec![0; 16])]);
    // Directly, through another structure, and through an inline array.
    let direct: &[SchemaEntry] = &[(joaat(b"me"), 0x0C, 0, 0, a)];
    assert!(error(MetaFile::parse(&file(&[(a, 4, direct)]))).contains("contains itself"));
    let (to_b, to_a): (&[SchemaEntry], &[SchemaEntry]) = (&[(joaat(b"b"), 0x0C, 0, 0, b)], &[(joaat(b"a"), 0x0C, 0, 0, a)]);
    assert!(error(MetaFile::parse(&file(&[(a, 4, to_b), (b, 4, to_a)]))).contains("contains itself"));
    let inline: &[SchemaEntry] = &[(0x100, 0x0C, 0, 0, a), (joaat(b"items"), 0x0D, 1, 0, 2 << 16)];
    assert!(error(MetaFile::parse(&file(&[(a, 4, inline)]))).contains("contains itself"));

    // A chain of distinct structures by value: 64 levels are read, 65 refused.
    let chain = |n: u32| {
        let entries: Vec<Vec<SchemaEntry>> =
            (0..n).map(|i| if i + 1 < n { vec![(joaat(b"next"), 0x0C, 0, 0, 0x7000 + i + 1)] } else { vec![(joaat(b"x"), 0x05, 0, 0, 0)] }).collect();
        let structs: Vec<SchemaStruct> = entries.iter().enumerate().map(|(i, e)| (0x7000 + i as u32, 4, &e[..])).collect();
        pso_with(&structs, &[(0x7000, vec![0; 4])])
    };
    assert!(MetaFile::parse(&chain(64)).is_ok());
    assert!(error(MetaFile::parse(&chain(65))).contains("nests too deeply"));
}

#[test]
fn field_past_its_structure_is_reported() {
    // A 4-byte structure with a field at offset 8: the block holds 16 bytes,
    // so it reads, but rebuilding has only the structure's 4.
    let mut meta = MetaFile::parse(&pso_root(&[(joaat(b"value"), 0x05, 0, 8, 0)], 4, vec![0; 16], &[])).unwrap();
    meta.learn_names("CPaletoR value");
    let xml = meta.to_xml();
    assert!(xml.contains("<value value=\"0\" />"), "{xml}");
    assert!(error(meta.rebuild_from_xml(&xml)).contains("a field lies outside its structure"));
}

#[test]
fn block_ids_past_4095_are_reported() {
    // Each byte blob takes a block of its own; block ids have 12 bits.
    let (root, entry) = (joaat(b"CPaletoR"), joaat(b"CPaletoE"));
    let root_entries: &[RscEntry] = &[(0x100, 0, 0x05, 0, entry), (joaat(b"items"), 0, 0x52, 0, 0)];
    let entry_entries: &[RscEntry] = &[(joaat(b"data"), 0, 0x59, 0, 0)];
    let meta = MetaFile::parse(&rsc_with(&[(root, 16, root_entries), (entry, 8, entry_entries)], &[(root, vec![0; 16])], 0)).unwrap();
    let xml = |n: usize| format!("<CPaletoR><items>{}</items></CPaletoR>", "<Item><data>AB</data></Item>".repeat(n));
    let mut rebuilt = MetaFile::parse(&meta.rebuild_from_xml(&xml(3)).unwrap()).unwrap();
    rebuilt.learn_names("CPaletoR CPaletoE items data");
    assert_eq!(rebuilt.to_xml().matches("AB").count(), 3, "{}", rebuilt.to_xml());
    assert!(error(meta.rebuild_from_xml(&xml(4100))).contains("too many data blocks"));
}

#[test]
fn counts_past_their_width_are_reported() {
    // An atArray holds a 16-bit count: 65536 items would be written as 0.
    let meta = MetaFile::parse(&pso_root(&[(0x100, 0x02, 0, 0, 0), (joaat(b"items"), 0x0D, 0, 0, 0)], 16, vec![0; 16], &[])).unwrap();
    let xml = |n: usize| format!("<CPaletoR><items>{}</items></CPaletoR>", "1 ".repeat(n));
    let mut rebuilt = MetaFile::parse(&meta.rebuild_from_xml(&xml(65535)).unwrap()).unwrap();
    rebuilt.learn_names("CPaletoR items");
    assert_eq!(rebuilt.to_xml().split_whitespace().filter(|w| *w == "1").count(), 65535);
    assert!(error(meta.rebuild_from_xml(&xml(65536))).contains("65536 is too many for a 16-bit count"));

    // A string behind a pointer, with a 16-bit length.
    let meta = MetaFile::parse(&pso_root(&[(joaat(b"name"), 0x0B, 3, 0, 0)], 16, vec![0; 16], &[])).unwrap();
    let xml = |n: usize| format!("<CPaletoR><name>{}</name></CPaletoR>", "s".repeat(n));
    assert!(meta.rebuild_from_xml(&xml(65535)).is_ok());
    assert!(error(meta.rebuild_from_xml(&xml(65536))).contains("too many for a 16-bit count"));

    // RBF names and strings have 16-bit lengths.
    let meta = MetaFile::parse(&rbf("a", 1, 0)).unwrap();
    let long = "x".repeat(65536);
    assert!(meta.rebuild_from_xml(&format!("<a b=\"{}\" />", &long[1..])).is_ok());
    assert!(error(meta.rebuild_from_xml(&format!("<a b=\"{long}\" />"))).contains("too many bytes in a string"));
    assert!(error(meta.rebuild_from_xml(&format!("<{long} />"))).contains("too many bytes in a name"));
}

#[test]
fn xml_integers_must_fit_their_field() {
    // Numbers fit their field signed or not: a U8 of 300 would be written as 44.
    let entries: &[SchemaEntry] = &[(joaat(b"byte"), 0x02, 0, 0, 0), (joaat(b"short"), 0x03, 0, 2, 0), (joaat(b"int"), 0x05, 0, 4, 0)];
    let meta = MetaFile::parse(&pso_root(entries, 8, vec![0; 8], &[])).unwrap();
    let rebuild = |field: &str, value: &str| meta.rebuild_from_xml(&format!("<CPaletoR><{field} value=\"{value}\" /></CPaletoR>"));
    for (field, low, high, size) in [("byte", "-128", "255", 1), ("short", "-32768", "65535", 2), ("int", "-2147483648", "4294967295", 4)] {
        assert!(rebuild(field, low).is_ok() && rebuild(field, high).is_ok(), "{field}");
        let below = (low.parse::<i64>().unwrap() - 1).to_string();
        let above = (high.parse::<i64>().unwrap() + 1).to_string();
        for value in [below, above] {
            assert!(error(rebuild(field, &value)).contains(&format!("{value} does not fit in {size} bytes")), "{field} {value}");
        }
    }
    // Array items too.
    let mut meta = MetaFile::parse(&pso()).unwrap();
    meta.learn_names(NAMES);
    let xml = meta.to_xml();
    assert!(error(meta.rebuild_from_xml(&xml.replace("10 20 30", "10 4294967296 30"))).contains("does not fit in 4 bytes"));
}
