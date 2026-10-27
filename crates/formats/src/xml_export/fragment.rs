//! Fragments (`.yft`) as CodeWalker's `.yft.xml`: the elements and order of
//! `FragType.WriteXml` and the types it writes (`Frag.cs`, `Clothes.cs`, MIT),
//! read field by field from the resource.
//!
//! CodeWalker leaves out the main drawable when it is a cloth's (it is written
//! in `<Cloths>` instead), and the skeleton and bound of the drawables of the
//! physics children and the drawable array, which take the main drawable's.

use super::{Drawable, Xml, attrs, declaration_name, f32_at, fl, read_frag_drawable, read_lights, vec3_at};
use crate::Error;
use crate::bounds_xml::{bound_xml, fragment_bound_xml};
use crate::drawable::is_enhanced;
use crate::names::Names;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};

const ROOT: u64 = 0x5000_0000;

/// The XML of a fragment, and the drawables it holds (for their textures).
pub(super) fn write(res: &Resource, names: &Names) -> Result<(String, Vec<Drawable>), Error> {
    let mut w = Writer { res, names, xml: Xml { out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"), depth: 0 }, drawables: Vec::new(), parts_inherit: false };
    w.fragment()?;
    Ok((w.xml.out, w.drawables))
}

/// What a simple list (pointer, count) holds, and how each item is written.
#[derive(Clone, Copy)]
enum Item {
    F32,
    U32,
    U16,
    /// `x, y, z, w`.
    Vec4,
}

impl Item {
    fn size(self) -> usize {
        match self {
            Item::F32 | Item::U32 => 4,
            Item::U16 => 2,
            Item::Vec4 => 16,
        }
    }

    fn text(self, b: &[u8]) -> String {
        match self {
            Item::F32 => fl(f32_at(b, 0)),
            Item::U32 => u32_at(b, 0).to_string(),
            Item::U16 => u16_at(b, 0).to_string(),
            Item::Vec4 => vec4_text(b),
        }
    }
}

fn vec4_text(b: &[u8]) -> String {
    (0..4).map(|i| fl(f32_at(b, i * 4))).collect::<Vec<_>>().join(", ")
}

fn vec4_at(b: &[u8], at: usize) -> [f32; 4] {
    std::array::from_fn(|i| f32_at(b, at + i * 4))
}

/// `n` floats from `at`, formatted.
fn floats(b: &[u8], at: usize, n: usize) -> Vec<String> {
    (0..n).map(|i| fl(f32_at(b, at + i * 4))).collect()
}

/// A fixed-size name as CodeWalker's `FragPhysNameStruct_s` reads it: each byte a character, NULs left out.
fn group_name(b: &[u8]) -> String {
    b.iter().filter(|&&c| c != 0).map(|&c| c as char).collect()
}

/// A `PsoChar32` as CodeWalker reads it: ASCII ('?' for other bytes), NULs left out.
fn ascii_name(b: &[u8]) -> String {
    b.iter().filter(|&&c| c != 0).map(|&c| if c.is_ascii() { c as char } else { '?' }).collect()
}

/// A joint type as .NET writes CodeWalker's `FragJointType`.
fn joint_type(kind: u8) -> String {
    match kind {
        0 => "DOF1".into(),
        1 => "DOF3".into(),
        n => n.to_string(),
    }
}

/// A row of a vehicle window's shatter map as CodeWalker's `WindowShatterMapRow.WriteLine`
/// writes it: two characters a cell, `##` before the first run and after the last, `--`
/// between the two runs, the runs' bytes in upper-case hex, `width` cells in all (or more).
fn shatter_line(start1: u8, data1: Option<&[u8]>, start2: u8, data2: Option<&[u8]>, width: usize) -> String {
    let mut line = String::new();
    let mut at = 0;
    if let Some(data) = data1 {
        line.push_str(&"##".repeat(start1 as usize));
        data.iter().for_each(|b| line.push_str(&format!("{b:02X}")));
        at = start1 as usize + data.len();
    }
    if let Some(data) = data2 {
        line.push_str(&"--".repeat((start2 as usize).saturating_sub(at)));
        data.iter().for_each(|b| line.push_str(&format!("{b:02X}")));
        at = start2 as usize + data.len();
    }
    line.push_str(&"##".repeat(width.saturating_sub(at)));
    line
}

struct Writer<'a> {
    res: &'a Resource,
    names: &'a Names,
    xml: Xml,
    drawables: Vec<Drawable>,
    /// Whether the drawables of physics children and of the drawable array take the
    /// main drawable's skeleton and bound (CodeWalker's `AssignChildrenShaders`).
    parts_inherit: bool,
}

impl Writer<'_> {
    /// `count` pointers at `ptr` (none when `ptr` is 0).
    fn pointers(&self, ptr: u64, count: usize) -> Result<Vec<u64>, Error> {
        if ptr == 0 || count == 0 {
            return Ok(Vec::new());
        }
        Ok(self.res.block(ptr, count * 8)?.as_chunks::<8>().0.iter().map(|p| u64_at(p, 0)).collect())
    }

    /// A string by pointer, empty for none.
    fn string(&self, ptr: u64) -> Result<String, Error> {
        if ptr == 0 { Ok(String::new()) } else { self.res.string(ptr) }
    }

    /// The items of the simple list (pointer, count) at `at`; `None` when it has none,
    /// as CodeWalker reads an empty list.
    fn list(&self, b: &[u8], at: usize, item: Item) -> Result<Option<Vec<String>>, Error> {
        let (ptr, count) = (u64_at(b, at), u16_at(b, at + 8) as usize);
        if ptr == 0 || count == 0 {
            return Ok(None);
        }
        Ok(Some(self.res.block(ptr, count * item.size())?.chunks_exact(item.size()).map(|c| item.text(c)).collect()))
    }

    /// Simple lists as raw arrays, each as `name` when it has items.
    fn lists(&mut self, b: &[u8], lists: &[(&str, usize, Item)]) -> Result<(), Error> {
        for &(name, at, item) in lists {
            if let Some(items) = self.list(b, at, item)? {
                self.xml.raw(name, &items, if matches!(item, Item::Vec4) { 1 } else { 10 });
            }
        }
        Ok(())
    }

    fn values(&mut self, b: &[u8], floats: &[(&str, usize)]) {
        for &(name, at) in floats {
            self.xml.value(name, fl(f32_at(b, at)));
        }
    }

    fn vectors(&mut self, b: &[u8], vectors: &[(&str, usize)]) {
        for &(name, at) in vectors {
            self.xml.empty(name, &attrs(&vec3_at(b, at)));
        }
    }

    /// A fragment drawable as the element `tag`.
    fn drawable(&mut self, tag: &str, ptr: u64, name: Option<String>, part: bool) -> Result<(), Error> {
        let d = read_frag_drawable(self.res, ptr, name, part, self.names)?;
        self.xml.open(tag);
        self.xml.drawable(&d);
        self.xml.close(tag);
        self.drawables.push(d);
        Ok(())
    }

    /// CodeWalker's `FragType.WriteXml`.
    fn fragment(&mut self) -> Result<(), Error> {
        let res = self.res;
        let f = res.block(ROOT, 0x130)?;
        let (drawable, group, cloth_drawable) = (u64_at(f, 0x30), u64_at(f, 0xF0), u64_at(f, 0xF8));
        self.parts_inherit = group != 0 && (drawable != 0 || cloth_drawable != 0);
        let cloths = self.pointers(u64_at(f, 0x60), u16_at(f, 0x68) as usize)?;
        let mut cloth_drawables = Vec::new();
        for &c in cloths.iter().filter(|&&c| c != 0) {
            cloth_drawables.push(u64_at(res.block(c, 0x80)?, 0x18));
        }
        self.xml.open("Fragment");
        let name = self.string(u64_at(f, 0x58))?;
        self.xml.text("Name", &name);
        self.xml.empty("BoundingSphereCenter", &attrs(&vec3_at(f, 0x20)));
        self.xml.value("BoundingSphereRadius", fl(f32_at(f, 0x2C)));
        for (name, at) in [("UnknownB0", 0xB0), ("UnknownB8", 0xB8), ("UnknownBC", 0xBC), ("UnknownC0", 0xC0), ("UnknownC4", 0xC4)] {
            self.xml.value(name, u32_at(f, at) as i32);
        }
        self.values(f, &[("UnknownCC", 0xCC), ("GravityFactor", 0xD0), ("BuoyancyFactor", 0xD4)]);
        if drawable != 0 && !cloth_drawables.contains(&drawable) {
            self.drawable("Drawable", drawable, None, false)?;
        }
        let count = u32_at(f, 0x48) as usize;
        let array = self.pointers(u64_at(f, 0x38), count)?;
        if !array.is_empty() {
            let array_names = self.pointers(u64_at(f, 0x40), count)?;
            self.xml.open("DrawableArray");
            for (i, &d) in array.iter().enumerate().filter(|(_, d)| **d != 0) {
                let name = self.string(array_names.get(i).copied().unwrap_or(0))?;
                self.drawable("Item", d, Some(name), self.parts_inherit)?;
            }
            self.xml.close("DrawableArray");
        }
        let transforms = u64_at(f, 0xA8);
        if transforms != 0 {
            self.bone_transforms(transforms)?;
        }
        if group != 0 {
            self.xml.open("Physics");
            let g = res.block(group, 0x30)?;
            for (i, tag) in ["LOD1", "LOD2", "LOD3"].into_iter().enumerate() {
                let lod = u64_at(g, 0x10 + i * 8);
                if lod != 0 {
                    self.xml.open(tag);
                    self.lod(lod)?;
                    self.xml.close(tag);
                }
            }
            self.xml.close("Physics");
        }
        let vehicle_glass = u64_at(f, 0x120);
        if vehicle_glass != 0 {
            self.xml.open("VehicleGlassWindows");
            self.vehicle_glass(vehicle_glass)?;
            self.xml.close("VehicleGlassWindows");
        }
        let glass = u64_at(f, 0xE0);
        if glass != 0 {
            let windows = self.pointers(glass, f[0xD9] as usize)?;
            self.items("GlassWindows", &windows, |w, p| w.glass_window(p))?;
        }
        let lights = read_lights(res, u64_at(f, 0x110), u16_at(f, 0x118), self.names)?;
        self.xml.lights(&lights);
        if !cloths.is_empty() {
            self.items("Cloths", &cloths, |w, p| w.environment_cloth(p))?;
        }
        self.xml.close("Fragment");
        Ok(())
    }

    /// CodeWalker's `WriteItemArray` of blocks by pointer: `<name />` for none, `<Item />` for a null one.
    fn items(&mut self, name: &str, pointers: &[u64], mut item: impl FnMut(&mut Self, u64) -> Result<(), Error>) -> Result<(), Error> {
        if pointers.is_empty() {
            self.xml.empty(name, "");
            return Ok(());
        }
        self.xml.open(name);
        for &p in pointers {
            if p == 0 {
                self.xml.empty("Item", "");
            } else {
                self.xml.open("Item");
                item(self, p)?;
                self.xml.close("Item");
            }
        }
        self.xml.close(name);
        Ok(())
    }

    /// `FragBoneTransforms`: 3x4 matrices (rows) after a 0x20-byte header.
    fn bone_transforms(&mut self, ptr: u64) -> Result<(), Error> {
        let h = self.res.block(ptr, 0x20)?;
        let (count, unk) = (h[0x10] as usize, u16_at(h, 0x12));
        let tag = format!("BoneTransforms unk=\"{unk}\"");
        if count == 0 {
            self.xml.empty(&tag, "");
            return Ok(());
        }
        let items = self.res.block(ptr + 0x20, count * 48)?;
        self.xml.open(&tag);
        for m in items.as_chunks::<48>().0 {
            self.xml.open("Item");
            self.xml.rows(&floats(m, 0, 12), 4);
            self.xml.close("Item");
        }
        self.xml.close("BoneTransforms");
        Ok(())
    }

    /// `FragPhysicsLOD.WriteXml`.
    fn lod(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let l = res.block(ptr, 0x130)?;
        self.values(l, &[("Unknown14", 0x14), ("Unknown18", 0x18), ("Unknown1C", 0x1C)]);
        self.vectors(
            l,
            &[("PositionOffset", 0x30), ("Unknown40", 0x40), ("Unknown50", 0x50), ("DampingLinearC", 0x60), ("DampingLinearV", 0x70), ("DampingLinearV2", 0x80), ("DampingAngularC", 0x90), ("DampingAngularV", 0xA0), ("DampingAngularV2", 0xB0)],
        );
        for (tag, at) in [("Archetype", 0xD8), ("Archetype2", 0xE0)] {
            let archetype = u64_at(l, at);
            if archetype != 0 {
                self.xml.open(tag);
                self.archetype(archetype)?;
                self.xml.close(tag);
            }
        }
        let body = u64_at(l, 0x20);
        if body != 0 {
            self.xml.open("ArticulatedBody");
            self.articulated_body(body)?;
            self.xml.close("ArticulatedBody");
        }
        let transforms = u64_at(l, 0x100);
        if transforms != 0 {
            let count = u32_at(res.block(transforms, 0x20)?, 0x10) as usize;
            let matrices = res.block(transforms + 0x20, count.saturating_mul(64))?;
            self.xml.open("Transforms");
            for m in matrices.as_chunks::<64>().0 {
                self.xml.open("Item");
                self.xml.rows(&floats(m, 0, 16), 4);
                self.xml.close("Item");
            }
            self.xml.close("Transforms");
        }
        let (groups, children) = (u64_at(l, 0xC8), u64_at(l, 0xD0));
        if groups != 0 {
            let count = l[0x11A] as usize;
            let list = self.pointers(groups, count)?;
            // Group names come from their own list when there is one (CodeWalker's fix for edited files).
            let group_names = self.pointers(u64_at(l, 0xC0), count)?;
            let mut i = 0;
            self.items("Groups", &list, |w, p| {
                let name = group_names.get(i).copied().filter(|&n| n != 0).unwrap_or(p + 0x80);
                i += 1;
                w.group(p, name)
            })?;
        }
        if children != 0 {
            let count = l[0x11D] as usize;
            let list = self.pointers(children, count)?;
            // What the LOD keeps for each child: a float and two vectors.
            let per_child = |ptr: u64, size: usize| if ptr == 0 || count == 0 { Ok(None) } else { res.block(ptr, count * size).map(Some) };
            let (unk_floats, tensors, unk_vecs) = (per_child(u64_at(l, 0x28), 4)?, per_child(u64_at(l, 0xF0), 16)?, per_child(u64_at(l, 0xF8), 16)?);
            self.xml.open("Children");
            for (i, &p) in list.iter().enumerate() {
                if p == 0 {
                    self.xml.empty("Item", "");
                    continue;
                }
                let c = res.block(p, 0x100)?;
                self.xml.open("Item");
                self.xml.value("GroupIndex", u16_at(c, 0x10));
                self.xml.value("BoneTag", u16_at(c, 0x12));
                self.values(c, &[("PristineMass", 0x08), ("DamagedMass", 0x0C)]);
                self.xml.value("UnkFloat", fl(unk_floats.map_or(0.0, |b| f32_at(b, i * 4))));
                self.xml.empty("UnkVec", &attrs(&unk_vecs.map_or([0.0; 4], |b| vec4_at(b, i * 16))));
                self.xml.empty("InertiaTensor", &attrs(&tensors.map_or([0.0; 4], |b| vec4_at(b, i * 16))));
                if u64_at(c, 0xB0) != 0 {
                    self.xml.empty("EventSet", "");
                }
                for (tag, at) in [("Drawable", 0xA0), ("Drawable2", 0xA8)] {
                    let d = u64_at(c, at);
                    if d != 0 {
                        self.drawable(tag, d, None, self.parts_inherit)?;
                    }
                }
                self.xml.close("Item");
            }
            self.xml.close("Children");
        }
        for (name, at, count) in [("UnknownData1", 0x108, l[0x118]), ("UnknownData2", 0x110, l[0x119])] {
            let p = u64_at(l, at);
            if p != 0 && count != 0 {
                let bytes = res.block(p, count as usize)?;
                self.xml.raw(name, &bytes.iter().map(u8::to_string).collect::<Vec<_>>(), 10);
            }
        }
        Ok(())
    }

    /// `FragPhysArchetype.WriteXml`.
    fn archetype(&mut self, ptr: u64) -> Result<(), Error> {
        let a = self.res.block(ptr, 0xE0)?;
        let name = self.string(u64_at(a, 0x18))?;
        self.xml.text("Name", &name);
        self.values(a, &[("Mass", 0x40), ("MassInv", 0x44), ("Unknown48", 0x48), ("Unknown4C", 0x4C), ("Unknown50", 0x50), ("Unknown54", 0x54)]);
        self.vectors(a, &[("InertiaTensor", 0x60), ("InertiaTensorInv", 0x70)]);
        let bounds = fragment_bound_xml(self.res, u64_at(a, 0x20), 0)?;
        self.xml.block(&bounds);
        Ok(())
    }

    /// `FragPhysArticulatedBodyType.WriteXml`.
    fn articulated_body(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let b = res.block(ptr, 0xB0)?;
        self.xml.raw("ItemIndices", &(0..22).map(|i| u32_at(b, 0x10 + i * 4).to_string()).collect::<Vec<_>>(), 22);
        self.xml.raw("ItemFlags", &b[0x8A..0xA0].iter().map(u8::to_string).collect::<Vec<_>>(), 22);
        let (vectors, vector_count) = (u64_at(b, 0x80), b[0x88] as usize);
        if vectors != 0 && vector_count != 0 {
            let items: Vec<String> = res.block(vectors, vector_count * 16)?.as_chunks::<16>().0.iter().map(|v| vec4_text(v)).collect();
            self.xml.raw("UnknownVectors", &items, 1);
        }
        let joints = u64_at(b, 0x78);
        if joints != 0 {
            self.xml.open("Joints");
            for p in self.pointers(joints, b[0x89] as usize)? {
                if p == 0 {
                    self.xml.empty("Item", "");
                    continue;
                }
                // Both joint types write the nine vectors from 0x20.
                let j = res.block(p, 0xB0)?;
                self.xml.open(&format!("Item type=\"{}\"", joint_type(j[0x15])));
                self.xml.value("FragIndex1", j[0x16]);
                self.xml.value("FragIndex2", j[0x17]);
                self.xml.value("Unknown10", fl(f32_at(j, 0x10)));
                for (k, name) in ["Unknown20", "Unknown30", "Unknown40", "Unknown50", "Unknown60", "Unknown70", "Unknown80", "Unknown90", "UnknownA0"].into_iter().enumerate() {
                    self.xml.empty(name, &attrs(&vec4_at(j, 0x20 + k * 16)));
                }
                self.xml.close("Item");
            }
            self.xml.close("Joints");
        }
        Ok(())
    }

    /// `FragPhysTypeGroup.WriteXml`; its name is 40 bytes at `name`.
    fn group(&mut self, ptr: u64, name: u64) -> Result<(), Error> {
        let g = self.res.block(ptr, 0xB0)?;
        let name = group_name(self.res.block(name, 40)?);
        self.xml.text("Name", &name);
        self.xml.value("ParentIndex", g[0x4D]);
        self.xml.value("GlassWindowIndex", g[0x52]);
        self.xml.value("GlassFlags", g[0x53]);
        self.values(
            g,
            &[
                ("Strength", 0x10),
                ("ForceTransmissionScaleUp", 0x14),
                ("ForceTransmissionScaleDown", 0x18),
                ("JointStiffness", 0x1C),
                ("MinSoftAngle1", 0x20),
                ("MaxSoftAngle1", 0x24),
                ("MaxSoftAngle2", 0x28),
                ("MaxSoftAngle3", 0x2C),
                ("RotationSpeed", 0x30),
                ("RotationStrength", 0x34),
                ("RestoringStrength", 0x38),
                ("RestoringMaxTorque", 0x3C),
                ("LatchStrength", 0x40),
                ("Mass", 0x44),
                ("MinDamageForce", 0x54),
                ("DamageHealth", 0x58),
                ("UnkFloat5C", 0x5C),
                ("UnkFloat60", 0x60),
                ("UnkFloat64", 0x64),
                ("UnkFloat68", 0x68),
                ("UnkFloat6C", 0x6C),
                ("UnkFloat70", 0x70),
                ("UnkFloat74", 0x74),
                ("UnkFloat78", 0x78),
                ("UnkFloatA8", 0xA8),
            ],
        );
        Ok(())
    }

    /// `FragGlassWindow.WriteXml`. Enhanced windows hold a gen9 vertex declaration
    /// (0x140 bytes instead of 0x10), which CodeWalker writes as GTAV1.
    fn glass_window(&mut self, ptr: u64) -> Result<(), Error> {
        let enhanced = is_enhanced(self.res.version);
        let (size, tail) = if enhanced { (0x1A0, 0x180) } else { (0x70, 0x50) };
        let g = self.res.block(ptr, size)?;
        self.xml.value("Flags", u16_at(g, tail + 6));
        self.xml.open("Projection");
        let projection: Vec<String> = (0..3).flat_map(|r| floats(g, r * 16, 3)).collect();
        self.xml.rows(&projection, 3);
        self.xml.close("Projection");
        self.values(g, &[("UnkFloat13", 0x30), ("UnkFloat14", 0x34), ("UnkFloat15", 0x38), ("UnkFloat16", 0x3C), ("Thickness", tail), ("UnkFloat18", tail + 8), ("UnkFloat19", tail + 0xC)]);
        self.xml.empty("Tangent", &attrs(&vec3_at(g, tail + 0x10)));
        let (kind, flags) = if enhanced {
            let flags = super::GEN9_SLOTS.iter().filter(|&&(slot, _)| g[0x40 + 52 * 5 + slot] != 0).fold(0u32, |f, &(_, bit)| f | 1 << bit);
            ("GTAV1".to_string(), flags)
        } else {
            (declaration_name(u64_at(g, 0x48)), u32_at(g, 0x40))
        };
        self.xml.open(&format!("Layout type=\"{kind}\""));
        for (bit, name) in super::SEMANTICS.iter().enumerate() {
            if flags & (1 << bit) != 0 {
                self.xml.empty(name, "");
            }
        }
        self.xml.close("Layout");
        Ok(())
    }

    /// `FragVehicleGlassWindows.WriteXml`: the windows follow the header one after
    /// the other, each padded to 16 bytes from the start.
    fn vehicle_glass(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let count = u16_at(res.block(ptr, 0x10)?, 6) as usize;
        // The header, the (even) list of offsets and a word.
        let mut at = 16 + (count + (count & 1)) as u64 * 8;
        for _ in 0..count {
            let w = res.block(ptr + at, 0x70)?;
            let (width, rows, bytes) = (u16_at(w, 0x48) as usize, u16_at(w, 0x4A) as usize, u16_at(w, 0x4C));
            let mut pos = at + 0x70;
            let mut map = None;
            if bytes != 0 {
                pos += rows as u64 * 2;
                let mut lines = Vec::with_capacity(rows);
                for _ in 0..rows {
                    let (line, len) = self.shatter_row(ptr + pos, width)?;
                    lines.push(line);
                    pos += len;
                }
                map = Some(lines);
            }
            self.xml.open("Window");
            self.xml.value("ItemID", u16_at(w, 0x44));
            self.xml.value("UnkUshort1", u16_at(w, 0x46));
            self.xml.value("UnkUshort4", u16_at(w, 0x60));
            self.xml.value("UnkUshort5", u16_at(w, 0x62));
            // CodeWalker reads this matrix by columns (`ReadMatrix`) and writes it by rows.
            self.xml.open("Projection");
            let projection: Vec<String> = (0..16).map(|k| fl(f32_at(w, (k % 4 * 4 + k / 4) * 4))).collect();
            self.xml.rows(&projection, 4);
            self.xml.close("Projection");
            self.values(w, &[("UnkFloat17", 0x58), ("UnkFloat18", 0x5C), ("CracksTextureTiling", 0x64)]);
            if let Some(lines) = map {
                self.xml.open("ShatterMap");
                for line in &lines {
                    self.xml.line(line);
                }
                self.xml.close("ShatterMap");
            }
            self.xml.close("Window");
            at = pos.next_multiple_of(16);
        }
        Ok(())
    }

    /// A shatter map row (`WindowShatterMapRow.Read`) as its line, and its length in bytes.
    fn shatter_row(&self, ptr: u64, width: usize) -> Result<(String, u64), Error> {
        let res = self.res;
        let head = res.block(ptr, 2)?;
        let (start1, end1) = (head[0], head[1]);
        let mut len = 2u64;
        let (mut data1, mut start2, mut data2) = (None, 255, None);
        let n = end1 as i32 - start1 as i32 + 1;
        if n > 0 {
            data1 = Some(res.block(ptr + len, n as usize)?);
            len += n as u64;
            start2 = res.block(ptr + len, 1)?[0];
            len += 1;
            if start2 != 255 {
                let end2 = res.block(ptr + len, 1)?[0];
                len += 1;
                let n2 = end2 as i32 - start2 as i32 + 1;
                if n2 > 0 {
                    data2 = Some(res.block(ptr + len, n2 as usize)?);
                    len += n2 as u64;
                }
            }
        }
        Ok((shatter_line(start1, data1, start2, data2, width), len))
    }

    /// `EnvironmentCloth.WriteXml`.
    fn environment_cloth(&mut self, ptr: u64) -> Result<(), Error> {
        let e = self.res.block(ptr, 0x80)?;
        self.xml.value("Unknown78", u32_at(e, 0x78));
        self.lists(e, &[("UnknownData", 0x60, Item::U32)])?;
        let (controller, tuning, drawable) = (u64_at(e, 0x28), u64_at(e, 0x10), u64_at(e, 0x18));
        if controller != 0 {
            self.xml.open("Controller");
            self.controller(controller)?;
            self.xml.close("Controller");
        }
        if tuning != 0 {
            let t = self.res.block(tuning, 0x40)?;
            self.xml.open("InstanceTuning");
            self.values(t, &[("Unknown10", 0x10), ("Unknown14", 0x14), ("Unknown20", 0x20), ("Unknown24", 0x24), ("Unknown28", 0x28), ("Unknown2C", 0x2C)]);
            self.xml.value("Flags", u32_at(t, 0x30));
            self.values(t, &[("Unknown34", 0x34), ("Unknown38", 0x38)]);
            self.xml.value("Unknown3C", u32_at(t, 0x3C));
            self.xml.close("InstanceTuning");
        }
        if drawable != 0 {
            self.drawable("Drawable", drawable, None, false)?;
        }
        Ok(())
    }

    /// `ClothController.WriteXml`.
    fn controller(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let c = res.block(ptr, 0x80)?;
        self.xml.text("Name", &ascii_name(&c[0x58..0x78]));
        self.xml.value("Type", u32_at(c, 0x50));
        self.values(c, &[("Unknown78", 0x78)]);
        let bridge = u64_at(c, 0x10);
        if bridge != 0 {
            let b = res.block(bridge, 0x140)?;
            self.xml.open("BridgeSimGfx");
            for (name, at) in [("VertexCount", 0x10), ("Unknown14", 0x14), ("Unknown18", 0x18)] {
                self.xml.value(name, u32_at(b, at));
            }
            self.lists(
                b,
                &[
                    ("Unknown20", 0x20, Item::F32),
                    ("Unknown30", 0x30, Item::F32),
                    ("Unknown40", 0x40, Item::F32),
                    ("Unknown60", 0x60, Item::F32),
                    ("Unknown70", 0x70, Item::U32),
                    ("Unknown80", 0x80, Item::U32),
                    ("UnknownA0", 0xA0, Item::F32),
                    ("UnknownB0", 0xB0, Item::U32),
                    ("UnknownC0", 0xC0, Item::U32),
                    ("UnknownE0", 0xE0, Item::U16),
                    ("UnknownF0", 0xF0, Item::U16),
                    ("Unknown100", 0x100, Item::U16),
                    ("Unknown128", 0x128, Item::U32),
                ],
            )?;
            self.xml.close("BridgeSimGfx");
        }
        let morph = u64_at(c, 0x18);
        if morph != 0 {
            let m = res.block(morph, 0x40)?;
            self.xml.open("MorphController");
            for (tag, at) in [("Unknown18", 0x18), ("Unknown20", 0x20), ("Unknown28", 0x28)] {
                let p = u64_at(m, at);
                if p != 0 {
                    let u = res.block(p, 0x190)?;
                    self.xml.open(tag);
                    // CodeWalker writes this integer through a float: the same digits for any real value.
                    self.xml.value("Unknown180", u32_at(u, 0x180));
                    self.lists(
                        u,
                        &[
                            ("Unknown50", 0x50, Item::Vec4),
                            ("Unknown60", 0x60, Item::U16),
                            ("Unknown70", 0x70, Item::U16),
                            ("Unknown80", 0x80, Item::U16),
                            ("Unknown90", 0x90, Item::U16),
                            ("UnknownA0", 0xA0, Item::Vec4),
                            ("UnknownB0", 0xB0, Item::U16),
                            ("UnknownC0", 0xC0, Item::U16),
                            ("UnknownD0", 0xD0, Item::U16),
                            ("UnknownE0", 0xE0, Item::U16),
                            ("Unknown150", 0x150, Item::U16),
                            ("Unknown160", 0x160, Item::U16),
                        ],
                    )?;
                    self.xml.close(tag);
                }
            }
            self.xml.close("MorphController");
        }
        for (tag, at) in [("VerletCloth1", 0x20), ("VerletCloth2", 0x28), ("VerletCloth3", 0x30)] {
            let p = u64_at(c, at);
            if p != 0 {
                self.xml.open(tag);
                self.verlet_cloth(p)?;
                self.xml.close(tag);
            }
        }
        Ok(())
    }

    /// `VerletCloth.WriteXml`. Its bound belongs to the cloth, not the fragment, so
    /// composite children keep their flags.
    fn verlet_cloth(&mut self, ptr: u64) -> Result<(), Error> {
        let res = self.res;
        let v = res.block(ptr, 0x180)?;
        self.vectors(v, &[("BBMin", 0x30), ("BBMax", 0x40)]);
        self.xml.value("Unknown3C", u32_at(v, 0x3C));
        self.xml.value("Unknown4C", u32_at(v, 0x4C));
        self.values(v, &[("Unknown50", 0x50), ("UnknownA8", 0xA8), ("UnknownAC", 0xAC)]);
        self.xml.value("UnknownE8", u32_at(v, 0xE8));
        self.xml.value("UnknownFA", u16_at(v, 0xFA));
        self.xml.value("Unknown148", u16_at(v, 0x148));
        self.values(v, &[("Unknown158", 0x158)]);
        if u64_at(v, 0x140) != 0 {
            self.xml.empty("Unknown140", "");
        }
        if u64_at(v, 0x130) != 0 {
            self.xml.empty("Behaviour", "");
        }
        self.lists(v, &[("Vertices", 0x80, Item::Vec4), ("Vertices2", 0x70, Item::Vec4)])?;
        for (name, at) in [("Constraints", 0x110), ("Constraints2", 0x100)] {
            let (p, count) = (u64_at(v, at), u16_at(v, at + 8) as usize);
            if p == 0 || count == 0 {
                continue;
            }
            self.xml.open(name);
            for c in res.block(p, count * 16)?.as_chunks::<16>().0 {
                self.xml.open("Item");
                self.xml.value("Unknown0", u16_at(c, 0));
                self.xml.value("Unknown2", u16_at(c, 2));
                self.values(c, &[("Unknown4", 4), ("Unknown8", 8), ("UnknownC", 0xC)]);
                self.xml.close("Item");
            }
            self.xml.close(name);
        }
        let bounds = bound_xml(res, u64_at(v, 0x18), 0)?;
        self.xml.block(&bounds);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shatter_lines_fill_the_width() {
        assert_eq!(shatter_line(1, Some(&[0x0A, 0xFF]), 255, None, 5), "##0AFF####");
        assert_eq!(shatter_line(0, Some(&[1]), 3, Some(&[2, 3]), 6), "01----0203##");
        assert_eq!(shatter_line(0, None, 255, None, 2), "####");
        // A row longer than the width is written whole.
        assert_eq!(shatter_line(0, Some(&[1, 2, 3]), 255, None, 2), "010203");
    }

    #[test]
    fn names_read_like_codewalker() {
        assert_eq!(group_name(b"door_dside_f\0\0\0\0"), "door_dside_f");
        assert_eq!(ascii_name(b"cloth\xE9\0\0"), "cloth?");
        assert_eq!(joint_type(1), "DOF3");
        assert_eq!(joint_type(7), "7");
        assert_eq!(declaration_name(0x0300_0000_0199_6006), "GTAV3");
        assert_eq!(declaration_name(5), "5");
    }

    /// A Legacy fragment in one system page: a name, one light, a vehicle window
    /// with a two-row shatter map, and a bone transform.
    fn sample() -> Resource {
        let mut s = vec![0u8; 0x400];
        let mut put = |at: usize, bytes: &[u8]| s[at..at + bytes.len()].copy_from_slice(bytes);
        let ptr = |at: usize| (ROOT + at as u64).to_le_bytes();
        put(0x58, &ptr(0x3A0));
        put(0x3A0, b"prop_x\0");
        put(0x2C, &2.5f32.to_le_bytes());
        put(0xB8, &(-16i32).to_le_bytes());
        // Bone transforms at 0x140: one 3x4 matrix.
        put(0xA8, &ptr(0x140));
        put(0x150, &[1, 1, 1, 0]);
        put(0x160, &1.0f32.to_le_bytes());
        // A light list of one at 0x200.
        put(0x110, &ptr(0x200));
        put(0x118, &[1, 0, 1, 0]);
        put(0x200 + 0x26, &[1]);
        // Vehicle windows at 0x2B0: a header of one window, its two offsets and a word, then the window.
        put(0x120, &ptr(0x2B0));
        put(0x2B0 + 6, &1u16.to_le_bytes());
        let w = 0x2B0 + 0x20;
        put(w + 0x44, &7u16.to_le_bytes());
        put(w + 0x48, &[3, 0, 2, 0, 9, 0]);
        // Rows after the two offsets: cells 1-2, then cell 0 and a second run at 2.
        put(w + 0x74, &[1, 2, 0xAB, 0xCD, 255]);
        put(w + 0x79, &[0, 0, 0x01, 2, 2, 0x02]);
        Resource { version: 162, system: s, graphics: Vec::new() }
    }

    #[test]
    fn fragments_list_their_parts_in_order() {
        let (xml, drawables) = write(&sample(), &Names::new()).unwrap();
        assert!(drawables.is_empty());
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Fragment>\n <Name>prop_x</Name>\n"), "{xml}");
        assert!(xml.contains("<BoundingSphereRadius value=\"2.5\" />\n <UnknownB0 value=\"0\" />\n <UnknownB8 value=\"-16\" />"), "{xml}");
        assert!(xml.contains(" <BoneTransforms unk=\"1\">\n  <Item>\n   1 0 0 0\n   0 0 0 0\n   0 0 0 0\n  </Item>\n </BoneTransforms>\n"), "{xml}");
        assert!(xml.contains("<ItemID value=\"7\" />"), "{xml}");
        assert!(xml.contains("<ShatterMap>\n    ##ABCD\n    01--02\n   </ShatterMap>"), "{xml}");
        assert!(xml.contains("<Lights>\n  <Item>"), "{xml}");
        assert!(!xml.contains("<Drawable>") && !xml.contains("Physics") && !xml.contains("Cloths"), "{xml}");
        assert!(xml.find("</VehicleGlassWindows>").unwrap() < xml.find("<Lights>").unwrap(), "{xml}");
        assert!(xml.ends_with("</Lights>\n</Fragment>\n"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }
}
