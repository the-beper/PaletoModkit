//! Models (`.ydr`, `.ydd`) as CodeWalker XML: the text Sollumz and CodeWalker
//! import, with the textures it names as DDS files. Layouts follow CodeWalker's
//! `Drawable.cs` (MIT); the XML follows its `WriteXml` methods.
//!
//! Not written: collision bounds, joints and lights. Vertices are written in
//! the GTAV1 layout whatever the source layout was; the Enhanced shaders keep
//! only their textures, with the common sampler names.

use std::collections::BTreeMap;
use std::fmt::Display;

use crate::Error;
use crate::drawable::{half, is_enhanced};
use crate::hash::joaat_lower;
use crate::names::Names;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};
use crate::ytd::{Format, Texture, TextureDictionary};

const ROOT: u64 = 0x5000_0000;
/// Element names of the vertex semantics, by bit (CodeWalker's `VertexSemantics`).
const SEMANTICS: [&str; 16] = [
    "Position", "BlendWeights", "BlendIndices", "Normal", "Colour0", "Colour1", "TexCoord0", "TexCoord1", "TexCoord2", "TexCoord3", "TexCoord4", "TexCoord5", "TexCoord6", "TexCoord7", "Tangent", "Binormal",
];
/// Gen9 vertex element slots (their byte offset and format are at `slot * 4` and `52 * 5 + slot`
/// of the declaration), with the GTAV1 bit each becomes.
const GEN9_SLOTS: [(usize, usize); 13] = [(0, 0), (4, 3), (8, 14), (16, 1), (20, 2), (24, 4), (25, 5), (28, 6), (29, 7), (30, 8), (31, 9), (32, 10), (33, 11)];
/// Vertices, each a list of its components' values in bit order.
type Vertices = Vec<Vec<Vec<f32>>>;
/// Bone flags by bit (CodeWalker's `EBoneFlags`).
const BONE_FLAGS: [&str; 16] = ["RotX", "RotY", "RotZ", "LimitRotation", "TransX", "TransY", "TransZ", "LimitTranslation", "ScaleX", "ScaleY", "ScaleZ", "LimitScale", "Unk0", "Unk1", "Unk2", "Unk3"];
/// Sampler names of the Enhanced shaders' common textures, as they hash in the game's dictionaries.
const GEN9_SAMPLERS: [(&str, &str); 5] = [("diffusetex", "DiffuseSampler"), ("bumptex", "BumpSampler"), ("speculartex", "SpecSampler"), ("detailtex", "DetailSampler"), ("tintpalettetex", "TintPaletteSampler")];

/// Result of an export: the XML, and the DDS files it names (name, contents).
pub struct Export {
    pub xml: String,
    pub textures: Vec<(String, Vec<u8>)>,
}

/// Exports a model. `name` is the drawable's name (a `.ydr`'s file name);
/// `dictionaries` are searched for the textures its shaders name but do not hold.
pub fn export(data: &[u8], extension: &str, name: &str, dictionaries: &[TextureDictionary]) -> Result<Export, Error> {
    let res = Resource::parse(data)?;
    let names = Names::new();
    let drawables = match extension.to_ascii_lowercase().as_str() {
        "ydr" => vec![read_drawable(&res, ROOT, name.to_string(), &names)?],
        "ydd" => read_dictionary(&res, &names)?,
        other => return Err(Error::Unsupported(format!(".{other} models"))),
    };
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for d in &drawables {
        for tex in d.embedded.iter().flatten() {
            files.entry(tex.file_name(false)).or_insert_with(|| tex.to_dds());
        }
        for shader in &d.shaders {
            for param in &shader.params {
                let Value::Texture(Some(texture)) = &param.value else { continue };
                let embedded = d.embedded.iter().flatten().any(|t| t.name.eq_ignore_ascii_case(texture));
                if embedded {
                    continue;
                }
                if let Some(tex) = dictionaries.iter().flat_map(|dict| &dict.textures).find(|t| t.name.eq_ignore_ascii_case(texture)) {
                    files.entry(tex.file_name(false)).or_insert_with(|| tex.to_dds());
                }
            }
        }
    }
    let xml = write(&drawables, extension.eq_ignore_ascii_case("ydd"));
    Ok(Export { xml, textures: files.into_iter().collect() })
}

/// A drawable, as far as the XML needs it.
struct Drawable {
    name: String,
    center: [f32; 3],
    radius: f32,
    min: [f32; 3],
    max: [f32; 3],
    lod_dist: [f32; 4],
    /// Low byte of each level's render mask.
    flags: [u8; 4],
    /// The embedded texture dictionary, when the shader group has one.
    embedded: Option<Vec<Texture>>,
    /// Whether the drawable has a shader group at all.
    has_group: bool,
    shaders: Vec<Shader>,
    skeleton: Option<Skeleton>,
    /// Model lists of High, Medium, Low and VeryLow (`None` when the list is absent).
    lods: [Option<Vec<Model>>; 4],
}

struct Shader {
    name: String,
    file: String,
    bucket: u8,
    params: Vec<Param>,
}

struct Param {
    name: String,
    value: Value,
}

enum Value {
    /// A texture by name, or none.
    Texture(Option<String>),
    Vector([f32; 4]),
    Array(Vec<[f32; 4]>),
}

struct Skeleton {
    /// Unknown_1Ch, and the three hashes at 0x50, 0x54 and 0x58.
    unknown: [u32; 4],
    bones: Vec<Bone>,
}

struct Bone {
    name: String,
    tag: u16,
    index: i16,
    parent: i16,
    sibling: i16,
    flags: u16,
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

struct Model {
    /// The 16-bit render mask: low byte `RenderMask`, high byte `Flags`.
    mask: u16,
    /// The skeleton binding: bone index (4th byte), unknown (3rd), skin flag (2nd).
    binding: u32,
    geometries: Vec<Geometry>,
}

struct Geometry {
    shader: u16,
    bone_ids: Vec<u16>,
    min: [f32; 3],
    max: [f32; 3],
    /// The GTAV1 components present, as bits of [`SEMANTICS`].
    layout: u32,
    vertices: Vertices,
    indices: Vec<u16>,
}

/// The drawables of a dictionary, named by its hash list.
fn read_dictionary(res: &Resource, names: &Names) -> Result<Vec<Drawable>, Error> {
    let root = res.block(ROOT, 0x40)?;
    let (hashes, count) = (u64_at(root, 0x20), u16_at(root, 0x28) as usize);
    let (list, drawables) = (u64_at(root, 0x30), u16_at(root, 0x38) as usize);
    let hashes = if count == 0 { &[][..] } else { res.block(hashes, count * 4)? };
    let pointers = if drawables == 0 { &[][..] } else { res.block(list, drawables * 8)? };
    let mut out = Vec::new();
    for (i, p) in pointers.as_chunks::<8>().0.iter().enumerate() {
        let ptr = u64::from_le_bytes(*p);
        if ptr == 0 {
            continue;
        }
        let name = hashes.get(i * 4..i * 4 + 4).map_or(String::new(), |h| names.name(u32_at(h, 0)).into_owned());
        out.push(read_drawable(res, ptr, name, names)?);
    }
    Ok(out)
}

fn read_drawable(res: &Resource, ptr: u64, name: String, names: &Names) -> Result<Drawable, Error> {
    let base = res.block(ptr, 0xA8)?;
    let enhanced = is_enhanced(res.version);
    let vec3 = |at: usize| [f32_at(base, at), f32_at(base, at + 4), f32_at(base, at + 8)];
    let group = u64_at(base, 0x10);
    let (embedded, shaders, has_group) = if group == 0 {
        (None, Vec::new(), false)
    } else {
        let g = res.block(group, 0x10)?;
        let dict = u64_at(g, 8);
        let embedded = if dict == 0 { None } else { Some(TextureDictionary::read(res, dict, enhanced)?.textures) };
        (embedded, read_shaders(res, group, enhanced, names)?, true)
    };
    let mut lods: [Option<Vec<Model>>; 4] = Default::default();
    for (i, lod) in lods.iter_mut().enumerate() {
        *lod = read_models(res, u64_at(base, 0x50 + i * 8), enhanced)?;
    }
    Ok(Drawable {
        name,
        center: vec3(0x20),
        radius: f32_at(base, 0x2C),
        min: vec3(0x30),
        max: vec3(0x40),
        lod_dist: std::array::from_fn(|i| f32_at(base, 0x70 + i * 4)),
        flags: std::array::from_fn(|i| u32_at(base, 0x80 + i * 4) as u8),
        embedded,
        has_group,
        shaders,
        skeleton: read_skeleton(res, u64_at(base, 0x18))?,
        lods,
    })
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn vec4(res: &Resource, ptr: u64) -> Result<[f32; 4], Error> {
    let b = res.block(ptr, 16)?;
    Ok(std::array::from_fn(|i| f32_at(b, i * 4)))
}

/// The name of a texture from its pointer (`None` for no texture).
fn texture_name(res: &Resource, ptr: u64) -> Result<Option<String>, Error> {
    if ptr == 0 {
        return Ok(None);
    }
    let texture = res.block(ptr, 0x30)?;
    let name = u64_at(texture, 0x28);
    Ok(Some(if name == 0 { String::new() } else { res.string(name)? }))
}

fn read_shaders(res: &Resource, group: u64, enhanced: bool, names: &Names) -> Result<Vec<Shader>, Error> {
    let g = res.block(group, 0x40)?;
    let (list, count) = (u64_at(g, 0x10), u16_at(g, 0x18) as usize);
    if list == 0 || count == 0 {
        return Ok(Vec::new());
    }
    res.block(list, count * 8)?.as_chunks::<8>().0.iter().map(|p| read_shader(res, u64::from_le_bytes(*p), enhanced, names)).collect()
}

fn read_shader(res: &Resource, ptr: u64, enhanced: bool, names: &Names) -> Result<Shader, Error> {
    if enhanced {
        // Gen9: the name at 0, the texture list at 0x10 and the parameter infos at 0x20.
        let shader = res.block(ptr, 0x40)?;
        let (name, textures, infos) = (u32_at(shader, 0), u64_at(shader, 0x10), u64_at(shader, 0x20));
        let mut params = Vec::new();
        if textures != 0 && infos != 0 {
            let header = res.block(infos, 8)?;
            let (texture_count, count) = (header[1] as usize, header[4] as usize);
            let infos = res.block(infos.wrapping_add(8), count * 8)?;
            let list = res.block(textures, texture_count * 8)?;
            for info in infos.as_chunks::<8>().0 {
                let (hash, data) = (u32_at(info, 0), u32_at(info, 4));
                // Only textures (type 0); constants have no XML form here.
                if data & 3 != 0 {
                    continue;
                }
                let index = (data >> 2 & 0xFF) as usize;
                let texture = match list.get(index * 8..index * 8 + 8).map(|b| u64_at(b, 0)) {
                    Some(t) => texture_name(res, t)?,
                    None => None,
                };
                let name = match GEN9_SAMPLERS.iter().find(|(gen9, _)| joaat_lower(gen9) == hash) {
                    Some((_, legacy)) => legacy.to_string(),
                    None => names.name(hash).into_owned(),
                };
                params.push(Param { name, value: Value::Texture(texture) });
            }
        }
        return Ok(Shader { name: names.name(name).into_owned(), file: String::new(), bucket: 0, params });
    }
    let shader = res.block(ptr, 0x30)?;
    let (params_ptr, count) = (u64_at(shader, 0), shader[0x10] as usize);
    let mut params = Vec::new();
    if params_ptr != 0 && count > 0 {
        // Parameters (type 0 = texture, 1 = one vector, more = that many vectors), their data, then their name hashes.
        let entries = res.block(params_ptr, count * 16)?;
        let data: usize = entries.as_chunks::<16>().0.iter().map(|e| 16 * e[0] as usize).sum();
        let hashes = res.block(params_ptr.wrapping_add((count * 16 + data) as u64), count * 4)?;
        for (e, h) in entries.as_chunks::<16>().0.iter().zip(hashes.as_chunks::<4>().0) {
            let value = match e[0] {
                0 => Value::Texture(texture_name(res, u64_at(e, 8))?),
                1 => Value::Vector(vec4(res, u64_at(e, 8))?),
                n => {
                    let vectors = res.block(u64_at(e, 8), n as usize * 16)?;
                    Value::Array(vectors.as_chunks::<16>().0.iter().map(|v| std::array::from_fn(|i| f32_at(v, i * 4))).collect())
                }
            };
            params.push(Param { name: names.name(u32::from_le_bytes(*h)).into_owned(), value });
        }
    }
    Ok(Shader { name: names.name(u32_at(shader, 8)).into_owned(), file: names.name(u32_at(shader, 0x18)).into_owned(), bucket: shader[0x11], params })
}

fn read_skeleton(res: &Resource, ptr: u64) -> Result<Option<Skeleton>, Error> {
    if ptr == 0 {
        return Ok(None);
    }
    let s = res.block(ptr, 0x70)?;
    let (list, count) = (u64_at(s, 0x20), u16_at(s, 0x5E) as usize);
    let unknown = [u32_at(s, 0x1C), u32_at(s, 0x50), u32_at(s, 0x54), u32_at(s, 0x58)];
    let mut bones = Vec::new();
    if list != 0 && count != 0 {
        for b in res.block(list, count * 80)?.as_chunks::<80>().0 {
            let name = u64_at(b, 0x38);
            bones.push(Bone {
                name: if name == 0 { String::new() } else { res.string(name)? },
                tag: u16_at(b, 0x44),
                index: i16::from_le_bytes([b[0x42], b[0x43]]),
                parent: i16::from_le_bytes([b[0x32], b[0x33]]),
                sibling: i16::from_le_bytes([b[0x30], b[0x31]]),
                flags: u16_at(b, 0x40),
                translation: std::array::from_fn(|i| f32_at(b, 0x10 + i * 4)),
                rotation: std::array::from_fn(|i| f32_at(b, i * 4)),
                scale: std::array::from_fn(|i| f32_at(b, 0x20 + i * 4)),
            });
        }
    }
    Ok(Some(Skeleton { unknown, bones }))
}

fn read_models(res: &Resource, list: u64, enhanced: bool) -> Result<Option<Vec<Model>>, Error> {
    if list == 0 {
        return Ok(None);
    }
    let head = res.block(list, 16)?;
    let (models, count) = (u64_at(head, 0), u16_at(head, 8) as usize);
    let mut out = Vec::new();
    if models != 0 && count != 0 {
        for p in res.block(models, count * 8)?.as_chunks::<8>().0 {
            let model = res.block(u64::from_le_bytes(*p), 0x30)?;
            let (geometries, count) = (u64_at(model, 8), u16_at(model, 0x10) as usize);
            let mapping = u64_at(model, 0x20);
            let shaders = if mapping == 0 || count == 0 { &[][..] } else { res.block(mapping, count * 2)? };
            let mut list = Vec::new();
            if geometries != 0 && count != 0 {
                for (gi, g) in res.block(geometries, count * 8)?.as_chunks::<8>().0.iter().enumerate() {
                    let shader = shaders.get(gi * 2..gi * 2 + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                    if let Some(geometry) = read_geometry(res, u64::from_le_bytes(*g), shader, enhanced)? {
                        list.push(geometry);
                    }
                }
            }
            out.push(Model { mask: u16_at(model, 0x2C), binding: u32_at(model, 0x28), geometries: list });
        }
    }
    Ok(Some(out))
}

/// A geometry with its buffers; `None` when it has none to draw.
fn read_geometry(res: &Resource, ptr: u64, shader: u16, enhanced: bool) -> Result<Option<Geometry>, Error> {
    let geom = res.block(ptr, 0x98)?;
    let (vb, ib) = (u64_at(geom, 0x18), u64_at(geom, 0x38));
    if vb == 0 || ib == 0 {
        return Ok(None);
    }
    let ib = res.block(ib, 0x20)?;
    let (indices, count) = (u64_at(ib, if enhanced { 0x18 } else { 0x10 }), u32_at(ib, 8) as usize);
    if indices == 0 || count == 0 {
        return Ok(None);
    }
    let indices: Vec<u16> = res.block(indices, count * 2)?.as_chunks::<2>().0.iter().map(|i| u16::from_le_bytes(*i)).collect();
    let (layout, vertices) = read_vertices(res, vb, enhanced)?;
    let (mut min, mut max) = ([0.0f32; 3], [0.0f32; 3]);
    for (i, v) in vertices.iter().enumerate() {
        for c in 0..3 {
            let p = v[0][c];
            min[c] = if i == 0 { p } else { min[c].min(p) };
            max[c] = if i == 0 { p } else { max[c].max(p) };
        }
    }
    let bone_ids = u64_at(geom, 0x68);
    let bone_count = u16_at(geom, 0x72) as usize;
    let bone_ids = if bone_ids == 0 || bone_count == 0 { Vec::new() } else { res.block(bone_ids, bone_count * 2)?.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect() };
    Ok(Some(Geometry { shader, bone_ids, min, max, layout, vertices, indices }))
}

/// How a component's values are stored in a vertex.
#[derive(Clone, Copy)]
enum Source {
    Floats(usize),
    Halves(usize),
    /// Bytes, written as integers.
    Bytes(usize),
}

impl Source {
    fn size(self) -> usize {
        match self {
            Source::Floats(n) => 4 * n,
            Source::Halves(n) => 2 * n,
            Source::Bytes(n) => n,
        }
    }

    fn read(self, raw: &[u8]) -> Vec<f32> {
        match self {
            Source::Floats(n) => (0..n).map(|i| f32_at(raw, i * 4)).collect(),
            Source::Halves(n) => (0..n).map(|i| half(u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]))).collect(),
            Source::Bytes(n) => raw[..n].iter().map(|&b| b as f32).collect(),
        }
    }
}

/// Whether a source can be written as its GTAV1 component (other sources are refused).
fn gtav1_accepts(bit: usize, source: Source) -> bool {
    matches!(
        (bit, source),
        (0 | 3, Source::Floats(3)) | (1 | 2 | 4 | 5, Source::Bytes(4)) | (6..=13, Source::Floats(2) | Source::Halves(2)) | (14 | 15, Source::Floats(4) | Source::Halves(4))
    )
}

fn legacy_source(nibble: u64) -> Option<Source> {
    Some(match nibble {
        1 => Source::Halves(2),
        3 => Source::Halves(4),
        5 => Source::Floats(2),
        6 => Source::Floats(3),
        7 => Source::Floats(4),
        8 | 9 => Source::Bytes(4),
        _ => return None,
    })
}

fn gen9_source(format: u8) -> Option<Source> {
    Some(match format {
        6 => Source::Floats(3),
        2 => Source::Floats(4),
        16 => Source::Floats(2),
        34 => Source::Halves(2),
        0x1C..=0x1E => Source::Bytes(4),
        _ => return None,
    })
}

/// The GTAV1 components present (as bits) and the values of each vertex.
fn read_vertices(res: &Resource, vb: u64, enhanced: bool) -> Result<(u32, Vertices), Error> {
    let unsupported = || Error::Unsupported("this vertex layout".into());
    // Each component: its GTAV1 bit, source, and byte offset in the vertex.
    let (count, stride, data, mut parts) = if enhanced {
        let b = res.block(vb, 0x40)?;
        let (count, stride, data, info) = (u32_at(b, 8) as usize, u16_at(b, 0xC) as usize, u64_at(b, 0x18), u64_at(b, 0x38));
        let decl = res.block(info, 52 * 6)?;
        let mut parts = Vec::new();
        for (slot, bit) in GEN9_SLOTS {
            let format = decl[52 * 5 + slot];
            if format == 0 {
                continue;
            }
            parts.push((bit, gen9_source(format).ok_or_else(unsupported)?, u32_at(decl, slot * 4) as usize));
        }
        (count, stride, data, parts)
    } else {
        let b = res.block(vb, 0x40)?;
        let (stride, data, count, info) = (u16_at(b, 8) as usize, u64_at(b, 0x10), u32_at(b, 0x18) as usize, u64_at(b, 0x30));
        let decl = res.block(info, 16)?;
        let (flags, types) = (u32_at(decl, 0), u64_at(decl, 8));
        let mut parts = Vec::new();
        let mut at = 0;
        for bit in 0..16 {
            if flags & (1 << bit) == 0 {
                continue;
            }
            let source = legacy_source((types >> (bit * 4)) & 0xF).ok_or_else(unsupported)?;
            parts.push((bit, source, at));
            at += source.size();
        }
        (count, stride, data, parts)
    };
    let mut layout = 0u32;
    for &(bit, source, at) in &parts {
        if !gtav1_accepts(bit, source) || at + source.size() > stride {
            return Err(unsupported());
        }
        layout |= 1 << bit;
    }
    if layout & 1 == 0 {
        return Err(unsupported());
    }
    parts.sort_by_key(|p| p.0);
    if count == 0 {
        return Ok((layout, Vec::new()));
    }
    let raw = res.block(data, stride * count)?;
    let vertices = raw.chunks_exact(stride).map(|v| parts.iter().map(|&(_, source, at)| source.read(&v[at..])).collect()).collect();
    Ok((layout, vertices))
}

/// Formats a float as CodeWalker reads it back (`NaN` and the infinities by name).
fn fl(v: f32) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        v.to_string()
    }
}

/// Escapes text for XML, quotes included; control characters, which XML cannot hold, are left out.
fn escape(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r')).collect();
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `x="" y="" z=""` (and `w` for four values), with a leading space.
fn attrs(v: &[f32]) -> String {
    let names = ["x", "y", "z", "w"];
    v.iter().zip(names).map(|(x, n)| format!(" {n}=\"{}\"", fl(*x))).collect()
}

fn d3d_name(format: Format) -> &'static str {
    match format {
        Format::Bgra8 => "D3DFMT_A8R8G8B8",
        Format::Bgrx8 => "D3DFMT_X8R8G8B8",
        Format::Bgr5a1 => "D3DFMT_A1R5G5B5",
        Format::A8 => "D3DFMT_A8",
        Format::Rgba8 => "D3DFMT_A8B8G8R8",
        Format::L8 => "D3DFMT_L8",
        Format::Bc1 => "D3DFMT_DXT1",
        Format::Bc2 => "D3DFMT_DXT3",
        Format::Bc3 => "D3DFMT_DXT5",
        Format::Bc4 => "D3DFMT_ATI1",
        Format::Bc5 => "D3DFMT_ATI2",
        Format::Bc6h => "D3DFMT_BC6H",
        Format::Bc7 => "D3DFMT_BC7",
    }
}

/// Writes CodeWalker's XML with one space of indentation per level.
struct Xml {
    out: String,
    depth: usize,
}

impl Xml {
    fn line(&mut self, text: &str) {
        self.out.extend(std::iter::repeat_n(' ', self.depth));
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn open(&mut self, name: &str) {
        self.line(&format!("<{name}>"));
        self.depth += 1;
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.line(&format!("</{name}>"));
    }

    /// `<name attrs />`, where `attrs` starts with its own space (or is empty).
    fn empty(&mut self, name: &str, attrs: &str) {
        self.line(&format!("<{name}{attrs} />"));
    }

    fn value(&mut self, name: &str, value: impl Display) {
        self.empty(name, &format!(" value=\"{value}\""));
    }

    fn text(&mut self, name: &str, text: &str) {
        if text.is_empty() {
            self.empty(name, "");
        } else {
            self.line(&format!("<{name}>{}</{name}>", escape(text)));
        }
    }

    fn drawable(&mut self, d: &Drawable) {
        self.text("Name", &d.name);
        self.empty("BoundingSphereCenter", &attrs(&d.center));
        self.value("BoundingSphereRadius", fl(d.radius));
        self.empty("BoundingBoxMin", &attrs(&d.min));
        self.empty("BoundingBoxMax", &attrs(&d.max));
        for (name, dist) in ["LodDistHigh", "LodDistMed", "LodDistLow", "LodDistVlow"].iter().zip(d.lod_dist) {
            self.value(name, fl(dist));
        }
        for (name, flags) in ["FlagsHigh", "FlagsMed", "FlagsLow", "FlagsVlow"].iter().zip(d.flags) {
            self.value(name, flags);
        }
        if d.has_group {
            self.shader_group(d);
        }
        if let Some(skeleton) = &d.skeleton {
            self.skeleton(skeleton);
        }
        for (name, lod) in ["DrawableModelsHigh", "DrawableModelsMedium", "DrawableModelsLow", "DrawableModelsVeryLow"].iter().zip(&d.lods) {
            if let Some(models) = lod {
                self.models(name, models);
            }
        }
    }

    fn shader_group(&mut self, d: &Drawable) {
        self.open("ShaderGroup");
        if let Some(textures) = &d.embedded {
            if textures.is_empty() {
                self.empty("TextureDictionary", "");
            } else {
                self.open("TextureDictionary");
                for tex in textures {
                    self.open("Item");
                    self.text("Name", &tex.name);
                    self.value("Unk32", 0);
                    self.text("Usage", "DEFAULT");
                    self.text("UsageFlags", "0");
                    self.value("ExtraFlags", 0);
                    self.value("Width", tex.width);
                    self.value("Height", tex.height);
                    self.value("MipLevels", tex.levels);
                    self.text("Format", d3d_name(tex.format));
                    self.text("FileName", &tex.file_name(false));
                    self.close("Item");
                }
                self.close("TextureDictionary");
            }
        }
        if d.shaders.is_empty() {
            self.empty("Shaders", "");
        } else {
            self.open("Shaders");
            for shader in &d.shaders {
                self.open("Item");
                self.text("Name", &shader.name);
                self.text("FileName", &shader.file);
                self.value("RenderBucket", shader.bucket);
                if !shader.params.is_empty() {
                    self.open("Parameters");
                    for param in &shader.params {
                        self.param(param);
                    }
                    self.close("Parameters");
                }
                self.close("Item");
            }
            self.close("Shaders");
        }
        self.close("ShaderGroup");
    }

    fn param(&mut self, param: &Param) {
        let name = escape(&param.name);
        match &param.value {
            Value::Texture(None) => self.empty("Item", &format!(" name=\"{name}\" type=\"Texture\"")),
            Value::Texture(Some(texture)) => {
                self.line(&format!("<Item name=\"{name}\" type=\"Texture\">"));
                self.depth += 1;
                self.text("Name", texture);
                self.close("Item");
            }
            Value::Vector(v) => self.empty("Item", &format!(" name=\"{name}\" type=\"Vector\"{}", attrs(v))),
            Value::Array(values) => {
                self.line(&format!("<Item name=\"{name}\" type=\"Array\">"));
                self.depth += 1;
                for v in values {
                    self.empty("Value", &attrs(v));
                }
                self.close("Item");
            }
        }
    }

    fn skeleton(&mut self, skeleton: &Skeleton) {
        self.open("Skeleton");
        for (name, value) in ["Unknown1C", "Unknown50", "Unknown54", "Unknown58"].iter().zip(skeleton.unknown) {
            self.value(name, value);
        }
        if skeleton.bones.is_empty() {
            self.empty("Bones", "");
        } else {
            self.open("Bones");
            for bone in &skeleton.bones {
                self.open("Item");
                self.text("Name", &bone.name);
                self.value("Tag", bone.tag);
                self.value("Index", bone.index);
                self.value("ParentIndex", bone.parent);
                self.value("SiblingIndex", bone.sibling);
                self.text("Flags", &bone_flags(bone.flags));
                self.empty("Translation", &attrs(&bone.translation));
                self.empty("Rotation", &attrs(&bone.rotation));
                self.empty("Scale", &attrs(&bone.scale));
                self.empty("TransformUnk", &attrs(&[0.0; 4]));
                self.close("Item");
            }
            self.close("Bones");
        }
        self.close("Skeleton");
    }

    fn models(&mut self, name: &str, models: &[Model]) {
        if models.is_empty() {
            self.empty(name, "");
            return;
        }
        self.open(name);
        for model in models {
            self.open("Item");
            self.value("RenderMask", model.mask & 0xFF);
            self.value("Flags", model.mask >> 8);
            self.value("HasSkin", ((model.binding >> 8) & 0xFF != 0) as u8);
            self.value("BoneIndex", (model.binding >> 24) as u8);
            self.value("Unknown1", (model.binding >> 16) as u8);
            if model.geometries.is_empty() {
                self.empty("Geometries", "");
            } else {
                self.open("Geometries");
                for g in &model.geometries {
                    self.geometry(g);
                }
                self.close("Geometries");
            }
            self.close("Item");
        }
        self.close(name);
    }

    fn geometry(&mut self, g: &Geometry) {
        self.open("Item");
        self.value("ShaderIndex", g.shader);
        self.empty("BoundingBoxMin", &attrs(&[g.min[0], g.min[1], g.min[2], 0.0]));
        self.empty("BoundingBoxMax", &attrs(&[g.max[0], g.max[1], g.max[2], 0.0]));
        if !g.bone_ids.is_empty() {
            let ids: Vec<String> = g.bone_ids.iter().map(u16::to_string).collect();
            self.text("BoneIDs", &ids.join(", "));
        }
        self.open("VertexBuffer");
        self.value("Flags", 0);
        self.line("<Layout type=\"GTAV1\">");
        self.depth += 1;
        for (bit, name) in SEMANTICS.iter().enumerate() {
            if g.layout & (1 << bit) != 0 {
                self.empty(name, "");
            }
        }
        self.close("Layout");
        if g.vertices.is_empty() {
            self.empty("Data", "");
        } else {
            self.open("Data");
            for vertex in &g.vertices {
                let row: Vec<String> = vertex.iter().map(|c| c.iter().map(|v| fl(*v)).collect::<Vec<_>>().join(" ")).collect();
                self.line(&row.join("   "));
            }
            self.close("Data");
        }
        self.close("VertexBuffer");
        self.open("IndexBuffer");
        self.indices(&g.indices);
        self.close("IndexBuffer");
        self.close("Item");
    }

    /// Triangle indices: on one line up to 24 of them, else 24 to a line.
    fn indices(&mut self, indices: &[u16]) {
        if indices.is_empty() {
            self.empty("Data", "");
        } else if indices.len() <= 24 {
            let list: Vec<String> = indices.iter().map(u16::to_string).collect();
            self.line(&format!("<Data>{}</Data>", list.join(" ")));
        } else {
            self.open("Data");
            for chunk in indices.chunks(24) {
                let list: Vec<String> = chunk.iter().map(u16::to_string).collect();
                self.line(&list.join(" "));
            }
            self.close("Data");
        }
    }
}

fn bone_flags(flags: u16) -> String {
    if flags == 0 {
        return "None".into();
    }
    (0..16).filter(|&i| flags & (1 << i) != 0).map(|i| BONE_FLAGS[i]).collect::<Vec<_>>().join(", ")
}

/// The XML of a `.ydr` (one drawable) or a `.ydd` (a dictionary of them).
fn write(drawables: &[Drawable], dictionary: bool) -> String {
    let mut xml = Xml { out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"), depth: 0 };
    if dictionary {
        xml.open("DrawableDictionary");
        for d in drawables {
            xml.open("Item");
            xml.drawable(d);
            xml.close("Item");
        }
        xml.close("DrawableDictionary");
    } else {
        xml.open("Drawable");
        xml.drawable(&drawables[0]);
        xml.close("Drawable");
    }
    xml.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(indices: Vec<u16>) -> Geometry {
        // Position, Colour0 (bytes) and TexCoord0: the layout bits 0, 4 and 6.
        let vertices = vec![vec![vec![0.0, 0.0, 0.0], vec![255.0, 0.0, 0.0, 0.0], vec![0.0, 1.0]], vec![vec![1.0, 0.0, 0.0], vec![1.0, 2.0, 3.0, 4.0], vec![0.5, 0.25]]];
        Geometry { shader: 0, bone_ids: vec![], min: [0.0; 3], max: [1.0; 3], layout: 1 | 16 | 64, vertices, indices }
    }

    fn drawable(name: &str, indices: Vec<u16>) -> Drawable {
        let model = Model { mask: 0x80FF, binding: 0x0100_0000, geometries: vec![geometry(indices)] };
        Drawable {
            name: name.into(),
            center: [0.0; 3],
            radius: 1.0,
            min: [-1.0; 3],
            max: [1.0; 3],
            lod_dist: [9998.0; 4],
            flags: [0; 4],
            embedded: None,
            has_group: false,
            shaders: Vec::new(),
            skeleton: None,
            lods: [Some(vec![model]), None, None, None],
        }
    }

    #[test]
    fn rows_separate_components_by_three_spaces() {
        let xml = write(&[drawable("prop_x", vec![0, 1, 1])], false);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Drawable>\n"), "{xml}");
        assert!(xml.contains("<Layout type=\"GTAV1\">\n"), "{xml}");
        assert!(xml.contains("<Position />\n"), "{xml}");
        assert!(xml.contains("\n       0 0 0   255 0 0 0   0 1\n"), "{xml}");
        assert!(xml.contains("\n       1 0 0   1 2 3 4   0.5 0.25\n"), "{xml}");
        assert!(xml.contains("<IndexBuffer>\n      <Data>0 1 1</Data>\n"), "{xml}");
        assert!(xml.contains("<BoundingSphereRadius value=\"1\" />"), "{xml}");
        assert!(xml.contains("<DrawableModelsHigh>"), "{xml}");
        assert!(!xml.contains("DrawableModelsMedium"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn long_index_lists_wrap_at_24() {
        let xml = write(&[drawable("prop_x", (0..30).map(|i| i % 2).collect())], false);
        assert!(xml.contains("      <Data>\n       0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1\n       0 1 0 1 0 1\n      </Data>"), "{xml}");
    }

    #[test]
    fn names_are_escaped() {
        let xml = write(&[drawable("a<b&\"c\"", vec![0, 1, 1])], false);
        assert!(xml.contains("<Name>a&lt;b&amp;&quot;c&quot;</Name>"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn dictionaries_wrap_their_items() {
        let xml = write(&[drawable("a", vec![0, 1, 1]), drawable("b", vec![0, 1, 1])], true);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DrawableDictionary>\n"), "{xml}");
        assert_eq!(xml.matches("\n <Item>\n").count(), 2, "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn control_characters_are_left_out() {
        let xml = write(&[drawable("a\u{1}b\tc", vec![0, 1, 1])], false);
        assert!(xml.contains("<Name>ab\tc</Name>"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn floats_read_back_exactly() {
        assert_eq!(fl(0.5), "0.5");
        assert_eq!(fl(f32::NAN), "NaN");
        assert_eq!(fl(f32::NEG_INFINITY), "-Infinity");
        assert_eq!(bone_flags(0), "None");
        assert_eq!(bone_flags(0x1 | 0x10), "RotX, TransX");
    }
}
