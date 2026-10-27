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
