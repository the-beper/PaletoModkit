//! A navigation mesh built by hand: a quad and a triangle sharing an edge.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use paleto_formats::navmesh;

const SYS: u64 = 0x5000_0000;

/// Sector bounds (100, 200, 10) to (250, 350, 30); vertices at the corners of the quad
/// (0, 0), (1, 0), (1, 1), (0, 1) in sector units and one more at (2, 0.5) beyond it.
fn build() -> Vec<u8> {
    pack(&system())
}

/// The system pages of [`build`], to damage before packing them.
fn system() -> Vec<u8> {
    let mut system = vec![0u8; 0x1000];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    // Root: lists at 0x200 (vertices), 0x280 (indices), 0x300 (polygons); sector at 0x380.
    put(0x70, &(SYS + 0x200).to_le_bytes());
    put(0x80, &(SYS + 0x280).to_le_bytes());
    put(0x118, &(SYS + 0x300).to_le_bytes());
    put(0x120, &(SYS + 0x380).to_le_bytes());
    for (at, v) in [(0x380, 100.0f32), (0x384, 200.0), (0x388, 10.0), (0x390, 250.0), (0x394, 350.0), (0x398, 30.0)] {
        put(at, &v.to_le_bytes());
    }
    // Each list has two parts (parts table at +0x40, data further on) to cover the joining.
    let list = |put: &mut dyn FnMut(usize, &[u8]), at: usize, total: u32, parts: [(usize, u32); 2]| {
        put(at + 8, &total.to_le_bytes());
        put(at + 0x10, &(SYS + at as u64 + 0x40).to_le_bytes());
        put(at + 0x20, &2u32.to_le_bytes());
        for (k, (data, count)) in parts.into_iter().enumerate() {
            put(at + 0x40 + k * 16, &(SYS + data as u64).to_le_bytes());
            put(at + 0x48 + k * 16, &count.to_le_bytes());
        }
    };
    list(&mut put, 0x200, 5, [(0x400, 3), (0x440, 2)]);
    list(&mut put, 0x280, 7, [(0x480, 4), (0x4a0, 3)]);
    list(&mut put, 0x300, 2, [(0x500, 1), (0x540, 1)]);
    let max = u16::MAX;
    let vertices = [[0, 0, 0], [max, 0, 0], [max, max, max], [0, max, max], [max, max / 2, max]];
    for (k, v) in vertices.iter().enumerate() {
        let at = if k < 3 { 0x400 + k * 6 } else { 0x440 + (k - 3) * 6 };
        for (i, c) in v.iter().enumerate() {
            put(at + i * 2, &c.to_le_bytes());
        }
    }
    // Indices: quad 0 1 2 3, then triangle 1 4 2.
    for (k, i) in [0u16, 1, 2, 3].into_iter().enumerate() {
        put(0x480 + k * 2, &i.to_le_bytes());
    }
    for (k, i) in [1u16, 4, 2].into_iter().enumerate() {
        put(0x4a0 + k * 2, &i.to_le_bytes());
    }
    // Polygons: index count in bits 5.. of the u16 at +2, first index at +4.
    put(0x502, &(4u16 << 5).to_le_bytes());
    put(0x542, &(3u16 << 5).to_le_bytes());
    put(0x544, &4u16.to_le_bytes());
    system
}

fn pack(system: &[u8]) -> Vec<u8> {
    let version = 2u32;
    let pages = 1 << 24; // 8 pages of 0x200 bytes
    let mut out = Vec::new();
    for w in [0x3743_5352, version, ((version >> 4) << 28) | pages, ((version & 0xF) << 28)] {
        out.extend_from_slice(&u32::to_le_bytes(w));
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(system).unwrap();
    z.finish().unwrap()
}

#[test]
fn polygons_become_triangle_fans() {
    let drawables = navmesh::parse(&build()).unwrap();
    let [drawable] = &drawables[..] else { panic!("one drawable expected") };
    assert!(drawable.textures.is_empty());
    let [(name, mesh)] = &drawable.lods[..] else { panic!("one level expected") };
    assert_eq!(*name, "Navmesh");
    assert_eq!(mesh.positions.len(), 5);
    assert_eq!(mesh.positions[0], [100.0, 200.0, 10.0]);
    assert_eq!(mesh.positions[2], [250.0, 350.0, 30.0]);
    assert!((mesh.positions[4][1] - 275.0).abs() < 0.01);
    assert_eq!(mesh.uvs.len(), 5);
    assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3, 1, 4, 2]);
    assert_eq!((mesh.parts.len(), mesh.parts[0].start, mesh.parts[0].count, mesh.parts[0].texture), (1, 0, 9, None));
}

#[test]
fn files_that_are_not_navmeshes_are_refused() {
    assert!(navmesh::parse(b"not a resource at all").is_err());
}

fn put(system: &mut [u8], at: usize, bytes: &[u8]) {
    system[at..at + bytes.len()].copy_from_slice(bytes);
}

#[test]
fn list_parts_beyond_the_total_are_refused() {
    // Both parts of the vertex list point at the first block, with the whole count each.
    let mut s = system();
    for k in 0..2 {
        put(&mut s, 0x240 + k * 16, &(SYS + 0x400).to_le_bytes());
        put(&mut s, 0x248 + k * 16, &5u32.to_le_bytes());
    }
    assert!(navmesh::parse(&pack(&s)).is_err());
    // One part far larger than the total, and than the file.
    let mut s = system();
    put(&mut s, 0x248, &u32::MAX.to_le_bytes());
    assert!(navmesh::parse(&pack(&s)).is_err());
    // More parts than any file has, and a total that is never reached.
    for (at, value) in [(0x220, u32::MAX), (0x220, 4097), (0x208, u32::MAX), (0x208, 0)] {
        let mut s = system();
        put(&mut s, at, &value.to_le_bytes());
        assert!(navmesh::parse(&pack(&s)).is_err(), "{at:#x} = {value}");
    }
}

#[test]
fn wild_pointers_and_truncated_files_are_errors() {
    let wild = [u64::MAX, u64::MAX - 0xF, SYS + 0x0FFF_FFFF, SYS + 0xFFF, 0x6000_0000, 0x7000_0000, 0];
    // The root's lists and sector, a list's parts table, and one part's data.
    for at in [0x70, 0x80, 0x118, 0x120, 0x210, 0x240, 0x2d0, 0x350] {
        for ptr in wild {
            let mut s = system();
            put(&mut s, at, &ptr.to_le_bytes());
            assert!(navmesh::parse(&pack(&s)).is_err(), "{at:#x} = {ptr:#x}");
        }
    }
    // Sector bounds that are NaN, infinite or inverted.
    for (at, v) in [(0x380, f32::NAN), (0x394, f32::INFINITY), (0x390, 0.0)] {
        let mut s = system();
        put(&mut s, at, &v.to_le_bytes());
        assert!(navmesh::parse(&pack(&s)).is_err(), "{at:#x} = {v}");
    }
    // Polygons whose indices run past the list or name vertices it lacks: left out.
    let mut s = system();
    put(&mut s, 0x502, &(0x7FFu16 << 5).to_le_bytes());
    put(&mut s, 0x544, &u16::MAX.to_le_bytes());
    assert!(navmesh::parse(&pack(&s)).is_err());
    let mut s = system();
    put(&mut s, 0x4a2, &u16::MAX.to_le_bytes());
    let drawables = navmesh::parse(&pack(&s)).unwrap();
    assert_eq!(drawables[0].lods[0].1.indices, [0, 1, 2, 0, 2, 3]);
    // Every truncation of a valid file.
    let data = build();
    for n in 0..data.len() {
        let _ = navmesh::parse(&data[..n]);
    }
}
