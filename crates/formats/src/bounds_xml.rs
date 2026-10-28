//! Collision bounds as CodeWalker XML: a `.ybn` as the `.ybn.xml` that
//! CodeWalker and Sollumz import, and the `<Bounds>` element of a model's
//! embedded collision. Layouts and element order follow CodeWalker's
//! `Bounds.cs` (MIT), its `WriteXml` methods and `YbnXml`.
//!
//! Geometry vertices are written as CodeWalker keeps them: the quantised
//! values times the quantum, relative to `GeometryCenter`. BVH trees and
//! octants are not written; CodeWalker builds them again on import.

use crate::Error;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};
use crate::xml_export::{attrs, f32_at, fl, vec3_at as vec3};

const ROOT: u64 = 0x5000_0000;
/// Composites nested deeper than this are refused (a damaged one may point back at itself).
const MAX_DEPTH: usize = 8;
/// Bounds and items (vertices, polygons, materials, colours) written at most in one export.
const MAX_VISITS: usize = 100_000;
const MAX_ITEMS: usize = 32_000_000;
/// Composite child flags by bit (CodeWalker's `EBoundCompositeFlags`).
const COMPOSITE_FLAGS: [&str; 32] = [
    "UNKNOWN", "MAP_WEAPON", "MAP_DYNAMIC", "MAP_ANIMAL", "MAP_COVER", "MAP_VEHICLE", "VEHICLE_NOT_BVH", "VEHICLE_BVH", "VEHICLE_BOX", "PED", "RAGDOLL", "ANIMAL", "ANIMAL_RAGDOLL", "OBJECT", "OBJECT_ENV_CLOTH", "PLANT",
    "PROJECTILE", "EXPLOSION", "PICKUP", "FOLIAGE", "FORKLIFT_FORKS", "TEST_WEAPON", "TEST_CAMERA", "TEST_AI", "TEST_SCRIPT", "TEST_VEHICLE_WHEEL", "GLASS", "MAP_RIVER", "SMOKE", "UNSMASHED", "MAP_STAIRS", "MAP_DEEP_SURFACE",
];
/// Material flags by bit (CodeWalker's `EBoundMaterialFlags`).
const MATERIAL_FLAGS: [&str; 16] = [
    "FLAG_STAIRS", "FLAG_NOT_CLIMBABLE", "FLAG_SEE_THROUGH", "FLAG_SHOOT_THROUGH", "FLAG_NOT_COVER", "FLAG_WALKABLE_PATH", "FLAG_NO_CAM_COLLISION", "FLAG_SHOOT_THROUGH_FX", "FLAG_NO_DECAL", "FLAG_NO_NAVMESH", "FLAG_NO_RAGDOLL", "FLAG_VEHICLE_WHEEL", "FLAG_NO_PTFX",
    "FLAG_TOO_STEEP_FOR_PLAYER", "FLAG_NO_NETWORK_SPAWN", "FLAG_NO_CAM_COLLISION_ALLOW_CLIPPING",
];
/// A composite child's transformation as CodeWalker writes it: four rows of four.
type Rows = [[f32; 4]; 4];
const IDENTITY: Rows = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// A whole `.ybn` as CodeWalker's `.ybn.xml`.
pub fn export_ybn(data: &[u8]) -> Result<String, Error> {
    write_ybn(&Resource::parse(data)?)
}

fn write_ybn(res: &Resource) -> Result<String, Error> {
    let mut w = Writer::new(res, 0, false);
    w.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    w.open("BoundsFile");
    w.bound(ROOT, "Bounds", None, 0)?;
    w.close("BoundsFile");
    Ok(w.out)
}

/// The `<Bounds type="…">` element of a drawable's embedded bound at `ptr`, indented
/// one space per level from `depth` spaces and ending with a newline; empty when `ptr` is 0.
pub(crate) fn bound_xml(res: &Resource, ptr: u64, depth: usize) -> Result<String, Error> {
    element(res, ptr, depth, false)
}

/// The same for a bound owned by a fragment (its drawable, physics archetype or
/// physics level of detail): CodeWalker leaves out the composite flags of its children.
pub(crate) fn fragment_bound_xml(res: &Resource, ptr: u64, depth: usize) -> Result<String, Error> {
    element(res, ptr, depth, true)
}

fn element(res: &Resource, ptr: u64, depth: usize, fragment: bool) -> Result<String, Error> {
    if ptr == 0 {
        return Ok(String::new());
    }
    let mut w = Writer::new(res, depth, fragment);
    w.bound(ptr, "Bounds", None, 0)?;
    Ok(w.out)
}

/// Where a composite child sits: its transformation, and its flags (`None` under a fragment's root).
struct Placement {
    rows: Rows,
    flags: Option<(u32, u32)>,
}

fn type_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0 => "Sphere",
        1 => "Capsule",
        3 => "Box",
        4 => "Geometry",
        8 => "GeometryBVH",
        10 => "Composite",
        12 => "Disc",
        13 => "Cylinder",
        15 => "Cloth",
        _ => return None,
    })
}

/// `count` items of `size` bytes at `ptr`, all of them or an error.
fn array(res: &Resource, ptr: u64, count: usize, size: usize) -> Result<&[u8], Error> {
    let len = count.checked_mul(size).ok_or_else(|| Error::Format("collision array is too large".into()))?;
    res.block(ptr, len)
}

/// A .NET flags enum as text: `NONE`, or the names of the bits set, lowest first.
fn flags(value: u32, names: &[&str]) -> String {
    if value == 0 {
        return "NONE".into();
    }
    (0..names.len()).filter(|&i| value & (1 << i) != 0).map(|i| names[i]).collect::<Vec<_>>().join(", ")
}

/// A quantised vertex times the quantum, as `x, y, z`.
fn vertex(v: &[u8; 6], quantum: [f32; 3]) -> String {
    let c = |i: usize| fl(i16::from_le_bytes([v[i * 2], v[i * 2 + 1]]) as f32 * quantum[i]);
    format!("{}, {}, {}", c(0), c(1), c(2))
}

/// One polygon's element, or `None` for the types CodeWalker does not know (5 to 7).
fn polygon(p: &[u8], m: u8) -> Option<String> {
    let u = |at: usize| u16_at(p, at);
    let radius = fl(f32_at(p, 4));
    Some(match p[0] & 7 {
        0 => {
            let (a, b, c) = (u(4), u(6), u(8));
            format!("<Triangle m=\"{m}\" v1=\"{}\" v2=\"{}\" v3=\"{}\" f1=\"{}\" f2=\"{}\" f3=\"{}\" />", a & 0x7FFF, b & 0x7FFF, c & 0x7FFF, a >> 15, b >> 15, c >> 15)
        }
        1 => format!("<Sphere m=\"{m}\" v=\"{}\" radius=\"{radius}\" />", u(2)),
        2 => format!("<Capsule m=\"{m}\" v1=\"{}\" v2=\"{}\" radius=\"{radius}\" />", u(2), u(8)),
        3 => format!("<Box m=\"{m}\" v1=\"{}\" v2=\"{}\" v3=\"{}\" v4=\"{}\" />", u(4) as i16, u(6) as i16, u(8) as i16, u(10) as i16),
        4 => format!("<Cylinder m=\"{m}\" v1=\"{}\" v2=\"{}\" radius=\"{radius}\" />", u(2), u(8)),
        _ => return None,
    })
}

/// Writes CodeWalker's XML with one space of indentation per level.
struct Writer<'a> {
    res: &'a Resource,
    out: String,
    depth: usize,
    /// Whether the root bound belongs to a fragment.
    fragment: bool,
    visits: usize,
    items: usize,
}

impl<'a> Writer<'a> {
    fn new(res: &'a Resource, depth: usize, fragment: bool) -> Self {
        Writer { res, out: String::new(), depth, fragment, visits: 0, items: 0 }
    }

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

    fn value(&mut self, name: &str, value: impl std::fmt::Display) {
        self.empty(name, &format!(" value=\"{value}\""));
    }

    /// Text that never needs escaping (numbers and flag names).
    fn text(&mut self, name: &str, text: &str) {
        self.line(&format!("<{name}>{text}</{name}>"));
    }

    /// One item per line, or a single item inline (CodeWalker's raw arrays of row size 1).
    fn rows(&mut self, name: &str, rows: &[String]) {
        if let [row] = rows {
            self.text(name, row);
        } else {
            self.open(name);
            for row in rows {
                self.line(row);
            }
            self.close(name);
        }
    }

    fn spend(&mut self, items: usize) -> Result<(), Error> {
        self.items = self.items.saturating_add(items);
        if self.items > MAX_ITEMS {
            return Err(Error::Format("collision bounds are too large".into()));
        }
        Ok(())
    }

    /// One bound as the element `name`; `place` is set for a composite's child.
    fn bound(&mut self, ptr: u64, name: &str, place: Option<&Placement>, nesting: usize) -> Result<(), Error> {
        if nesting > MAX_DEPTH {
            return Err(Error::Format("collision bounds nest too deep".into()));
        }
        self.visits += 1;
        if self.visits > MAX_VISITS {
            return Err(Error::Format("too many collision bounds".into()));
        }
        let res = self.res;
        let h = res.block(ptr, 0x70)?;
        let kind = h[0x10];
        let Some(type_name) = type_name(kind) else {
            if place.is_some() {
                self.empty(name, " type=\"None\"");
                return Ok(());
            }
            return Err(Error::Unsupported(format!("collision bound type {kind}")));
        };
        self.line(&format!("<{name} type=\"{type_name}\">"));
        self.depth += 1;
        self.header(h, place);
        match kind {
            4 | 8 => self.geometry(ptr)?,
            10 => self.composite(ptr, nesting)?,
            _ => {}
        }
        self.close(name);
        Ok(())
    }

    /// The fields every bound has (CodeWalker's `Bounds.WriteXml`).
    fn header(&mut self, h: &[u8], place: Option<&Placement>) {
        self.empty("BoxMin", &attrs(&vec3(h, 0x30)));
        self.empty("BoxMax", &attrs(&vec3(h, 0x20)));
        self.empty("BoxCenter", &attrs(&vec3(h, 0x40)));
        self.empty("SphereCenter", &attrs(&vec3(h, 0x50)));
        self.value("SphereRadius", fl(f32_at(h, 0x14)));
        self.value("Margin", fl(f32_at(h, 0x2C)));
        self.value("Volume", fl(f32_at(h, 0x6C)));
        self.empty("Inertia", &attrs(&vec3(h, 0x60)));
        self.value("MaterialIndex", h[0x4C]);
        self.value("MaterialColourIndex", h[0x5D]);
        self.value("ProceduralID", h[0x4D]);
        self.value("RoomID", h[0x4E] & 0x1F);
        self.value("PedDensity", h[0x4E] >> 5);
        self.value("UnkFlags", h[0x4F]);
        self.value("PolyFlags", h[0x5C]);
        self.value("UnkType", u32_at(h, 0x3C));
        if let Some(place) = place {
            self.open("CompositeTransform");
            for row in &place.rows {
                self.line(&row.map(fl).join(" "));
            }
            self.close("CompositeTransform");
            if let Some((first, second)) = place.flags {
                self.text("CompositeFlags1", &flags(first, &COMPOSITE_FLAGS));
                self.text("CompositeFlags2", &flags(second, &COMPOSITE_FLAGS));
            }
        }
    }

    /// Geometry and BVH geometry: materials, vertices, colours and polygons.
    fn geometry(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let g = res.block(ptr, 0x130)?;
        let quantum = vec3(g, 0x90);
        self.empty("GeometryCenter", &attrs(&vec3(g, 0xA0)));
        self.value("UnkFloat1", fl(f32_at(g, 0x9C)));
        self.value("UnkFloat2", fl(f32_at(g, 0xAC)));
        // CodeWalker writes the materials whenever the pointer is set, even none of them.
        let (materials, material_count) = (u64_at(g, 0xF0), g[0x120] as usize);
        if materials != 0 {
            if material_count == 0 {
                self.empty("Materials", "");
            } else {
                let list = array(res, materials, material_count, 8)?;
                self.spend(material_count)?;
                self.open("Materials");
                for m in list.as_chunks::<8>().0 {
                    self.material(u32_at(m, 0), u32_at(m, 4));
                }
                self.close("Materials");
            }
        }
        let colour_rows = |list: &[u8]| -> Vec<String> { list.as_chunks::<4>().0.iter().map(|c| format!("{}, {}, {}, {}", c[0], c[1], c[2], c[3])).collect() };
        let (colours, colour_count) = (u64_at(g, 0xF8), g[0x121] as usize);
        if colours != 0 && colour_count != 0 {
            let list = array(res, colours, colour_count, 4)?;
            self.spend(colour_count)?;
            self.rows("MaterialColours", &colour_rows(list));
        }
        let (vertices, vertex_count) = (u64_at(g, 0xB0), u32_at(g, 0xD0) as usize);
        if vertices != 0 && vertex_count != 0 {
            let raw = array(res, vertices, vertex_count, 6)?;
            self.spend(vertex_count)?;
            let rows: Vec<String> = raw.as_chunks::<6>().0.iter().map(|v| vertex(v, quantum)).collect();
            self.rows("Vertices", &rows);
        }
        let vertex_colours = u64_at(g, 0xB8);
        if vertex_colours != 0 && vertex_count != 0 {
            let list = array(res, vertex_colours, vertex_count, 4)?;
            self.spend(vertex_count)?;
            self.rows("VertexColours", &colour_rows(list));
        }
        let polygon_count = u32_at(g, 0xD4) as usize;
        if polygon_count != 0 {
            let polygons = array(res, u64_at(g, 0x88), polygon_count, 16)?;
            self.spend(polygon_count)?;
            // Each polygon's index into the materials; all 0 without the list.
            let indices = match u64_at(g, 0x118) {
                0 => None,
                p => Some(array(res, p, polygon_count, 1)?),
            };
            self.open("Polygons");
            for (i, p) in polygons.as_chunks::<16>().0.iter().enumerate() {
                if let Some(line) = polygon(p, indices.map_or(0, |m| m[i])) {
                    self.line(&line);
                }
            }
            self.close("Polygons");
        }
        Ok(())
    }

    /// A material's two packed words, field by field (CodeWalker's `BoundMaterial_s`).
    fn material(&mut self, d1: u32, d2: u32) {
        self.open("Item");
        self.value("Type", d1 & 0xFF);
        self.value("ProceduralID", (d1 >> 8) & 0xFF);
        self.value("RoomID", (d1 >> 16) & 0x1F);
        self.value("PedDensity", (d1 >> 21) & 0x7);
        self.text("Flags", &flags(((d1 >> 24) & 0xFF) | ((d2 & 0xFF) << 8), &MATERIAL_FLAGS));
        self.value("MaterialColourIndex", (d2 >> 8) & 0xFF);
        self.value("Unk", d2 >> 16);
        self.close("Item");
    }

    /// A composite's children, each with its transformation and flags.
    fn composite(&mut self, ptr: u64, nesting: usize) -> Result<(), Error> {
        let res = self.res;
        let c = res.block(ptr, 0xB0)?;
        let (children, count) = (u64_at(c, 0x70), u16_at(c, 0xA0) as usize);
        if children == 0 || count == 0 {
            self.empty("Children", "");
            return Ok(());
        }
        let list = array(res, children, count, 8)?;
        // The first transformation list, or the second when the first is missing.
        let transforms = [u64_at(c, 0x78), u64_at(c, 0x80)].into_iter().find(|&p| p != 0).map(|p| array(res, p, count, 64)).transpose()?;
        let child_flags = match u64_at(c, 0x90) {
            0 => None,
            p => Some(array(res, p, count, 8)?),
        };
        let with_flags = !(self.fragment && nesting == 0);
        self.open("Children");
        for i in 0..count {
            let child = u64_at(list, i * 8);
            if child == 0 {
                self.empty("Item", " type=\"None\"");
                continue;
            }
            // Four columns of three floats, each padded to 16 bytes; CodeWalker writes them as rows of a 4x4 matrix.
            let rows = transforms.map_or(IDENTITY, |t| std::array::from_fn(|r| {
                let [x, y, z] = vec3(t, i * 64 + r * 16);
                [x, y, z, if r == 3 { 1.0 } else { 0.0 }]
            }));
            // CodeWalker writes the first flags list's two words, as CompositeFlags1 and 2.
            let flags = with_flags.then(|| child_flags.map_or((0, 0), |f| (u32_at(f, i * 8), u32_at(f, i * 8 + 4))));
            self.bound(child, "Item", Some(&Placement { rows, flags }), nesting + 1)?;
        }
        self.close("Children");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = ROOT;

    fn put(buf: &mut Vec<u8>, at: usize, bytes: &[u8]) {
        if buf.len() < at + bytes.len() {
            buf.resize(at + bytes.len(), 0);
        }
        buf[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn put_f32s(buf: &mut Vec<u8>, at: usize, values: &[f32]) {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        put(buf, at, &bytes);
    }

    /// A composite of a box, a geometry and a missing child.
    fn sample() -> Resource {
        let mut s = vec![0; 0x700];
        // The composite: three children, one transformation list, one flags list.
        s[0x10] = 10;
        put(&mut s, 0x70, &(S + 0x200).to_le_bytes());
        put(&mut s, 0x78, &(S + 0x220).to_le_bytes());
        put(&mut s, 0x90, &(S + 0x2E0).to_le_bytes());
        put(&mut s, 0xA0, &[3, 0, 3, 0]);
        put(&mut s, 0x200, &[(S + 0x300).to_le_bytes(), (S + 0x400).to_le_bytes(), [0; 8]].concat());
        for i in 0..3 {
            let at = 0x220 + i * 64;
            put_f32s(&mut s, at, &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0]);
        }
        put(&mut s, 0x2E0, &[(1u32 | 1 << 13).to_le_bytes(), [0; 4]].concat());
        // A box.
        s[0x310] = 3;
        put_f32s(&mut s, 0x314, &[1.5]);
        put_f32s(&mut s, 0x320, &[1.0, 1.0, 1.0, 0.04, -1.0, -1.0, -1.0]);
        s[0x34E] = 3 | 2 << 5;
        // A geometry of three vertices, a triangle and a sphere.
        s[0x410] = 4;
        put(&mut s, 0x488, &(S + 0x640).to_le_bytes());
        put_f32s(&mut s, 0x490, &[0.5, 0.5, 0.5, 0.0, 10.0, 0.0, 0.0]);
        put(&mut s, 0x4B0, &(S + 0x600).to_le_bytes());
        put(&mut s, 0x4D0, &[3, 0, 0, 0, 2, 0, 0, 0]);
        put(&mut s, 0x4F0, &[(S + 0x680).to_le_bytes(), (S + 0x690).to_le_bytes()].concat());
        put(&mut s, 0x518, &(S + 0x6A0).to_le_bytes());
        put(&mut s, 0x520, &[1, 1]);
        let vertices: Vec<u8> = [2i16, 0, 0, 0, 4, 0, 0, 0, -6].iter().flat_map(|v| v.to_le_bytes()).collect();
        put(&mut s, 0x600, &vertices);
        put(&mut s, 0x644, &[0, 0, 1, 0x80, 2, 0]);
        put(&mut s, 0x650, &[1, 0, 1, 0]);
        put_f32s(&mut s, 0x654, &[0.25]);
        put(&mut s, 0x680, &[(5u32 | 1 << 24).to_le_bytes(), (2u32 << 8).to_le_bytes()].concat());
        put(&mut s, 0x690, &[255, 128, 0, 255]);
        put(&mut s, 0x6A0, &[0, 0]);
        Resource { version: 43, system: s, graphics: Vec::new() }
    }

    #[test]
    fn composites_list_their_children() {
        let xml = write_ybn(&sample()).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<BoundsFile>\n <Bounds type=\"Composite\">\n  <BoxMin x=\"0\" y=\"0\" z=\"0\" />\n"), "{xml}");
        assert!(xml.contains("\n  <Children>\n   <Item type=\"Box\">\n"), "{xml}");
        assert!(xml.contains("\n   <Item type=\"None\" />\n  </Children>\n </Bounds>\n</BoundsFile>\n"), "{xml}");
        assert!(xml.contains("<CompositeTransform>\n     1 0 0 0\n     0 1 0 0\n     0 0 1 0\n     1 2 3 1\n    </CompositeTransform>\n"), "{xml}");
        assert!(xml.contains("<CompositeFlags1>UNKNOWN, OBJECT</CompositeFlags1>\n    <CompositeFlags2>NONE</CompositeFlags2>"), "{xml}");
        assert!(xml.contains("<SphereRadius value=\"1.5\" />\n    <Margin value=\"0.04\" />"), "{xml}");
        assert!(xml.contains("<RoomID value=\"3\" />\n    <PedDensity value=\"2\" />"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn geometries_keep_their_quantised_vertices() {
        let xml = write_ybn(&sample()).unwrap();
        assert!(xml.contains("<Item type=\"Geometry\">"), "{xml}");
        assert!(xml.contains("<GeometryCenter x=\"10\" y=\"0\" z=\"0\" />"), "{xml}");
        assert!(xml.contains("<Vertices>\n     1, 0, 0\n     0, 2, 0\n     0, 0, -3\n    </Vertices>"), "{xml}");
        assert!(xml.contains("<Triangle m=\"0\" v1=\"0\" v2=\"1\" v3=\"2\" f1=\"0\" f2=\"1\" f3=\"0\" />"), "{xml}");
        assert!(xml.contains("<Sphere m=\"0\" v=\"1\" radius=\"0.25\" />"), "{xml}");
        assert!(xml.contains("<MaterialColours>255, 128, 0, 255</MaterialColours>"), "{xml}");
        assert!(xml.contains("<Type value=\"5\" />"), "{xml}");
        assert!(xml.contains("<Flags>FLAG_STAIRS</Flags>\n      <MaterialColourIndex value=\"2\" />"), "{xml}");
        assert!(!xml.contains("VertexColours"), "{xml}");
    }

    #[test]
    fn embedded_bounds_start_at_the_given_depth() {
        let res = sample();
        let xml = bound_xml(&res, S, 3).unwrap();
        assert!(xml.starts_with("   <Bounds type=\"Composite\">\n"), "{xml}");
        assert!(xml.ends_with("\n   </Bounds>\n"), "{xml}");
        assert!(xml.contains("CompositeFlags1"), "{xml}");
        // A fragment's children have no flags.
        assert!(!fragment_bound_xml(&res, S, 3).unwrap().contains("CompositeFlags1"));
        assert_eq!(bound_xml(&res, 0, 3).unwrap(), "");
    }

    #[test]
    fn damaged_bounds_are_refused() {
        // A composite that is its own child.
        let mut res = sample();
        put(&mut res.system, 0x200, &S.to_le_bytes());
        assert!(write_ybn(&res).is_err());
        // Vertices said to be far more than the data holds.
        let mut res = sample();
        put(&mut res.system, 0x4D0, &u32::MAX.to_le_bytes());
        assert!(write_ybn(&res).is_err());
        // An unknown root type.
        let mut res = sample();
        res.system[0x10] = 7;
        assert!(write_ybn(&res).is_err());
    }

    #[test]
    fn flags_read_like_dotnet() {
        assert_eq!(flags(0, &MATERIAL_FLAGS), "NONE");
        assert_eq!(flags(0x8001, &MATERIAL_FLAGS), "FLAG_STAIRS, FLAG_NO_CAM_COLLISION_ALLOW_CLIPPING");
        assert_eq!(flags(1 << 31, &COMPOSITE_FLAGS), "MAP_DEEP_SURFACE");
    }
}
