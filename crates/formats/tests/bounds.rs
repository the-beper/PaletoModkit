//! Collision bounds built by hand: a composite with a translated box and a one-triangle geometry.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

const SYS: u64 = 0x5000_0000;

/// A resource whose system pages (0x400 bytes) are filled by `fill`.
fn resource(fill: impl Fn(&mut dyn FnMut(usize, &[u8]))) -> Vec<u8> {
    resource_of(1, fill)
}

/// A resource whose system page of 0x200 << `shift` bytes is filled by `fill`.
fn resource_of(shift: u32, fill: impl Fn(&mut dyn FnMut(usize, &[u8]))) -> Vec<u8> {
    let mut system = vec![0u8; 0x200 << shift];
    fill(&mut |at, bytes| system[at..at + bytes.len()].copy_from_slice(bytes));
    let version: u32 = 43;
    let pages = (1 << 27) | shift; // one page
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

const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

#[test]
fn composite_pointing_back_at_itself_finishes() {
    // The root lists itself 4095 times and then a box, with no transformations.
    let data = resource_of(7, |put| {
        put(0x10, &[10]);
        put(0x70, &(SYS + 0x1000).to_le_bytes());
        put(0xA0, &u16::MAX.to_le_bytes());
        for i in 0..4095 {
            put(0x1000 + i * 8, &SYS.to_le_bytes());
        }
        put(0x1000 + 4095 * 8, &(SYS + 0xC0).to_le_bytes());
        box_bound(put);
    });
    let start = std::time::Instant::now();
    let drawables = paleto_formats::bounds::parse(&data).unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());
    let mesh = &drawables[0].lods[0].1;
    assert!(!mesh.positions.is_empty() && mesh.positions.len().is_multiple_of(36), "{}", mesh.positions.len());
    assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
}

#[test]
fn wild_pointers_and_truncated_files_are_not_a_panic() {
    let wild = [u64::MAX, u64::MAX - 7, SYS + 0x0FFF_FFF8, SYS + 0x3FF, 0x6000_0000, 0x7000_0000, 1];
    // A composite whose children, transformations and child count are all out of range.
    for &ptr in &wild {
        let data = resource(|put| {
            put(0x10, &[10]);
            put(0x70, &ptr.to_le_bytes());
            put(0x78, &ptr.to_le_bytes());
            put(0xA0, &u16::MAX.to_le_bytes());
        });
        let _ = paleto_formats::bounds::parse(&data);
        // Children that are wild themselves, with transformations running off the end.
        let data = resource(|put| {
            put(0x10, &[10]);
            put(0x70, &(SYS + 0x200).to_le_bytes());
            put(0x78, &(SYS + 0x3F0).to_le_bytes());
            put(0xA0, &4u16.to_le_bytes());
            for i in 0..3 {
                put(0x200 + i * 8, &ptr.to_le_bytes());
            }
            put(0x218, &(SYS + 0xC0).to_le_bytes());
            box_bound(put);
        });
        let _ = paleto_formats::bounds::parse(&data);
        // A geometry whose vertices and polygons are out of range, with counts at their maximum.
        let data = resource(|put| {
            put(0x10, &[4]);
            put(0x88, &ptr.to_le_bytes());
            put(0x90, &floats(&[1.0, 1.0, 1.0]));
            put(0xB0, &ptr.to_le_bytes());
            put(0xD0, &u32::MAX.to_le_bytes());
            put(0xD4, &u32::MAX.to_le_bytes());
        });
        assert!(paleto_formats::bounds::parse(&data).is_err());
    }
    // A geometry whose polygons name vertices it does not have.
    let data = resource(|put| {
        put(0x10, &[4]);
        put(0x88, &(SYS + 0x200).to_le_bytes());
        put(0x90, &floats(&[1.0, 1.0, 1.0]));
        put(0xB0, &(SYS + 0x100).to_le_bytes());
        put(0xD0, &3u32.to_le_bytes());
        put(0xD4, &u32::MAX.to_le_bytes());
        for i in 0..32 {
            put(0x200 + i * 16, &[(i % 8) as u8, 0, 0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F]);
        }
    });
    assert!(paleto_formats::bounds::parse(&data).is_err());
    // Every truncation of a valid file.
    let data = resource(|put| {
        composite(put);
        box_bound(put);
        geometry(put);
    });
    for n in 0..data.len() {
        let _ = paleto_formats::bounds::parse(&data[..n]);
    }
}

#[test]
fn nan_and_infinite_values_are_left_out() {
    let data = resource_of(2, |put| {
        // Root composite: a good box, a box with a NaN extent, the good box again moved
        // to infinity, a geometry with a NaN sphere radius and a good triangle, and a
        // geometry whose vertices are all NaN.
        put(0x10, &[10]);
        put(0x70, &(SYS + 0x600).to_le_bytes());
        put(0x78, &(SYS + 0x640).to_le_bytes());
        put(0xA0, &5u16.to_le_bytes());
        for (i, child) in [0xC0u64, 0x140, 0xC0, 0x200, 0x300].into_iter().enumerate() {
            put(0x600 + i * 8, &(SYS + child).to_le_bytes());
            let mut m = IDENTITY;
            if i == 2 {
                m[12] = f32::INFINITY;
            }
            put(0x640 + i * 64, &floats(&m));
        }
        box_bound(put);
        put(0x140 + 0x10, &[3]);
        put(0x140 + 0x20, &floats(&[f32::NAN, 1.0, 1.0]));
        put(0x140 + 0x30, &floats(&[-1.0, -1.0, -1.0]));
        let triangle: Vec<u8> = [0i16, 0, 0, 1, 0, 0, 0, 1, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        for (at, quantum, polys) in [(0x200, 1.0, 0x4A0usize), (0x300, f32::NAN, 0x4E0)] {
            let vertices = polys + 0x20;
            put(at + 0x10, &[4]);
            put(at + 0x88, &(SYS + polys as u64).to_le_bytes());
            put(at + 0x90, &floats(&[quantum, 1.0, 1.0]));
            put(at + 0xB0, &(SYS + vertices as u64).to_le_bytes());
            put(at + 0xD0, &3u32.to_le_bytes());
            put(vertices, &triangle);
        }
        // Geometry A: a sphere around vertex 0 with a NaN radius, then the triangle.
        put(0x200 + 0xD4, &2u32.to_le_bytes());
        put(0x4A0, &[1]);
        put(0x4A4, &f32::NAN.to_le_bytes());
        put(0x4B0 + 6, &[1, 0, 2, 0]);
        // Geometry B: the triangle alone.
        put(0x300 + 0xD4, &1u32.to_le_bytes());
        put(0x4E6, &[1, 0, 2, 0]);
    });
    let drawables = paleto_formats::bounds::parse(&data).unwrap();
    let mesh = &drawables[0].lods[0].1;
    assert_eq!(mesh.positions.len(), 36 + 3);
    assert!(mesh.positions.iter().flatten().all(|v| v.is_finite()));
    assert_eq!(mesh.indices.len(), 36 + 3);
    assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));
    assert_eq!(mesh.parts[0].count, mesh.indices.len());

    // Nothing left to show: an error, as for any unreadable file.
    let sphere = resource(|put| {
        put(0x10, &[0]);
        put(0x14, &f32::NAN.to_le_bytes());
    });
    assert!(paleto_formats::bounds::parse(&sphere).is_err());
    let capsule = resource(|put| {
        put(0x10, &[1]);
        put(0x20, &floats(&[1.0, 1.0, 2.0]));
        put(0x30, &floats(&[-1.0, -1.0, -2.0]));
        put(0x40, &floats(&[f32::INFINITY, 0.0, 0.0]));
    });
    assert!(paleto_formats::bounds::parse(&capsule).is_err());
}
