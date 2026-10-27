//! Navigation meshes (`.ynv`): the walkable polygons of one 150 m sector of the
//! map, shown as a mesh of triangle fans.
//!
//! Layout (after CodeWalker's `NavMesh`; checked on Enhanced files, Legacy ones
//! are assumed to match): the root holds pointers to lists at 0x70 (vertices),
//! 0x80 (vertex indices) and 0x118 (polygons), and to the sector tree root at
//! 0x120, whose first two vectors are the bounds of the sector. A list is split
//! in parts: total count at +8, pointer to the parts at +0x10, part count at
//! +0x20; a part is a pointer and an item count (16 bytes). Vertices are three
//! u16 spread over the bounds; polygons take 48 bytes: the number of indices
//! in bits 5.. of the u16 at +2, the first index at +4.

use crate::drawable::{Drawable, Mesh, Part};
use crate::rsc::{Resource, u16_at, u32_at, u64_at};
use crate::Error;

const ROOT: u64 = 0x5000_0000;
const VERTICES: usize = 0x70;
const INDICES: usize = 0x80;
const POLYGONS: usize = 0x118;
const SECTOR: usize = 0x120;
const POLYGON_SIZE: usize = 48;
const MAX_PARTS: usize = 4096;

/// The navmesh of a `.ynv` file as one drawable with a single "Navmesh" level.
pub fn parse(data: &[u8]) -> Result<Vec<Drawable>, Error> {
    let res = Resource::parse(data)?;
    let root = res.block(ROOT, 0x130)?;
    let sector = res.block(u64_at(root, SECTOR), 0x20)?;
    let f = |at: usize| f32::from_le_bytes(sector[at..at + 4].try_into().unwrap());
    let (min, max) = ([f(0), f(4), f(8)], [f(0x10), f(0x14), f(0x18)]);
    if (0..3).any(|i| !(min[i].is_finite() && max[i].is_finite() && max[i] >= min[i])) {
        return Err(Error::Format("navmesh without valid bounds".into()));
    }
    let vertices = list(&res, u64_at(root, VERTICES), 6)?;
    let indices = list(&res, u64_at(root, INDICES), 2)?;
    let polygons = list(&res, u64_at(root, POLYGONS), POLYGON_SIZE)?;
    let positions: Vec<[f32; 3]> = vertices.as_chunks::<6>().0.iter().map(|v| std::array::from_fn(|i| min[i] + (max[i] - min[i]) * u16_at(v, i * 2) as f32 / 65535.0)).collect();
    let vertex_count = positions.len();
    let mut mesh = Mesh { uvs: vec![[0.0; 2]; vertex_count], positions, ..Mesh::default() };
    for poly in polygons.as_chunks::<POLYGON_SIZE>().0 {
        let (count, first) = ((u16_at(poly, 2) >> 5) as usize, u16_at(poly, 4) as usize);
        let Some(ids) = indices.get(first * 2..(first + count) * 2).filter(|_| count >= 3) else { continue };
        let ids: Vec<u32> = ids.as_chunks::<2>().0.iter().map(|i| u16_at(i, 0) as u32).collect();
        if ids.iter().any(|&i| i as usize >= vertex_count) {
            continue;
        }
        for k in 1..count - 1 {
            mesh.indices.extend([ids[0], ids[k], ids[k + 1]]);
        }
    }
    if mesh.indices.is_empty() {
        return Err(Error::Format("navmesh without polygons".into()));
    }
    mesh.parts.push(Part { start: 0, count: mesh.indices.len(), texture: None });
    Ok(vec![Drawable { name: String::new(), lods: vec![("Navmesh", mesh)], textures: Vec::new() }])
}

/// The items of a list, its parts joined (`size` bytes each).
fn list(res: &Resource, ptr: u64, size: usize) -> Result<Vec<u8>, Error> {
    let head = res.block(ptr, 0x28)?;
    let (total, parts, n) = (u32_at(head, 8) as usize, u64_at(head, 0x10), u32_at(head, 0x20) as usize);
    if n > MAX_PARTS {
        return Err(Error::Format("navmesh list with too many parts".into()));
    }
    let table = res.block(parts, n * 16)?;
    let mut out = Vec::new();
    for part in table.as_chunks::<16>().0 {
        let items = res.block(u64_at(part, 0), u32_at(part, 8) as usize * size)?;
        // Parts of a damaged file may all point at the same large block.
        if out.len() + items.len() > total * size {
            return Err(Error::Format("navmesh list parts do not add up".into()));
        }
        out.extend_from_slice(items);
    }
    if out.len() != total * size {
        return Err(Error::Format("navmesh list parts do not add up".into()));
    }
    Ok(out)
}
