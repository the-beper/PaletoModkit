//! Models (`.ydr`, `.ydd`, `.yft`) and collision (`.ybn`) as CodeWalker XML:
//! the text Sollumz and CodeWalker import, with the textures it names as DDS
//! files. Layouts follow CodeWalker's `Drawable.cs` and `Frag.cs` (MIT); the
//! XML follows their `WriteXml` methods. Collision bounds are written by
//! [`crate::bounds_xml`], fragments by the `fragment` module.
//!
//! Vertices are written in the GTAV1 layout whenever their values fit it, else
//! (cloth) in their own GTAV2, GTAV3 or GTAV4 layout. Enhanced shaders are
//! written as CodeWalker reads them: their textures and constants, renamed to
//! the Legacy names by `gen9_shaders.txt` (from CodeWalker's
//! `ShadersGen9Conversion.xml`).

mod fragment;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::sync::LazyLock;

use crate::Error;
use crate::bounds_xml::bound_xml;
use crate::drawable::{half, is_enhanced};
use crate::hash::joaat_lower;
use crate::names::Names;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};
use crate::ytd::{Format, Texture, TextureDictionary, usage_fields};

const ROOT: u64 = 0x5000_0000;
/// Element names of the vertex semantics, by bit (CodeWalker's `VertexSemantics`).
const SEMANTICS: [&str; 16] = [
    "Position", "BlendWeights", "BlendIndices", "Normal", "Colour0", "Colour1", "TexCoord0", "TexCoord1", "TexCoord2", "TexCoord3", "TexCoord4", "TexCoord5", "TexCoord6", "TexCoord7", "Tangent", "Binormal",
];
/// Gen9 vertex element slots (their byte offset and format are at `slot * 4` and `52 * 5 + slot`
/// of the declaration), with the GTAV1 bit each becomes.
const GEN9_SLOTS: [(usize, usize); 13] = [(0, 0), (4, 3), (8, 14), (16, 1), (20, 2), (24, 4), (25, 5), (28, 6), (29, 7), (30, 8), (31, 9), (32, 10), (33, 11)];
/// The vertex declaration types CodeWalker names (`VertexDeclarationTypes`): each
/// semantic's component type in the nibble of its bit.
const DECLARATIONS: [(&str, u64); 4] = [("GTAV1", 0x7755_5555_5599_6996), ("GTAV2", 0x0300_0000_0199_A006), ("GTAV3", 0x0300_0000_0199_6006), ("GTAV4", 0x7655_5555_5599_6996)];
/// Vertices, each a list of its components' values in bit order.
type Vertices = Vec<Vec<Vec<f32>>>;
/// Bone flags by bit (CodeWalker's `EBoneFlags`).
const BONE_FLAGS: [&str; 16] = ["RotX", "RotY", "RotZ", "LimitRotation", "TransX", "TransY", "TransZ", "LimitTranslation", "ScaleX", "ScaleY", "ScaleZ", "LimitScale", "Unk0", "Unk1", "Unk2", "Unk3"];
/// Texture usages by value (CodeWalker's `TextureUsage`).
const USAGES: [&str; 30] = [
    "UNKNOWN", "DEFAULT", "TERRAIN", "CLOUDDENSITY", "CLOUDNORMAL", "CABLE", "FENCE", "ENVEFF", "SCRIPT", "WATERFLOW", "WATERFOAM", "WATERFOG", "WATEROCEAN", "WATER", "FOAMOPACITY", "FOAM", "DIFFUSEMIPSHARPEN", "DIFFUSEDETAIL", "DIFFUSEDARK", "DIFFUSEALPHAOPAQUE", "DIFFUSE", "DETAIL", "NORMAL", "SPECULAR", "EMISSIVE", "TINTPALETTE", "SKIPPROCESSING", "DONOTOPTIMIZE", "TEST", "COUNT",
];
/// Texture usage flags by bit (CodeWalker's `TextureUsageFlags`).
const USAGE_FLAGS: [&str; 25] = [
    "NOT_HALF", "HD_SPLIT", "X2", "X4", "Y4", "X8", "X16", "X32", "X64", "Y64", "X128", "X256", "X512", "Y512", "X1024", "Y1024", "X2048", "Y2048", "EMBEDDEDSCRIPTRT", "UNK19", "UNK20", "UNK21", "FLAG_FULL", "MAPS_HALF", "UNK24",
];

/// A fragment drawable's matrix set: its count, and each matrix with its address.
type MatrixSet = (usize, Vec<(u64, [f32; 12])>);
/// Joint limits: rotation (bone id, unknown, min, max) and translation (bone id, min, max).
type RotationLimit = (u16, u16, [f32; 3], [f32; 3]);
type TranslationLimit = (u32, [f32; 3], [f32; 3]);

/// Shader and parameter names by hash, from `gen9_shaders.txt`.
#[derive(Default)]
struct ShaderTable {
    shaders: HashMap<u32, &'static str>,
    /// `<shader>.sps`, by its hash.
    files: HashMap<u32, String>,
    /// Legacy parameter names, as the Legacy shaders spell them.
    legacy: HashMap<u32, &'static str>,
    /// The usual Legacy parameter of each Enhanced one.
    common: HashMap<u32, u32>,
    /// For each shader of the table, the Enhanced parameters it maps otherwise (`None`: kept as they are).
    exceptions: HashMap<u32, Vec<(u32, Option<u32>)>>,
}

static SHADERS: LazyLock<ShaderTable> = LazyLock::new(|| {
    // Names as CodeWalker hashes them: lowercase, or `hash_XXXXXXXX`.
    let hash = |name: &str| name.strip_prefix("hash_").and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or_else(|| joaat_lower(name));
    let mut table = ShaderTable::default();
    for line in include_str!("gen9_shaders.txt").lines().filter(|l| !l.starts_with('#')) {
        if let Some(rest) = line.strip_prefix('@') {
            let mut parts = rest.split('\t');
            let name = parts.next().unwrap_or_default();
            let mut exceptions = Vec::new();
            for part in parts {
                exceptions.push(match part.split_once('=') {
                    Some((gen9, legacy)) => {
                        table.legacy.entry(joaat_lower(legacy)).or_insert(legacy);
                        (hash(gen9), Some(joaat_lower(legacy)))
                    }
                    None => (hash(part), None),
                });
            }
            table.shaders.insert(joaat_lower(name), name);
            table.files.insert(joaat_lower(&format!("{name}.sps")), format!("{name}.sps"));
            table.exceptions.insert(joaat_lower(name), exceptions);
        } else if let Some((gen9, legacy)) = line.split_once('\t') {
            table.legacy.entry(joaat_lower(legacy)).or_insert(legacy);
            table.common.insert(hash(gen9), joaat_lower(legacy));
        }
    }
    table
});

impl ShaderTable {
    /// The Legacy parameter an Enhanced one of a shader becomes (CodeWalker's `ParamsMapGen9ToLegacy`).
    fn legacy_param(&self, shader: u32, param: u32) -> u32 {
        let Some(exceptions) = self.exceptions.get(&shader) else { return param };
        match exceptions.iter().find(|e| e.0 == param) {
            Some((_, legacy)) => legacy.unwrap_or(param),
            None => self.common.get(&param).copied().unwrap_or(param),
        }
    }

    fn param_name(&self, hash: u32, names: &Names) -> String {
        self.legacy.get(&hash).map_or_else(|| names.name(hash).into_owned(), |n| n.to_string())
    }

    fn shader_name(&self, hash: u32, names: &Names) -> String {
        self.shaders.get(&hash).map_or_else(|| names.name(hash).into_owned(), |n| n.to_string())
    }

    fn file_name(&self, hash: u32, names: &Names) -> String {
        self.files.get(&hash).cloned().unwrap_or_else(|| names.name(hash).into_owned())
    }
}

/// Result of an export: the XML, and the DDS files it names (name, contents).
pub struct Export {
    pub xml: String,
    pub textures: Vec<(String, Vec<u8>)>,
}

/// Exports a model (`.ydr`, `.ydd`, `.yft`) or collision (`.ybn`, without
/// textures). `name` is the drawable's name (a `.ydr`'s file name; fragments
/// carry their own); `dictionaries` are searched for the textures its shaders
/// name but do not hold.
pub fn export(data: &[u8], extension: &str, name: &str, dictionaries: &[TextureDictionary]) -> Result<Export, Error> {
    let extension = extension.to_ascii_lowercase();
    if extension == "ybn" {
        return Ok(Export { xml: crate::bounds_xml::export_ybn(data)?, textures: Vec::new() });
    }
    let res = Resource::parse(data)?;
    let names = Names::new();
    let (xml, drawables) = match extension.as_str() {
        "ydr" => {
            let drawables = vec![read_drawable(&res, ROOT, name.to_string(), &names)?];
            (write(&drawables, false), drawables)
        }
        "ydd" => {
            let drawables = read_dictionary(&res, &names)?;
            (write(&drawables, true), drawables)
        }
        "yft" => fragment::write(&res, &names)?,
        other => return Err(Error::Unsupported(format!(".{other} models"))),
    };
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for d in &drawables {
        for tex in d.embedded.iter().flatten().map(|e| &e.texture) {
            files.entry(tex.file_name(false)).or_insert_with(|| tex.to_dds());
        }
        for shader in &d.shaders {
            for param in &shader.params {
                let Value::Texture(Some(texture)) = &param.value else { continue };
                let embedded = d.embedded.iter().flatten().any(|e| e.texture.name.eq_ignore_ascii_case(texture));
                if embedded {
                    continue;
                }
                if let Some(tex) = dictionaries.iter().flat_map(|dict| &dict.textures).find(|t| t.name.eq_ignore_ascii_case(texture)) {
                    files.entry(tex.file_name(false)).or_insert_with(|| tex.to_dds());
                }
            }
        }
    }
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
    embedded: Option<Vec<Embedded>>,
    /// Whether the drawable has a shader group at all.
    has_group: bool,
    shaders: Vec<Shader>,
    skeleton: Option<Skeleton>,
    joints: Option<Joints>,
    /// Model lists of High, Medium, Low and VeryLow (`None` when the list is absent).
    lods: [Option<Vec<Model>>; 4],
    /// The `<Bounds>` element of the embedded collision, indented from 0 (empty for none).
    bounds: String,
    /// The lights of a `.ydr` or `.ydd` drawable (fragments' drawables have none).
    lights: Option<Vec<Light>>,
    /// The fields of a fragment's drawable written before the drawable's own.
    frag: Option<FragFields>,
}

/// CodeWalker's `FragDrawable` fields: its matrix, and the capacity and used
/// entries (id, matrix) of its matrix list. Matrices are four columns of three.
struct FragFields {
    matrix: [f32; 12],
    matrices: Option<MatrixSet>,
}

/// An embedded texture with the header fields CodeWalker writes.
struct Embedded {
    texture: Texture,
    /// `Unknown_32h`, the usage data (usage in the low 5 bits, flags above) and the extra flags.
    unk32: u16,
    usage: u32,
    extra: u32,
}

struct Joints {
    /// Bone id, `UnknownA`, minimum and maximum of each rotation limit.
    rotation: Option<Vec<RotationLimit>>,
    /// Bone id, minimum and maximum of each translation limit.
    translation: Option<Vec<TranslationLimit>>,
}

/// A light: its 168 bytes (CodeWalker's `LightAttributes`) and the name of its projected texture.
struct Light {
    raw: Vec<u8>,
    projected: String,
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
    /// A constant without data, by its type ("Vector" or "Array").
    Missing(&'static str),
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
    /// The declaration type the vertices are written in (a name of [`DECLARATIONS`]).
    kind: &'static str,
    /// The components present, as bits of [`SEMANTICS`].
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

/// A `.ydr` or `.ydd` drawable: the drawable base, then its name, lights (at 0xB0) and bound (at 0xC8).
fn read_drawable(res: &Resource, ptr: u64, name: String, names: &Names) -> Result<Drawable, Error> {
    let mut d = read_base(res, ptr, name, names)?;
    let b = res.block(ptr, 0xD0)?;
    d.lights = Some(read_lights(res, u64_at(b, 0xB0), u16_at(b, 0xB8), names)?);
    d.bounds = bound_xml(res, u64_at(b, 0xC8), 0)?;
    Ok(d)
}

/// A fragment's drawable (CodeWalker's `FragDrawable`): the drawable base, its matrix
/// at 0xB0, bound at 0xF0, matrix list and name at 0x130. `name` replaces its own;
/// a `part` (a physics child or array drawable, CodeWalker's `OwnerDrawable`) is
/// written without its skeleton and bound.
fn read_frag_drawable(res: &Resource, ptr: u64, name: Option<String>, part: bool, names: &Names) -> Result<Drawable, Error> {
    let f = res.block(ptr, 0x150)?;
    let name = match name {
        Some(name) => name,
        None => match u64_at(f, 0x130) {
            0 => String::new(),
            p => res.string(p)?,
        },
    };
    let mut d = read_base(res, ptr, name, names)?;
    let matrix = |b: &[u8]| -> [f32; 12] { std::array::from_fn(|i| f32_at(b, (i / 3) * 16 + (i % 3) * 4)) };
    let (indices, index_count, capacity, list, count) = (u64_at(f, 0xF8), u16_at(f, 0x100) as usize, u16_at(f, 0x102) as usize, u64_at(f, 0x108), u16_at(f, 0x110) as usize);
    let matrices = if list == 0 || capacity == 0 {
        None
    } else {
        let all = res.block(list, capacity * 64)?;
        let ids = if indices == 0 || index_count == 0 { &[][..] } else { res.block(indices, index_count * 8)? };
        let used = (0..capacity.min(count)).map(|i| (ids.get(i * 8..i * 8 + 8).map_or(0, |b| u64_at(b, 0)), matrix(&all[i * 64..]))).collect();
        Some((capacity, used))
    };
    d.frag = Some(FragFields { matrix: matrix(&f[0xB0..]), matrices });
    if part {
        d.skeleton = None;
    } else {
        d.bounds = crate::bounds_xml::fragment_bound_xml(res, u64_at(f, 0xF0), 0)?;
    }
    Ok(d)
}

/// The drawable base (CodeWalker's `DrawableBase`), without bounds or lights.
fn read_base(res: &Resource, ptr: u64, name: String, names: &Names) -> Result<Drawable, Error> {
    let base = res.block(ptr, 0xA8)?;
    let enhanced = is_enhanced(res.version);
    let vec3 = |at: usize| [f32_at(base, at), f32_at(base, at + 4), f32_at(base, at + 8)];
    let group = u64_at(base, 0x10);
    let (embedded, shaders, has_group) = if group == 0 {
        (None, Vec::new(), false)
    } else {
        let g = res.block(group, 0x10)?;
        let dict = u64_at(g, 8);
        let embedded = if dict == 0 {
            None
        } else {
            let mut list = Vec::new();
            for (at, texture) in TextureDictionary::read_located(res, dict, enhanced)?.0 {
                let (unk32, usage, extra) = usage_fields(res, at, enhanced)?;
                list.push(Embedded { texture, unk32, usage, extra });
            }
            Some(list)
        };
        (embedded, read_shaders(res, group, enhanced, names)?, true)
    };
    let mut lods: [Option<Vec<Model>>; 4] = Default::default();
    let mut budget = Budget::default();
    for (i, lod) in lods.iter_mut().enumerate() {
        *lod = read_models(res, u64_at(base, 0x50 + i * 8), enhanced, &mut budget)?;
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
        joints: read_joints(res, u64_at(base, 0x90))?,
        lods,
        bounds: String::new(),
        lights: None,
        frag: None,
    })
}

pub(crate) fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn vec3_at(b: &[u8], at: usize) -> [f32; 3] {
    [f32_at(b, at), f32_at(b, at + 4), f32_at(b, at + 8)]
}

/// Joint limits (CodeWalker's `Joints`): rotation limits of 0xC0 bytes, translation limits of 0x40.
fn read_joints(res: &Resource, ptr: u64) -> Result<Option<Joints>, Error> {
    if ptr == 0 {
        return Ok(None);
    }
    let j = res.block(ptr, 0x40)?;
    let (rotation, rotations, translation, translations) = (u64_at(j, 0x10), u16_at(j, 0x30) as usize, u64_at(j, 0x18), u16_at(j, 0x32) as usize);
    let rotation = if rotation == 0 || rotations == 0 { None } else { Some(res.block(rotation, rotations * 0xC0)?.as_chunks::<0xC0>().0.iter().map(|r| (u16_at(r, 8), u16_at(r, 0xA), vec3_at(r, 0x5C), vec3_at(r, 0x68))).collect()) };
    let translation = if translation == 0 || translations == 0 { None } else { Some(res.block(translation, translations * 0x40)?.as_chunks::<0x40>().0.iter().map(|t| (u32_at(t, 8), vec3_at(t, 0x20), vec3_at(t, 0x30))).collect()) };
    Ok(Some(Joints { rotation, translation }))
}

/// Lights (CodeWalker's `LightAttributes`, 0xA8 bytes each) from a list's pointer and count.
fn read_lights(res: &Resource, list: u64, count: u16, names: &Names) -> Result<Vec<Light>, Error> {
    let count = count as usize;
    if list == 0 || count == 0 {
        return Ok(Vec::new());
    }
    Ok(res.block(list, count * 0xA8)?.as_chunks::<0xA8>().0.iter().map(|l| Light { raw: l.to_vec(), projected: names.name(u32_at(l, 0xA0)).into_owned() }).collect())
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
        // Gen9: the name at 0, the parameter block at 8 (its buffer pointers first), the texture
        // list at 0x10, the parameter infos at 0x20 and the render bucket at 0x39.
        let shader = res.block(ptr, 0x40)?;
        let (hash, block, textures, infos) = (u32_at(shader, 0), u64_at(shader, 8), u64_at(shader, 0x10), u64_at(shader, 0x20));
        let mut params = Vec::new();
        if infos != 0 {
            let header = res.block(infos, 8)?;
            let (buffers, texture_count, count) = (header[0] as usize, header[1] as usize, header[4] as usize);
            let infos = res.block(infos.wrapping_add(8), count * 8)?;
            let textures = if textures == 0 || texture_count == 0 { &[][..] } else { res.block(textures, texture_count * 8)? };
            let buffers = if block == 0 || buffers == 0 { &[][..] } else { res.block(block, buffers * 8)? };
            for info in infos.as_chunks::<8>().0 {
                let (param, data) = (u32_at(info, 0), u32_at(info, 4));
                let value = match data & 3 {
                    0 => {
                        let index = (data >> 2 & 0xFF) as usize;
                        match textures.get(index * 8..index * 8 + 8) {
                            Some(t) => Value::Texture(texture_name(res, u64_at(t, 0))?),
                            None => Value::Texture(None),
                        }
                    }
                    // A constant: buffer index, byte offset and length.
                    3 => {
                        let (buffer, offset, len) = ((data >> 2 & 0x3F) as usize, (data >> 8 & 0xFFF) as u64, (data >> 20 & 0xFFF) as usize);
                        let base = buffers.get(buffer * 8..buffer * 8 + 8).map_or(0, |b| u64_at(b, 0));
                        let (floats, vectors) = (len / 4, len / 16);
                        if base == 0 || floats == 0 {
                            Value::Missing(if vectors > 1 { "Array" } else { "Vector" })
                        } else if floats <= 4 {
                            let b = res.block(base.wrapping_add(offset), floats * 4)?;
                            Value::Vector(std::array::from_fn(|i| if i < floats { f32_at(b, i * 4) } else { 0.0 }))
                        } else if vectors == 1 {
                            // Between 5 and 7 floats: CodeWalker writes the item without values.
                            Value::Missing("Vector")
                        } else {
                            let b = res.block(base.wrapping_add(offset), vectors * 16)?;
                            Value::Array(b.as_chunks::<16>().0.iter().map(|v| std::array::from_fn(|i| f32_at(v, i * 4))).collect())
                        }
                    }
                    // Samplers and the unknown kind have no XML form.
                    _ => continue,
                };
                params.push(Param { name: SHADERS.param_name(SHADERS.legacy_param(hash, param), names), value });
            }
        }
        // CodeWalker names the file after the shader.
        let name = SHADERS.shader_name(hash, names);
        return Ok(Shader { file: format!("{name}.sps"), name, bucket: shader[0x39], params });
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
            params.push(Param { name: SHADERS.param_name(u32::from_le_bytes(*h), names), value });
        }
    }
    Ok(Shader { name: SHADERS.shader_name(u32_at(shader, 8), names), file: SHADERS.file_name(u32_at(shader, 0x18), names), bucket: shader[0x11], params })
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

/// Work limits shared by all the models of one drawable: models may point at the same geometry list,
/// so the counts are of visits, not of distinct data.
struct Budget {
    geometries: usize,
    vertices: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { geometries: 0, vertices: MAX_VERTICES }
    }
}

/// Geometry visits allowed per drawable.
const MAX_GEOMETRIES: usize = 100_000;
/// Vertices read per drawable.
const MAX_VERTICES: usize = 8_000_000;

fn read_models(res: &Resource, list: u64, enhanced: bool, budget: &mut Budget) -> Result<Option<Vec<Model>>, Error> {
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
                budget.geometries += count;
                if budget.geometries > MAX_GEOMETRIES {
                    return Err(Error::Format("model with too many geometries".into()));
                }
                for (gi, g) in res.block(geometries, count * 8)?.as_chunks::<8>().0.iter().enumerate() {
                    let shader = shaders.get(gi * 2..gi * 2 + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                    if let Some(geometry) = read_geometry(res, u64::from_le_bytes(*g), shader, enhanced, budget)? {
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
fn read_geometry(res: &Resource, ptr: u64, shader: u16, enhanced: bool, budget: &mut Budget) -> Result<Option<Geometry>, Error> {
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
    let (kind, layout, vertices) = read_vertices(res, vb, enhanced, budget)?;
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
    Ok(Some(Geometry { shader, bone_ids, min, max, kind, layout, vertices, indices }))
}

/// How a component's values are stored in a vertex.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Source {
    Floats(usize),
    Halves(usize),
    /// Bytes, written as integers.
    Bytes(usize),
    /// Four signed bytes, written divided by 127 (CodeWalker's `RGBA8SNorm`).
    Snorm,
}

impl Source {
    fn size(self) -> usize {
        match self {
            Source::Floats(n) => 4 * n,
            Source::Halves(n) => 2 * n,
            Source::Bytes(n) => n,
            Source::Snorm => 4,
        }
    }

    fn read(self, raw: &[u8]) -> Vec<f32> {
        match self {
            Source::Floats(n) => (0..n).map(|i| f32_at(raw, i * 4)).collect(),
            Source::Halves(n) => (0..n).map(|i| half(u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]))).collect(),
            Source::Bytes(n) => raw[..n].iter().map(|&b| b as f32).collect(),
            Source::Snorm => raw[..4].iter().map(|&b| b as i8 as f32 / 127.0).collect(),
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

/// A Legacy component type (CodeWalker's `VertexComponentType`) as a source.
fn legacy_source(nibble: u64) -> Option<Source> {
    Some(match nibble {
        1 => Source::Halves(2),
        2 => Source::Floats(1),
        3 => Source::Halves(4),
        5 => Source::Floats(2),
        6 => Source::Floats(3),
        7 => Source::Floats(4),
        8 | 9 => Source::Bytes(4),
        10 => Source::Snorm,
        _ => return None,
    })
}

fn gen9_source(format: u8) -> Option<Source> {
    Some(match format {
        6 => Source::Floats(3),
        2 => Source::Floats(4),
        10 => Source::Halves(4),
        16 => Source::Floats(2),
        34 => Source::Halves(2),
        0x1C..=0x1E => Source::Bytes(4),
        _ => return None,
    })
}

/// Whether every component (bit, source) is stored as a declaration type has it.
fn declaration_fits(types: u64, parts: &[(usize, Source, usize)]) -> bool {
    parts.iter().all(|&(bit, source, _)| legacy_source((types >> (bit * 4)) & 0xF) == Some(source))
}

/// A Legacy declaration type by name, or as CodeWalker writes one it does not name: the number.
fn declaration_name(types: u64) -> String {
    DECLARATIONS.iter().find(|d| d.1 == types).map_or_else(|| types.to_string(), |d| d.0.to_string())
}

/// The declaration type to write, the components present (as bits) and the values
/// of each vertex. Vertices are written as GTAV1 when their values fit it (whatever
/// they are stored as), else in the cloth types GTAV2 to GTAV4 that store them so.
fn read_vertices(res: &Resource, vb: u64, enhanced: bool, budget: &mut Budget) -> Result<(&'static str, u32, Vertices), Error> {
    let unsupported = || Error::Unsupported("this vertex layout".into());
    // Each component: its GTAV1 bit, source, and byte offset in the vertex; and the Legacy declaration type.
    let (count, stride, data, mut parts, types) = if enhanced {
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
        (count, stride, data, parts, None)
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
        (count, stride, data, parts, Some(types))
    };
    let cloth = |types: u64| DECLARATIONS[1..].iter().find(|d| d.1 == types).map(|d| d.0);
    let kind = match types.and_then(cloth) {
        // A Legacy cloth declaration keeps its type; others become GTAV1, Enhanced ones a type that stores them.
        Some(kind) => kind,
        None if parts.iter().all(|&(bit, source, _)| gtav1_accepts(bit, source)) => "GTAV1",
        None if types.is_none() => DECLARATIONS[1..].iter().find(|d| declaration_fits(d.1, &parts)).ok_or_else(unsupported)?.0,
        None => return Err(unsupported()),
    };
    let mut layout = 0u32;
    for &(bit, source, at) in &parts {
        if at + source.size() > stride {
            return Err(unsupported());
        }
        layout |= 1 << bit;
    }
    if layout & 1 == 0 {
        return Err(unsupported());
    }
    parts.sort_by_key(|p| p.0);
    if count == 0 {
        return Ok((kind, layout, Vec::new()));
    }
    budget.vertices = budget.vertices.checked_sub(count).ok_or_else(|| Error::Format("too many vertices".into()))?;
    let raw = res.block(data, stride * count)?;
    let vertices = raw.chunks_exact(stride).map(|v| parts.iter().map(|&(_, source, at)| source.read(&v[at..])).collect()).collect();
    Ok((kind, layout, vertices))
}

/// Formats a float as CodeWalker reads it back (`NaN` and the infinities by name).
pub(crate) fn fl(v: f32) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        v.to_string()
    }
}

/// The folder under the export folder that holds the textures of a model `stem`. A stem that would
/// leave the export folder (`""`, `.`, `..`) gets `textures` instead.
pub fn texture_folder(stem: &str) -> String {
    if matches!(stem, "" | "." | "..") { "textures".into() } else { stem.into() }
}

/// Escapes text for XML, quotes included; control characters, which XML cannot hold, are left out.
fn escape(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r')).collect();
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `x="" y="" z=""` (and `w` for four values), with a leading space.
pub(crate) fn attrs(v: &[f32]) -> String {
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

    /// CodeWalker's `WriteRawArray`: `<name />` for no items, one line for up to `row` of them, else rows of `row`.
    fn raw(&mut self, name: &str, items: &[String], row: usize) {
        if items.is_empty() {
            self.empty(name, "");
        } else if items.len() <= row {
            self.line(&format!("<{name}>{}</{name}>", items.join(" ")));
        } else {
            self.open(name);
            self.rows(items, row);
            self.close(name);
        }
    }

    /// CodeWalker's `WriteRawArrayContent`: the items in rows of `row`, at the current depth.
    fn rows(&mut self, items: &[String], row: usize) {
        for chunk in items.chunks(row) {
            self.line(&chunk.join(" "));
        }
    }

    /// Text written from indentation 0 (an element of [`crate::bounds_xml`]), at the current depth.
    fn block(&mut self, text: &str) {
        for line in text.lines() {
            self.line(line);
        }
    }

    /// A drawable's elements: CodeWalker's `Drawable.WriteXml`, or `FragDrawable.WriteXml` with [`FragFields`].
    fn drawable(&mut self, d: &Drawable) {
        self.text("Name", &d.name);
        if let Some(frag) = &d.frag {
            self.raw("Matrix", &frag.matrix.map(fl), 3);
            if let Some((capacity, used)) = &frag.matrices {
                self.open(&format!("Matrices capacity=\"{capacity}\""));
                for (id, matrix) in used {
                    self.open(&format!("Item id=\"{id}\""));
                    self.rows(&matrix.map(fl), 3);
                    self.close("Item");
                }
                self.close("Matrices");
            }
        }
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
        if let Some(joints) = &d.joints {
            self.joints(joints);
        }
        for (name, lod) in ["DrawableModelsHigh", "DrawableModelsMedium", "DrawableModelsLow", "DrawableModelsVeryLow"].iter().zip(&d.lods) {
            if let Some(models) = lod {
                self.models(name, models);
            }
        }
        self.block(&d.bounds);
        if let Some(lights) = &d.lights {
            self.lights(lights);
        }
    }

    fn shader_group(&mut self, d: &Drawable) {
        self.open("ShaderGroup");
        if let Some(textures) = &d.embedded {
            if textures.is_empty() {
                self.empty("TextureDictionary", "");
            } else {
                self.open("TextureDictionary");
                for e in textures {
                    let tex = &e.texture;
                    self.open("Item");
                    self.text("Name", &tex.name);
                    self.value("Unk32", e.unk32);
                    self.text("Usage", &usage_name(e.usage));
                    self.text("UsageFlags", &usage_flags(e.usage));
                    self.value("ExtraFlags", e.extra);
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
                // Always written: CodeWalker fails to save a shader without it.
                if shader.params.is_empty() {
                    self.empty("Parameters", "");
                } else {
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
            Value::Missing(kind) => self.empty("Item", &format!(" name=\"{name}\" type=\"{kind}\"")),
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

    fn joints(&mut self, joints: &Joints) {
        self.open("Joints");
        if let Some(limits) = &joints.rotation {
            self.open("RotationLimits");
            for (bone, unknown, min, max) in limits {
                self.open("Item");
                self.value("BoneId", bone);
                self.value("UnknownA", unknown);
                self.empty("Min", &attrs(min));
                self.empty("Max", &attrs(max));
                self.close("Item");
            }
            self.close("RotationLimits");
        }
        if let Some(limits) = &joints.translation {
            self.open("TranslationLimits");
            for (bone, min, max) in limits {
                self.open("Item");
                self.value("BoneId", bone);
                self.empty("Min", &attrs(min));
                self.empty("Max", &attrs(max));
                self.close("Item");
            }
            self.close("TranslationLimits");
        }
        self.close("Joints");
    }

    /// Lights in the order of CodeWalker's `LightAttributes.WriteXml`, from their bytes.
    fn lights(&mut self, lights: &[Light]) {
        if lights.is_empty() {
            self.empty("Lights", "");
            return;
        }
        self.open("Lights");
        for light in lights {
            let b = &light.raw[..];
            let f = |at: usize| fl(f32_at(b, at));
            let rgb = |at: usize| format!(" r=\"{}\" g=\"{}\" b=\"{}\"", b[at], b[at + 1], b[at + 2]);
            let kind = match b[0x26] {
                1 => "Point".to_string(),
                2 => "Spot".to_string(),
                4 => "Capsule".to_string(),
                n => n.to_string(),
            };
            self.open("Item");
            self.empty("Position", &attrs(&vec3_at(b, 0x08)));
            self.empty("Colour", &rgb(0x18));
            self.value("Flashiness", b[0x1B]);
            self.value("Intensity", f(0x1C));
            self.value("Flags", u32_at(b, 0x20));
            self.value("BoneId", u16_at(b, 0x24));
            self.text("Type", &kind);
            self.value("GroupId", b[0x27]);
            self.value("TimeFlags", u32_at(b, 0x28));
            self.value("Falloff", f(0x2C));
            self.value("FalloffExponent", f(0x30));
            self.empty("CullingPlaneNormal", &attrs(&vec3_at(b, 0x34)));
            self.value("CullingPlaneOffset", f(0x40));
            self.value("Unknown45", b[0x45]);
            self.value("Unknown46", u16_at(b, 0x46));
            self.value("VolumeIntensity", f(0x4C));
            self.value("VolumeSizeScale", f(0x50));
            self.empty("VolumeOuterColour", &rgb(0x54));
            self.value("LightHash", b[0x57]);
            self.value("VolumeOuterIntensity", f(0x58));
            self.value("CoronaSize", f(0x5C));
            self.value("VolumeOuterExponent", f(0x60));
            self.value("LightFadeDistance", b[0x64]);
            self.value("ShadowBlur", b[0x44]);
            self.value("ShadowFadeDistance", b[0x65]);
            self.value("SpecularFadeDistance", b[0x66]);
            self.value("VolumetricFadeDistance", b[0x67]);
            self.value("ShadowNearClip", f(0x68));
            self.value("CoronaIntensity", f(0x6C));
            self.value("CoronaZBias", f(0x70));
            self.empty("Direction", &attrs(&vec3_at(b, 0x74)));
            self.empty("Tangent", &attrs(&vec3_at(b, 0x80)));
            self.value("ConeInnerAngle", f(0x8C));
            self.value("ConeOuterAngle", f(0x90));
            self.empty("Extent", &attrs(&vec3_at(b, 0x94)));
            self.text("ProjectedTextureHash", &light.projected);
            self.close("Item");
        }
        self.close("Lights");
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
        self.line(&format!("<Layout type=\"{}\">", g.kind));
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
        self.raw("Data", &g.indices.iter().map(u16::to_string).collect::<Vec<_>>(), 24);
        self.close("IndexBuffer");
        self.close("Item");
    }
}

/// A texture usage by name (the number when it has none).
fn usage_name(usage: u32) -> String {
    let usage = usage & 0x1F;
    USAGES.get(usage as usize).map_or_else(|| usage.to_string(), |n| n.to_string())
}

/// Texture usage flags as .NET writes a flags enum: names joined by ", ", or the
/// number when it is 0 or has bits without a name.
fn usage_flags(usage: u32) -> String {
    let flags = usage >> 5;
    if flags == 0 || flags >> USAGE_FLAGS.len() != 0 {
        return flags.to_string();
    }
    (0..USAGE_FLAGS.len()).filter(|&i| flags & (1 << i) != 0).map(|i| USAGE_FLAGS[i]).collect::<Vec<_>>().join(", ")
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
        Geometry { shader: 0, bone_ids: vec![], min: [0.0; 3], max: [1.0; 3], kind: "GTAV1", layout: 1 | 16 | 64, vertices, indices }
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
            joints: None,
            lods: [Some(vec![model]), None, None, None],
            bounds: String::new(),
            lights: None,
            frag: None,
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
    fn enhanced_parameters_take_their_legacy_names() {
        let (names, h) = (Names::new(), joaat_lower);
        assert_eq!(SHADERS.param_name(SHADERS.legacy_param(h("blend_2lyr"), h("Specular")), &names), "specularFalloffMult");
        assert_eq!(SHADERS.param_name(SHADERS.legacy_param(h("normal_spec"), h("DiffuseTex")), &names), "DiffuseSampler");
        // An exception of the shader, and a shader the table does not know.
        assert_eq!(SHADERS.legacy_param(h("mirror_default"), h("Specular")), h("Specular"));
        assert_eq!(SHADERS.legacy_param(h("no_such_shader"), h("DiffuseTex")), h("DiffuseTex"));
        assert_eq!(SHADERS.shader_name(h("silhouettelayer"), &names), "silhouettelayer");
        assert_eq!(SHADERS.file_name(h("normal_spec.sps"), &names), "normal_spec.sps");
    }

    #[test]
    fn joints_lights_and_empty_parameters_are_written() {
        let mut d = drawable("prop_x", vec![0, 1, 1]);
        d.has_group = true;
        d.shaders = vec![Shader { name: "default".into(), file: "default.sps".into(), bucket: 0, params: vec![] }];
        d.joints = Some(Joints { rotation: Some(vec![(5, 0, [-1.0; 3], [1.0; 3])]), translation: None });
        let mut raw = vec![0u8; 0xA8];
        raw[0x26] = 2;
        d.lights = Some(vec![Light { raw, projected: String::new() }]);
        let xml = write(&[d], false);
        assert!(xml.contains("<Parameters />"), "{xml}");
        assert!(xml.contains("<Joints>\n  <RotationLimits>\n   <Item>\n    <BoneId value=\"5\" />"), "{xml}");
        assert!(!xml.contains("TranslationLimits"), "{xml}");
        assert!(xml.contains("<Type>Spot</Type>"), "{xml}");
        assert!(xml.find("</DrawableModelsHigh>").unwrap() < xml.find("<Lights>").unwrap(), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn floats_read_back_exactly() {
        assert_eq!(fl(0.5), "0.5");
        assert_eq!(fl(f32::NAN), "NaN");
        assert_eq!(fl(f32::NEG_INFINITY), "-Infinity");
        assert_eq!(bone_flags(0), "None");
        assert_eq!(bone_flags(0x1 | 0x10), "RotX, TransX");
        assert_eq!(usage_name(1 | 0x20), "DEFAULT");
        assert_eq!(usage_flags(1), "0");
        assert_eq!(usage_flags(((1 << 24) | (1 << 4)) << 5 | 22), "Y4, UNK24");
        assert_eq!(usage_flags(1 << 30), "33554432");
    }

    #[test]
    fn bounds_go_between_the_models_and_the_lights() {
        let mut d = drawable("prop_x", vec![0, 1, 1]);
        d.bounds = "<Bounds type=\"Box\">\n <Margin value=\"0.04\" />\n</Bounds>\n".into();
        d.lights = Some(Vec::new());
        let xml = write(&[d], true);
        assert!(xml.contains("  </DrawableModelsHigh>\n  <Bounds type=\"Box\">\n   <Margin value=\"0.04\" />\n  </Bounds>\n  <Lights />\n"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn fragment_drawables_start_with_their_matrices() {
        let mut d = drawable("chassis", vec![0, 1, 1]);
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        d.frag = Some(FragFields { matrix: identity, matrices: Some((4, vec![(7, identity)])) });
        let mut xml = Xml { out: String::new(), depth: 0 };
        xml.drawable(&d);
        assert!(xml.out.starts_with("<Name>chassis</Name>\n<Matrix>\n 1 0 0\n 0 1 0\n 0 0 1\n 0 0 0\n</Matrix>\n<Matrices capacity=\"4\">\n <Item id=\"7\">\n  1 0 0\n"), "{}", xml.out);
        assert!(xml.out.contains("  0 0 0\n </Item>\n</Matrices>\n<BoundingSphereCenter"), "{}", xml.out);
        assert!(!xml.out.contains("Lights"), "{}", xml.out);
    }

    #[test]
    fn raw_arrays_wrap_like_codewalker() {
        let mut xml = Xml { out: String::new(), depth: 1 };
        let items: Vec<String> = (0..5).map(|i| i.to_string()).collect();
        xml.raw("A", &items, 10);
        xml.raw("B", &items, 2);
        xml.raw("C", &[], 2);
        assert_eq!(xml.out, " <A>0 1 2 3 4</A>\n <B>\n  0 1\n  2 3\n  4\n </B>\n <C />\n");
    }

    /// A Legacy vertex buffer at 0x5000_0000 + 0x40 with two vertices of `stride` bytes at 0x100.
    fn vertex_buffer(flags: u32, types: u64, stride: usize, vertices: &[u8]) -> Resource {
        let mut s = vec![0u8; 0x200];
        let mut put = |at: usize, bytes: &[u8]| s[at..at + bytes.len()].copy_from_slice(bytes);
        put(0x48, &(stride as u16).to_le_bytes());
        put(0x50, &(ROOT + 0x100).to_le_bytes());
        put(0x58, &2u32.to_le_bytes());
        put(0x70, &(ROOT + 0x20).to_le_bytes());
        put(0x20, &flags.to_le_bytes());
        put(0x28, &types.to_le_bytes());
        put(0x100, vertices);
        Resource { version: 162, system: s, graphics: Vec::new() }
    }

    #[test]
    fn cloth_vertices_keep_their_declaration() {
        // GTAV2: Position (Float3), Normal (RGBA8SNorm), Colour0 (bytes), TexCoord0 (Half2), Tangent (Half4).
        let flags = 1 | 1 << 3 | 1 << 4 | 1 << 6 | 1 << 14;
        let mut v = Vec::new();
        for i in 0..2u8 {
            v.extend([1.0f32, 2.0, i as f32].iter().flat_map(|f| f.to_le_bytes()));
            v.extend([127, 0x81, 0, 0]);
            v.extend([10, 20, 30, 255]);
            v.extend([0x3800u16, 0x3C00, 0, 0x3C00, 0xBC00, 0x3400].iter().flat_map(|h| h.to_le_bytes()));
        }
        let res = vertex_buffer(flags, 0x0300_0000_0199_A006, 32, &v);
        let (kind, layout, vertices) = read_vertices(&res, ROOT + 0x40, false, &mut Budget::default()).unwrap();
        assert_eq!((kind, layout), ("GTAV2", flags));
        assert_eq!(vertices[1], [vec![1.0, 2.0, 1.0], vec![1.0, -1.0, 0.0, 0.0], vec![10.0, 20.0, 30.0, 255.0], vec![0.5, 1.0], vec![0.0, 1.0, -1.0, 0.25]]);
        // The same values in an unnamed declaration still become GTAV1 where they fit, and are refused where not.
        assert!(read_vertices(&vertex_buffer(flags, 0x0300_0000_0199_A007, 32, &v), ROOT + 0x40, false, &mut Budget::default()).is_err());
        let gtav3 = vertex_buffer(1 | 1 << 6, 0x0300_0000_0199_6006, 16, &[0; 32]);
        assert_eq!(read_vertices(&gtav3, ROOT + 0x40, false, &mut Budget::default()).unwrap().0, "GTAV3");
        let gtav1 = vertex_buffer(1 | 1 << 6, 0x7755_5555_5599_6996, 20, &[0; 40]);
        assert_eq!(read_vertices(&gtav1, ROOT + 0x40, false, &mut Budget::default()).unwrap().0, "GTAV1");
    }
}
