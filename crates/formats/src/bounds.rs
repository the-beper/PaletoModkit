//! Collision bounds (`.ybn`): primitives and triangle meshes, flattened
//! into one preview mesh. Layouts follow CodeWalker's `Bounds.cs` (MIT).
//!
//! Every bound starts with a 0x70-byte header (type at 0x10, bounding
//! sphere radius at 0x14, box at 0x20/0x30, centres at 0x40/0x50). Composites
//! list child bounds with a transformation each; geometries hold quantised
//! vertices and 16-byte polygons (triangles, boxes, spheres, capsules,
//! cylinders). Anything unreadable is skipped, so one damaged child does not
//! hide the rest.

use crate::Error;
use crate::drawable::{Drawable, Mesh, Part};
use crate::rsc::{Resource, u16_at, u32_at, u64_at};

const ROOT: u64 = 0x5000_0000;
/// Composites nested deeper than this are not followed.
const MAX_DEPTH: usize = 8;
/// Bounds visited and vertices added at most: a damaged composite may point back at itself.
const MAX_VISITS: usize = 100_000;
const MAX_VERTICES: usize = 8_000_000;
/// Children read from one composite, whatever its count says.
const MAX_CHILDREN: usize = 4096;
/// Segments around a round primitive (coarse on purpose).
const SEGMENTS: usize = 12;

type V3 = [f32; 3];

/// The collision bounds of a `.ybn` file as one drawable (a single "Collision" level).
pub fn parse(data: &[u8]) -> Result<Vec<Drawable>, Error> {
    let res = Resource::parse(data)?;
    if res.system.len() < 0x70 {
        return Err(Error::Format("bounds resource is too small".into()));
    }
    let mut mesh = Mesh::default();
    let mut seen = 0;
    let mut budget = MAX_VISITS;
    add_bound(&res, ROOT, &IDENTITY, 0, &mut mesh, &mut seen, &mut budget);
    if seen == 0 {
        return Err(Error::Format("no readable collision bounds".into()));
    }
    mesh.uvs = vec![[0.0, 0.0]; mesh.positions.len()];
    mesh.parts = vec![Part { start: 0, count: mesh.indices.len(), texture: None }];
    Ok(vec![Drawable { name: String::new(), lods: vec![("Collision", mesh)], textures: Vec::new() }])
}

/// Row-vector 4x3 transformation: three rows of axes, then the translation.
type Matrix = [[f32; 3]; 4];
const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]];

fn apply(m: &Matrix, p: V3) -> V3 {
    std::array::from_fn(|c| p[0] * m[0][c] + p[1] * m[1][c] + p[2] * m[2][c] + m[3][c])
}

/// `inner` first, then `outer`.
fn compose(inner: &Matrix, outer: &Matrix) -> Matrix {
    let mut out = IDENTITY;
    for r in 0..3 {
        out[r] = std::array::from_fn(|c| inner[r][0] * outer[0][c] + inner[r][1] * outer[1][c] + inner[r][2] * outer[2][c]);
    }
    out[3] = apply(outer, inner[3]);
    out
}

fn vec3(b: &[u8], at: usize) -> V3 {
    [f32::from_bits(u32_at(b, at)), f32::from_bits(u32_at(b, at + 4)), f32::from_bits(u32_at(b, at + 8))]
}

/// Adds one bound (and its children) to `mesh`; `seen` counts the bounds recognised.
fn add_bound(res: &Resource, ptr: u64, m: &Matrix, depth: usize, mesh: &mut Mesh, seen: &mut usize, budget: &mut usize) {
    if *budget == 0 || mesh.positions.len() > MAX_VERTICES {
        return;
    }
    *budget -= 1;
    let Ok(h) = res.block(ptr, 0x70) else { return };
    let kind = h[0x10];
    match kind {
        0 | 1 | 3 | 13 => {
            let (max, min) = (vec3(h, 0x20), vec3(h, 0x30));
            let center = vec3(h, 0x40);
            let half: V3 = std::array::from_fn(|i| (max[i] - min[i]) / 2.0);
            let added = finite(mesh, |mesh| match kind {
                0 => add_sphere(mesh, m, vec3(h, 0x50), f32::from_bits(u32_at(h, 0x14))),
                3 => add_box(mesh, m, center, half),
                // Capsules and cylinders stand along Z; the radius is the narrower half-extent.
                _ => {
                    let r = half[0].min(half[1]);
                    let reach = if kind == 1 { half[2] - r } else { half[2] };
                    add_tube(mesh, m, [center[0], center[1], center[2] - reach], [center[0], center[1], center[2] + reach], r, kind == 1);
                }
            });
            *seen += added as usize;
        }
        4 | 8 => {
            if let Some(n) = add_geometry(res, ptr, m, mesh) {
                *seen += n.min(1);
            }
        }
        10 if depth < MAX_DEPTH => {
            *seen += 1;
            let Ok(c) = res.block(ptr, 0xB0) else { return };
            let count = (u16_at(c, 0xA0) as usize).min(MAX_CHILDREN);
            let (children, transforms) = (u64_at(c, 0x70), u64_at(c, 0x78));
            let Ok(list) = res.bytes(children, count * 8) else { return };
            for i in 0..list.len() / 8 {
                let child = u64_at(list, i * 8);
                let local = res.block(transforms.wrapping_add(i as u64 * 64), 64).ok().filter(|_| transforms != 0).map(|t| -> Matrix { std::array::from_fn(|r| vec3(t, r * 16)) });
                add_bound(res, child, &local.map_or(*m, |l| compose(&l, m)), depth + 1, mesh, seen, budget);
            }
        }
        _ => {}
    }
}

/// Quantised vertices and polygons of a geometry; `None` when it holds neither.
fn add_geometry(res: &Resource, ptr: u64, m: &Matrix, mesh: &mut Mesh) -> Option<usize> {
    let g = res.block(ptr, 0xD8).ok()?;
    let (polygons, quantum, center, vertices) = (u64_at(g, 0x88), vec3(g, 0x90), vec3(g, 0xA0), u64_at(g, 0xB0));
    let (vertex_count, polygon_count) = (u32_at(g, 0xD0) as usize, u32_at(g, 0xD4) as usize);
    let raw = res.bytes(vertices, vertex_count * 6).ok()?;
    let points: Vec<V3> = raw.as_chunks::<6>().0.iter().map(|v| std::array::from_fn(|c| i16::from_le_bytes([v[c * 2], v[c * 2 + 1]]) as f32 * quantum[c] + center[c])).collect();
    let polys = res.bytes(polygons, polygon_count * 16).ok()?;
    let mut n = 0;
    for poly in polys.as_chunks::<16>().0 {
        // Triangles and boxes list vertices from byte 4; spheres, capsules and cylinders have their radius there.
        let point = |at: usize| points.get(u16_at(poly, at) as usize & 0x7FFF).copied();
        let added = match poly[0] & 7 {
            0 => {
                let (Some(a), Some(b), Some(c)) = (point(4), point(6), point(8)) else { continue };
                finite(mesh, |mesh| add_triangle(mesh, m, a, b, c))
            }
            1 => {
                let Some(c) = point(2) else { continue };
                finite(mesh, |mesh| add_sphere(mesh, m, c, f32::from_bits(u32_at(poly, 4))))
            }
            2 | 4 => {
                let (Some(a), Some(b)) = (point(2), point(8)) else { continue };
                finite(mesh, |mesh| add_tube(mesh, m, a, b, f32::from_bits(u32_at(poly, 4)), poly[0] & 7 == 2))
            }
            3 => {
                let (Some(o), Some(p1), Some(p2), Some(p3)) = (point(4), point(6), point(8), point(10)) else { continue };
                finite(mesh, |mesh| add_tetra_box(mesh, m, [o, p1, p2, p3]))
            }
            _ => continue,
        };
        n += added as usize;
    }
    (n > 0).then_some(n)
}

/// Adds one primitive with `add`, or nothing when a damaged value (a radius, an extent,
/// a vertex, a transformation) makes any of its points NaN or infinite.
fn finite(mesh: &mut Mesh, add: impl FnOnce(&mut Mesh)) -> bool {
    let (positions, indices) = (mesh.positions.len(), mesh.indices.len());
    add(mesh);
    if mesh.positions[positions..].iter().flatten().all(|v| v.is_finite()) {
        return true;
    }
    mesh.positions.truncate(positions);
    mesh.indices.truncate(indices);
    false
}

fn add_triangle(mesh: &mut Mesh, m: &Matrix, a: V3, b: V3, c: V3) {
    let base = mesh.positions.len() as u32;
    mesh.positions.extend([a, b, c].map(|p| apply(m, p)));
    mesh.indices.extend([base, base + 1, base + 2]);
}

fn add_quad(mesh: &mut Mesh, m: &Matrix, [a, b, c, d]: [V3; 4]) {
    add_triangle(mesh, m, a, b, c);
    add_triangle(mesh, m, a, c, d);
}

/// A box given as four of its corners, none sharing an edge (the polygon layout).
fn add_tetra_box(mesh: &mut Mesh, m: &Matrix, [v0, v1, v2, v3]: [V3; 4]) {
    let d = |p: V3| -> V3 { std::array::from_fn(|i| p[i] - v0[i]) };
    let (e1, e2, e3) = (d(v1), d(v2), d(v3));
    let edge = |x: V3, y: V3, z: V3| -> V3 { std::array::from_fn(|i| (x[i] + y[i] - z[i]) / 2.0) };
    add_corners(mesh, m, v0, edge(e2, e3, e1), edge(e1, e3, e2), edge(e1, e2, e3));
}

/// A box from a corner and its three edge vectors.
fn add_corners(mesh: &mut Mesh, m: &Matrix, o: V3, a: V3, b: V3, c: V3) {
    let at = |x: f32, y: f32, z: f32| -> V3 { std::array::from_fn(|i| o[i] + a[i] * x + b[i] * y + c[i] * z) };
    let p = [at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0), at(1.0, 1.0, 0.0), at(0.0, 1.0, 0.0), at(0.0, 0.0, 1.0), at(1.0, 0.0, 1.0), at(1.0, 1.0, 1.0), at(0.0, 1.0, 1.0)];
    for face in [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]] {
        add_quad(mesh, m, face.map(|i| p[i]));
    }
}

fn add_box(mesh: &mut Mesh, m: &Matrix, c: V3, half: V3) {
    let o = std::array::from_fn(|i| c[i] - half[i]);
    add_corners(mesh, m, o, [half[0] * 2.0, 0.0, 0.0], [0.0, half[1] * 2.0, 0.0], [0.0, 0.0, half[2] * 2.0]);
}

/// Latitude/longitude sphere, a few rings only.
fn add_sphere(mesh: &mut Mesh, m: &Matrix, c: V3, r: f32) {
    let rings = SEGMENTS / 2;
    let base = mesh.positions.len() as u32;
    for i in 0..=rings {
        let phi = std::f32::consts::PI * i as f32 / rings as f32;
        for j in 0..SEGMENTS {
            let theta = std::f32::consts::TAU * j as f32 / SEGMENTS as f32;
            mesh.positions.push(apply(m, [c[0] + r * phi.sin() * theta.cos(), c[1] + r * phi.sin() * theta.sin(), c[2] + r * phi.cos()]));
        }
    }
    for i in 0..rings as u32 {
        for j in 0..SEGMENTS as u32 {
            let (a, b) = (base + i * SEGMENTS as u32 + j, base + i * SEGMENTS as u32 + (j + 1) % SEGMENTS as u32);
            let (c, d) = (a + SEGMENTS as u32, b + SEGMENTS as u32);
            mesh.indices.extend([a, c, b, b, c, d]);
        }
    }
}

/// A cylinder between two points, with a sphere at each end for a capsule.
fn add_tube(mesh: &mut Mesh, m: &Matrix, a: V3, b: V3, r: f32, capsule: bool) {
    let axis: V3 = std::array::from_fn(|i| b[i] - a[i]);
    let len = axis.iter().map(|x| x * x).sum::<f32>().sqrt();
    if len < 1e-6 {
        return add_sphere(mesh, m, a, r);
    }
    let w = axis.map(|x| x / len);
    let helper = if w[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let cross = |p: V3, q: V3| -> V3 { [p[1] * q[2] - p[2] * q[1], p[2] * q[0] - p[0] * q[2], p[0] * q[1] - p[1] * q[0]] };
    let u = cross(w, helper);
    let un = u.iter().map(|x| x * x).sum::<f32>().sqrt();
    let u = u.map(|x| x / un);
    let v = cross(w, u);
    let base = mesh.positions.len() as u32;
    for end in [a, b] {
        for j in 0..SEGMENTS {
            let t = std::f32::consts::TAU * j as f32 / SEGMENTS as f32;
            mesh.positions.push(apply(m, std::array::from_fn(|i| end[i] + r * (u[i] * t.cos() + v[i] * t.sin()))));
        }
    }
    let n = SEGMENTS as u32;
    for j in 0..n {
        let k = (j + 1) % n;
        mesh.indices.extend([base + j, base + k, base + n + j, base + k, base + n + k, base + n + j]);
    }
    if capsule {
        add_sphere(mesh, m, a, r);
        add_sphere(mesh, m, b, r);
    } else {
        // Flat caps: a fan around the first vertex of each ring.
        for ring in [0, n] {
            for j in 1..n - 1 {
                mesh.indices.extend([base + ring, base + ring + j, base + ring + j + 1]);
            }
        }
    }
}
