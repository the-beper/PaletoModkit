//! Models (`.ydr` drawables, `.ydd` drawable dictionaries, `.yft`
//! fragments): their meshes, placed by their skeleton, with texture
//! coordinates and diffuse textures, for previews. Legacy and Enhanced
//! layouts, following CodeWalker's `Drawable.cs` and `Frag.cs` (MIT).
//!
//! Textures come from the model's own dictionary or from dictionaries the
//! caller passes (such as the `.ytd` of the same name), named by the
//! shader's diffuse parameter (Enhanced shaders keep their parameters in
//! another layout, read too).

use crate::Error;
use crate::hash::{joaat, joaat_lower};
use crate::rsc::{Resource, u16_at, u32_at, u64_at};
use crate::ytd::TextureDictionary;

const ROOT: u64 = 0x5000_0000;
/// Textures are decoded at the first mip level this size or smaller.
const PREVIEW_SIZE: u32 = 512;

/// Triangles of one level of detail, all models and geometries together.
#[derive(Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates of each position ((0, 0) when there are none).
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Runs of `indices` sharing a texture.
    pub parts: Vec<Part>,
}

pub struct Part {
    pub start: usize,
    pub count: usize,
    /// Index in [`Drawable::textures`].
    pub texture: Option<usize>,
}

/// A decoded texture: RGBA pixels.
pub struct Picture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Drawable {
    /// Name in a dictionary (hash as hex when unknown); empty otherwise.
    pub name: String,
    /// Levels of detail present, from High to Very low.
    pub lods: Vec<(&'static str, Mesh)>,
    pub textures: Vec<Picture>,
}

/// The drawables of a model file; `extension` tells `.ydr`, `.ydd` and `.yft`
/// apart. `dictionaries` are searched for textures the model does not hold.
pub fn parse(data: &[u8], extension: &str, dictionaries: &[TextureDictionary]) -> Result<Vec<Drawable>, Error> {
    let res = Resource::parse(data)?;
    let root = res.block(ROOT, 0x40)?;
    let ext = extension.to_ascii_lowercase();
    match ext.as_str() {
        "ydr" => Ok(vec![drawable(&res, ROOT, String::new(), dictionaries, &[])?]),
        "yft" => {
            let ptr = u64_at(root, 0x30);
            if ptr == 0 {
                return Err(Error::Format("fragment without a drawable".into()));
            }
            let children = fragment_children(&res, res.block(ROOT, 0x100)?).unwrap_or_default();
            Ok(vec![drawable(&res, ptr, String::new(), dictionaries, &children)?])
        }
        "ydd" => {
            let (hashes, count) = (u64_at(root, 0x20), u16_at(root, 0x28) as usize);
            let (list, drawables) = (u64_at(root, 0x30), u16_at(root, 0x38) as usize);
            let hashes = if count == 0 { &[][..] } else { res.block(hashes, count * 4)? };
            let pointers = if drawables == 0 { &[][..] } else { res.block(list, drawables * 8)? };
            pointers
                .as_chunks::<8>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, p)| u64::from_le_bytes(**p) != 0)
                .map(|(i, p)| {
                    let name = hashes.get(i * 4..i * 4 + 4).map_or(String::new(), |h| format!("{:08X}", u32_at(h, 0)));
                    drawable(&res, u64::from_le_bytes(*p), name, dictionaries, &[])
                })
                .collect()
        }
        other => Err(Error::Unsupported(format!(".{other} models"))),
    }
}

/// An affine transform: 3x3 matrix (rows) and translation.
#[derive(Clone, Copy)]
struct Transform {
    m: [[f32; 3]; 3],
    t: [f32; 3],
}

impl Transform {
    fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|r| self.m[r][0] * p[0] + self.m[r][1] * p[1] + self.m[r][2] * p[2] + self.t[r])
    }

    /// `self` after `inner`.
    fn then(&self, inner: &Transform) -> Transform {
        let m = std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| self.m[r][k] * inner.m[k][c]).sum()));
        Transform { m, t: self.apply(inner.t) }
    }

    /// Scale, then rotation (a quaternion x, y, z, w), then translation.
    fn from_parts(q: [f32; 4], t: [f32; 3], s: [f32; 3]) -> Transform {
        let [x, y, z, w] = q;
        let r = [
            [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
            [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
            [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
        ];
        Transform { m: std::array::from_fn(|row| std::array::from_fn(|c| r[row][c] * s[c])), t }
    }
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Absolute transforms of the skeleton's bones, from their local ones.
fn bones(res: &Resource, skeleton: u64) -> Result<Vec<Transform>, Error> {
    if skeleton == 0 {
        return Ok(Vec::new());
    }
    let s = res.block(skeleton, 0x70)?;
    let (list, count) = (u64_at(s, 0x20), u16_at(s, 0x5E) as usize);
    if list == 0 || count == 0 {
        return Ok(Vec::new());
    }
    let raw = res.block(list, count * 80)?;
    let mut out: Vec<Transform> = Vec::with_capacity(count);
    for (i, b) in raw.as_chunks::<80>().0.iter().enumerate() {
        let q = [f32_at(b, 0), f32_at(b, 4), f32_at(b, 8), f32_at(b, 12)];
        let local = Transform::from_parts(q, [f32_at(b, 0x10), f32_at(b, 0x14), f32_at(b, 0x18)], [f32_at(b, 0x20), f32_at(b, 0x24), f32_at(b, 0x28)]);
        let parent = i16::from_le_bytes([b[0x32], b[0x33]]);
        // Parents come before their children.
        let abs = match usize::try_from(parent).ok().filter(|&p| p < i) {
            Some(p) => out[p].then(&local),
            None => local,
        };
        out.push(abs);
    }
    Ok(out)
}

/// Diffuse texture names of the shaders, by shader index (Legacy layout).
fn shader_textures(res: &Resource, group: u64) -> Vec<Option<String>> {
    let read = || -> Result<Vec<Option<String>>, Error> {
        let g = res.block(group, 0x40)?;
        let (list, count) = (u64_at(g, 0x10), u16_at(g, 0x18) as usize);
        if list == 0 || count == 0 {
            return Ok(Vec::new());
        }
        let diffuse = [joaat(b"DiffuseSampler"), joaat_lower("DiffuseSampler")];
        let mut names = Vec::with_capacity(count);
        for p in res.block(list, count * 8)?.as_chunks::<8>().0 {
            if is_enhanced(res.version) {
                names.push(gen9_texture(res, u64::from_le_bytes(*p), &[joaat_lower("diffusetex")])?);
                continue;
            }
            let shader = res.block(u64::from_le_bytes(*p), 0x30)?;
            let (params, n) = (u64_at(shader, 0), shader[0x10] as usize);
            if params == 0 || n == 0 {
                names.push(None);
                continue;
            }
            // Parameters (type 0 = texture, else that many vectors), their data, then their name hashes.
            let entries = res.block(params, n * 16)?;
            let data: usize = entries.as_chunks::<16>().0.iter().map(|e| 16 * e[0] as usize).sum();
            let hashes = res.block(params + (n * 16 + data) as u64, n * 4)?;
            let found = entries.as_chunks::<16>().0.iter().zip(hashes.as_chunks::<4>().0).find(|(e, h)| e[0] == 0 && diffuse.contains(&u32::from_le_bytes(**h)));
            names.push(match found {
                Some((e, _)) if u64_at(e, 8) != 0 => {
                    let texture = res.block(u64_at(e, 8), 0x30)?;
                    let name = u64_at(texture, 0x28);
                    (name != 0).then(|| res.string(name)).transpose()?
                }
                _ => None,
            });
        }
        Ok(names)
    };
    read().unwrap_or_default()
}

/// The name of a gen9 (Enhanced) shader's texture parameter, named like the
/// legacy one in lowercase with "tex" for "Sampler" ("DiffuseSampler" →
/// "diffusetex"). Its parameters
/// are described apart: a count header (buffers, textures, unknowns, samplers,
/// parameters), then per parameter its name hash and a word whose low 2 bits
/// are the type (0 = texture) and the next 8 the index into the texture list.
fn gen9_texture(res: &Resource, shader: u64, wanted: &[u32]) -> Result<Option<String>, Error> {
    let shader = res.block(shader, 0x40)?;
    let (textures, infos) = (u64_at(shader, 0x10), u64_at(shader, 0x20));
    if textures == 0 || infos == 0 {
        return Ok(None);
    }
    let header = res.block(infos, 8)?;
    let (texture_count, count) = (header[1] as usize, header[4] as usize);
    let params = res.block(infos + 8, count * 8)?;
    let list = res.block(textures, texture_count * 8)?;
    for param in params.as_chunks::<8>().0 {
        let (hash, data) = (u32_at(param, 0), u32_at(param, 4));
        let index = (data >> 2 & 0xFF) as usize;
        if data & 3 != 0 || !wanted.contains(&hash) {
            continue;
        }
        let Some(texture) = list.get(index * 8..index * 8 + 8).map(|b| u64_at(b, 0)).filter(|&t| t != 0) else { return Ok(None) };
        let texture = res.block(texture, 0x30)?;
        let name = u64_at(texture, 0x28);
        return (name != 0).then(|| res.string(name)).transpose();
    }
    Ok(None)
}

const LOD_NAMES: [&str; 4] = ["High", "Medium", "Low", "Very low"];

/// Textures found so far for a model, and where to look for more.
struct TextureSource<'a> {
    pictures: Vec<Picture>,
    /// Dictionaries embedded in the drawables read so far.
    own: Vec<TextureDictionary>,
    external: &'a [TextureDictionary],
}

impl TextureSource<'_> {
    fn index(&mut self, name: &str) -> Option<usize> {
        if let Some(i) = self.pictures.iter().position(|t| t.name.eq_ignore_ascii_case(name)) {
            return Some(i);
        }
        let tex = self.own.iter().chain(self.external).flat_map(|d| &d.textures).find(|t| t.name.eq_ignore_ascii_case(name))?;
        let level = (0..tex.levels).find(|&l| (tex.width >> l).max(tex.height >> l) <= PREVIEW_SIZE).unwrap_or(tex.levels - 1);
        let (width, height, rgba) = tex.rgba(level)?;
        self.pictures.push(Picture { name: tex.name.clone(), width, height, rgba });
        Some(self.pictures.len() - 1)
    }
}

/// A drawable and, for fragments, the drawables of its parts placed where
/// they belong (vehicle wheels, doors...).
fn drawable(res: &Resource, ptr: u64, name: String, dictionaries: &[TextureDictionary], children: &[(u64, Transform)]) -> Result<Drawable, Error> {
    let mut textures = TextureSource { pictures: Vec::new(), own: Vec::new(), external: dictionaries };
    let mut lods: Vec<(&'static str, Mesh)> = LOD_NAMES.into_iter().map(|n| (n, Mesh::default())).collect();
    add_drawable(res, ptr, None, &mut lods, &mut textures)?;
    for (child, place) in children {
        // A part that cannot be read is left out rather than failing the model.
        let _ = add_drawable(res, *child, Some(place), &mut lods, &mut textures);
    }
    lods.retain(|(_, m)| !m.indices.is_empty());
    Ok(Drawable { name, lods, textures: textures.pictures })
}

/// Adds a drawable's meshes to `lods`. Models follow their bone, or
/// `place` for fragment parts.
fn add_drawable(res: &Resource, ptr: u64, place: Option<&Transform>, lods: &mut [(&'static str, Mesh)], textures: &mut TextureSource) -> Result<(), Error> {
    let base = res.block(ptr, 0xA8)?;
    let enhanced = is_enhanced(res.version);
    let bones = if place.is_some() { Vec::new() } else { bones(res, u64_at(base, 0x18)).unwrap_or_default() };
    let group = u64_at(base, 0x10);
    let shader_names = if group == 0 { Vec::new() } else { shader_textures(res, group) };
    if group != 0
        && let Some(dict) = res.block(group, 0x10).ok().map(|g| u64_at(g, 8)).filter(|&d| d != 0)
        && let Ok(dict) = TextureDictionary::read(res, dict, enhanced)
    {
        textures.own.push(dict);
    }

    for (i, (_, mesh)) in lods.iter_mut().enumerate() {
        let list = u64_at(base, 0x50 + i * 8);
        if list == 0 {
            continue;
        }
        // A pointer list: (pointer, count, capacity).
        let head = res.block(list, 16)?;
        let (models, count) = (u64_at(head, 0), u16_at(head, 8) as usize);
        if count == 0 || models == 0 {
            continue;
        }
        for m in res.block(models, count * 8)?.as_chunks::<8>().0 {
            let model = res.block(u64::from_le_bytes(*m), 0x30)?;
            let (geoms, count) = (u64_at(model, 8), u16_at(model, 0x10) as usize);
            if count == 0 || geoms == 0 {
                continue;
            }
            // Rigid models follow their bone; skinned ones are already in place.
            let binding = u32_at(model, 0x28);
            let (bone, skinned) = ((binding >> 24) as usize, (binding >> 8) & 0xFF != 0);
            let transform = place.copied().or_else(|| if skinned { None } else { bones.get(bone).copied() });
            let mapping = u64_at(model, 0x20);
            let shaders = if mapping == 0 { &[][..] } else { res.block(mapping, count * 2).unwrap_or_default() };
            for (gi, g) in res.block(geoms, count * 8)?.as_chunks::<8>().0.iter().enumerate() {
                let start = mesh.indices.len();
                geometry(res, u64::from_le_bytes(*g), mesh, transform.as_ref())?;
                let shader = shaders.get(gi * 2..gi * 2 + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
                let texture = shader.and_then(|s| shader_names.get(s)).cloned().flatten().and_then(|n| textures.index(&n));
                let part = Part { start, count: mesh.indices.len() - start, texture };
                if part.count > 0 {
                    mesh.parts.push(part);
                }
            }
        }
    }
    Ok(())
}

/// Bone tags of vehicle wheels: front ones, then the others; right-hand ones are mirrored.
const FRONT_WHEELS: [u16; 2] = [27922, 26418];
const OTHER_WHEELS: [u16; 8] = [29921, 29922, 29923, 27902, 5857, 5858, 5859, 26398];
const RIGHT_WHEELS: [u16; 5] = [26418, 5857, 5858, 5859, 26398];

/// The drawables of a fragment's parts (its first physics level of detail),
/// with where each goes. Wheels without a drawable reuse the front or rear
/// one, as the game does.
fn fragment_children(res: &Resource, fragment: &[u8]) -> Result<Vec<(u64, Transform)>, Error> {
    let group = u64_at(fragment, 0xF0);
    if group == 0 {
        return Ok(Vec::new());
    }
    let lod = u64_at(res.block(group, 0x30)?, 0x10);
    if lod == 0 {
        return Ok(Vec::new());
    }
    let lod = res.block(lod, 0x130)?;
    let offset = [f32_at(lod, 0x30), f32_at(lod, 0x34), f32_at(lod, 0x38)];
    let (list, count, transforms) = (u64_at(lod, 0xD0), lod[0x11D] as usize, u64_at(lod, 0x100));
    if list == 0 || count == 0 || transforms == 0 {
        return Ok(Vec::new());
    }
    let matrices = res.block(transforms, 0x20)?;
    let matrix_count = u32_at(matrices, 0x10) as usize;
    let matrices = res.block(transforms + 0x20, matrix_count * 64)?;
    let mut children = Vec::new();
    for p in res.block(list, count * 8)?.as_chunks::<8>().0 {
        let child = res.block(u64::from_le_bytes(*p), 0xB0)?;
        // A drawable without models counts as none (wheels other than the modelled ones).
        let drawable = Some(u64_at(child, 0xA0)).filter(|&d| d != 0 && has_models(res, d)).unwrap_or(0);
        children.push((u16_at(child, 0x12), drawable));
    }
    let wheel = |tags: &[u16]| children.iter().find(|(t, d)| tags.contains(t) && *d != 0).map(|c| c.1);
    let (front, rear) = (wheel(&FRONT_WHEELS), wheel(&OTHER_WHEELS));
    let mut out = Vec::new();
    for (i, &(tag, drawable)) in children.iter().enumerate() {
        let drawable = match drawable {
            // Each falls back on the other, as when a model has one wheel only.
            0 if FRONT_WHEELS.contains(&tag) => front.or(rear).unwrap_or(0),
            0 if OTHER_WHEELS.contains(&tag) => rear.or(front).unwrap_or(0),
            d => d,
        };
        let Some(m) = matrices.get(i * 64..i * 64 + 64).filter(|_| drawable != 0) else { continue };
        // A row-vector matrix: the 3x3 part transposed, translation in the fourth row.
        let at = |r: usize, c: usize| f32_at(m, (r * 4 + c) * 4);
        let mut t = Transform { m: std::array::from_fn(|r| std::array::from_fn(|c| at(c, r))), t: [at(3, 0) + offset[0], at(3, 1) + offset[1], at(3, 2) + offset[2]] };
        if RIGHT_WHEELS.contains(&tag) {
            t.m = [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]];
        }
        out.push((drawable, t));
    }
    Ok(out)
}

/// Whether a drawable has models at any level of detail.
fn has_models(res: &Resource, ptr: u64) -> bool {
    let Ok(base) = res.block(ptr, 0xA8) else { return false };
    (0..4).any(|i| {
        let list = u64_at(base, 0x50 + i * 8);
        list != 0 && res.block(list, 16).is_ok_and(|h| u64_at(h, 0) != 0 && u16_at(h, 8) > 0)
    })
}

/// Bytes of each vertex component type (CodeWalker's `VertexComponentType`).
const COMPONENT_SIZE: [usize; 16] = [0, 4, 4, 8, 0, 8, 12, 16, 4, 4, 4, 0, 0, 0, 0, 0];
const FLOAT3: u64 = 6;
const HALF2: u64 = 1;
const FLOAT2: u64 = 5;
/// The first texture coordinates' component.
const TEXCOORD0: usize = 6;

/// Legacy models are versions 165 (`.ydr`, `.ydd`) and 162 (`.yft`), and 160 for
/// a few fragments (66 in the Legacy game); the Enhanced edition uses others
/// (154, 159, 171) with gen9 vertex buffers.
pub(crate) fn is_enhanced(version: u32) -> bool {
    !matches!(version, 160 | 162 | 165)
}

/// Gen9 vertex element formats: R32G32B32_FLOAT, R16G16_FLOAT, R32G32 (floats).
const G9_FLOAT3: u8 = 6;
const G9_HALF2: u8 = 34;
const G9_FLOAT2: u8 = 16;
/// Gen9 element of the first texture coordinates.
const G9_TEXCOORD0: usize = 28;

/// Where texture coordinates are in a vertex, and whether they are halves.
type UvLayout = Option<(usize, bool)>;

pub(crate) fn half(bits: u16) -> f32 {
    let (sign, exp, frac) = ((bits >> 15) as u32, ((bits >> 10) & 0x1F) as i32, (bits & 0x3FF) as f32);
    let v = match exp {
        0 => frac / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    };
    if sign == 1 { -v } else { v }
}

fn geometry(res: &Resource, ptr: u64, mesh: &mut Mesh, transform: Option<&Transform>) -> Result<(), Error> {
    let geom = res.block(ptr, 0x98)?;
    let (vb, ib) = (u64_at(geom, 0x18), u64_at(geom, 0x38));
    if vb == 0 || ib == 0 {
        return Ok(());
    }
    let vb = res.block(vb, 0x40)?;
    let unsupported = || Error::Unsupported("this vertex layout".into());
    // Vertex size, data, count, where positions are in a vertex, and texture coordinates.
    let (stride, data, count, position, uv): (usize, u64, usize, usize, UvLayout) = if is_enhanced(res.version) {
        let (count, stride, data, info) = (u32_at(vb, 8) as usize, u16_at(vb, 0xC) as usize, u64_at(vb, 0x18), u64_at(vb, 0x38));
        if info == 0 || data == 0 || count == 0 {
            return Ok(());
        }
        // 52 element offsets (u32), sizes and formats (bytes); positions are element 0.
        let decl = res.block(info, 52 * 6)?;
        let format = |e: usize| decl[52 * 5 + e];
        if format(0) != G9_FLOAT3 {
            return Err(unsupported());
        }
        let uv = match format(G9_TEXCOORD0) {
            G9_HALF2 => Some((u32_at(decl, G9_TEXCOORD0 * 4) as usize, true)),
            G9_FLOAT2 => Some((u32_at(decl, G9_TEXCOORD0 * 4) as usize, false)),
            _ => None,
        };
        (stride, data, count, u32_at(decl, 0) as usize, uv)
    } else {
        let (stride, data, count, info) = (u16_at(vb, 8) as usize, u64_at(vb, 0x10), u32_at(vb, 0x18) as usize, u64_at(vb, 0x30));
        if info == 0 || data == 0 || count == 0 {
            return Ok(());
        }
        let decl = res.block(info, 16)?;
        let (flags, types) = (u32_at(decl, 0), u64_at(decl, 8));
        let kind = |i: usize| (types >> (i * 4)) & 0xF;
        // Positions: the first component, three floats.
        if flags & 1 == 0 || kind(0) != FLOAT3 {
            return Err(unsupported());
        }
        let present = |i: usize| flags & (1 << i) != 0;
        let used: usize = (0..16).filter(|&i| present(i)).map(|i| COMPONENT_SIZE[kind(i) as usize]).sum();
        if used > stride {
            return Err(unsupported());
        }
        let uv_at: usize = (0..TEXCOORD0).filter(|&i| present(i)).map(|i| COMPONENT_SIZE[kind(i) as usize]).sum();
        let uv = match (present(TEXCOORD0), kind(TEXCOORD0)) {
            (true, HALF2) => Some((uv_at, true)),
            (true, FLOAT2) => Some((uv_at, false)),
            _ => None,
        };
        (stride, data, count, 0, uv)
    };
    if stride < position + 12 || uv.is_some_and(|(at, h)| at + if h { 4 } else { 8 } > stride) {
        return Err(unsupported());
    }
    let vertices = res.block(data, stride * count)?;
    let first = mesh.positions.len() as u32;
    let vertex_count = count as u32;
    for v in vertices.chunks_exact(stride) {
        let p = [f32_at(v, position), f32_at(v, position + 4), f32_at(v, position + 8)];
        mesh.positions.push(transform.map_or(p, |t| t.apply(p)));
        mesh.uvs.push(match uv {
            Some((at, true)) => [half(u16::from_le_bytes([v[at], v[at + 1]])), half(u16::from_le_bytes([v[at + 2], v[at + 3]]))],
            Some((at, false)) => [f32_at(v, at), f32_at(v, at + 4)],
            None => [0.0, 0.0],
        });
    }

    // Index count, then the indices' pointer (after more fields in gen9).
    let ib = res.block(ib, 0x20)?;
    let indices = u64_at(ib, if is_enhanced(res.version) { 0x18 } else { 0x10 });
    let count = u32_at(ib, 8) as usize;
    if indices == 0 || count == 0 {
        return Ok(());
    }
    let raw = res.block(indices, count * 2)?;
    for tri in raw.as_chunks::<2>().0.as_chunks::<3>().0 {
        let idx: [u32; 3] = std::array::from_fn(|k| u16::from_le_bytes(tri[k]) as u32);
        if idx.iter().all(|&i| i < vertex_count) {
            mesh.indices.extend(idx.map(|i| first + i));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_enhanced_versions() {
        assert!([160, 162, 165].into_iter().all(|v| !is_enhanced(v)));
        assert!([154, 159, 171].into_iter().all(is_enhanced));
    }

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3C00), 1.0);
        assert_eq!(half(0xC000), -2.0);
        assert_eq!(half(0x3800), 0.5);
        assert_eq!(half(0), 0.0);
    }

    #[test]
    fn bone_transforms() {
        // 90° about Z, then moved by (1, 0, 0): (1, 0, 0) goes to (1, 1, 0).
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let t = Transform::from_parts([0.0, 0.0, s, s], [1.0, 0.0, 0.0], [1.0; 3]);
        let p = t.apply([1.0, 0.0, 0.0]);
        assert!((p[0] - 1.0).abs() < 1e-5 && (p[1] - 1.0).abs() < 1e-5 && p[2].abs() < 1e-5, "{p:?}");
        let twice = t.then(&t).apply([0.0; 3]);
        assert!((twice[0] - 1.0).abs() < 1e-5 && (twice[1] - 1.0).abs() < 1e-5, "{twice:?}");
    }
}
