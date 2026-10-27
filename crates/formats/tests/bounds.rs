//! Collision bounds built by hand: a composite with a translated box and a one-triangle geometry.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

const SYS: u64 = 0x5000_0000;

/// A resource whose system pages (0x400 bytes) are filled by `fill`.
fn resource(fill: impl Fn(&mut dyn FnMut(usize, &[u8]))) -> Vec<u8> {
    let mut system = vec![0u8; 0x400];
    fill(&mut |at, bytes| system[at..at + bytes.len()].copy_from_slice(bytes));
    let version: u32 = 43;
    let pages = (1 << 27) | 1; // one page of 0x400 bytes
    let mut out = Vec::new();
    for w in [0x3743_5352, version, ((version >> 4) << 28) | pages, (version & 0xF) << 28] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&system).unwrap();
    z.finish().unwrap()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn composite(put: &mut dyn FnMut(usize, &[u8])) {
    put(0x10, &[10]);
    put(0x70, &(SYS + 0x220).to_le_bytes());
    put(0x78, &(SYS + 0x240).to_le_bytes());
    put(0xA0, &2u16.to_le_bytes());
    put(0x220, &(SYS + 0xC0).to_le_bytes());
    put(0x228, &(SYS + 0x140).to_le_bytes());
    // Transformations: the box moves 10 along X, the geometry stays.
    put(0x240, &floats(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 0.0, 0.0, 1.0]));
    put(0x280, &floats(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
}

fn box_bound(put: &mut dyn FnMut(usize, &[u8])) {
    put(0xC0 + 0x10, &[3]);
    put(0xC0 + 0x20, &floats(&[1.0, 2.0, 3.0]));
    put(0xC0 + 0x30, &floats(&[-1.0, -2.0, -3.0]));
}

fn geometry(put: &mut dyn FnMut(usize, &[u8])) {
    put(0x140 + 0x10, &[4]);
    put(0x140 + 0x88, &(SYS + 0x2E0).to_le_bytes());
    put(0x140 + 0x90, &floats(&[1.0, 1.0, 1.0]));
    put(0x140 + 0xB0, &(SYS + 0x2C0).to_le_bytes());
    put(0x140 + 0xD0, &3u32.to_le_bytes());
    put(0x140 + 0xD4, &1u32.to_le_bytes());
    let shorts: Vec<u8> = [0i16, 0, 0, 4, 0, 0, 0, 4, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
    put(0x2C0, &shorts);
    put(0x2E4, &[0, 0, 1, 0, 2, 0]); // triangle: type 0 at byte 0, vertices 0, 1, 2 from byte 4
}

#[test]
fn composite_is_flattened_with_transformations() {
    let data = resource(|put| {
        composite(put);
        box_bound(put);
        geometry(put);
    });
    let drawables = paleto_formats::bounds::parse(&data).unwrap();
    let [drawable] = &drawables[..] else { panic!("one drawable expected") };
    let [(name, mesh)] = &drawable.lods[..] else { panic!("one level expected") };
    assert_eq!(*name, "Collision");
    // 12 triangles for the box, 1 for the geometry.
    assert_eq!(mesh.indices.len(), 39);
    assert_eq!(mesh.positions.len(), 39);
    assert_eq!(mesh.uvs.len(), 39);
    assert_eq!((mesh.parts.len(), mesh.parts[0].count, mesh.parts[0].texture), (1, 39, None));
    let (box_points, tri) = mesh.positions.split_at(36);
    for a in 0..3 {
        let half = [1.0, 2.0, 3.0][a];
        let shift = if a == 0 { 10.0 } else { 0.0 };
        let lo = box_points.iter().map(|p| p[a]).fold(f32::MAX, f32::min);
        let hi = box_points.iter().map(|p| p[a]).fold(f32::MIN, f32::max);
        assert!((lo - (shift - half)).abs() < 1e-4 && (hi - (shift + half)).abs() < 1e-4, "axis {a}: {lo}..{hi}");
    }
    assert_eq!(tri, [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]]);
}

#[test]
fn unreadable_bounds_are_an_error() {
    let data = resource(|put| put(0x10, &[99]));
    assert!(paleto_formats::bounds::parse(&data).is_err());
    assert!(paleto_formats::bounds::parse(b"not a resource").is_err());
}
