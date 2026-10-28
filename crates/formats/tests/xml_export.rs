//! CodeWalker XML export of a drawable dictionary built by hand: a Legacy `.ydd` with one
//! drawable, one model and one geometry of three vertices.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

const SYS: u64 = 0x5000_0000;

/// A Legacy (version 165) `.ydd` in one system page of 0x400 bytes.
fn ydd() -> Vec<u8> {
    let mut s = vec![0u8; 0x400];
    let mut put = |at: usize, bytes: &[u8]| s[at..at + bytes.len()].copy_from_slice(bytes);
    let ptr = |at: usize| (SYS + at as u64).to_le_bytes();
    let floats = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
    // Dictionary: one name hash at 0x40, one drawable pointer at 0x48.
    put(0x20, &ptr(0x40));
    put(0x28, &1u16.to_le_bytes());
    put(0x30, &ptr(0x48));
    put(0x38, &1u16.to_le_bytes());
    put(0x40, &0x1234_5678u32.to_le_bytes());
    put(0x48, &ptr(0x50));
    // Drawable at 0x50: no shader group or skeleton, high-detail models at 0x100.
    let d = 0x50;
    put(d + 0x20, &floats(&[0.5, 0.5, 0.0, 1.0]));
    put(d + 0x30, &floats(&[0.0, 0.0, 0.0]));
    put(d + 0x40, &floats(&[1.0, 1.0, 0.0]));
    put(d + 0x50, &ptr(0x100));
    put(d + 0x70, &floats(&[9998.0; 4]));
    put(d + 0x80, &0xFFu32.to_le_bytes());
    // Model list, the model at 0x120 with one geometry (shader 0).
    put(0x100, &ptr(0x110));
    put(0x108, &1u16.to_le_bytes());
    put(0x110, &ptr(0x120));
    put(0x120 + 0x08, &ptr(0x150));
    put(0x120 + 0x10, &1u16.to_le_bytes());
    put(0x120 + 0x20, &ptr(0x158));
    put(0x120 + 0x2C, &0x00FFu16.to_le_bytes());
    put(0x150, &ptr(0x160));
    // Geometry at 0x160: vertex buffer at 0x200, index buffer at 0x250.
    put(0x160 + 0x18, &ptr(0x200));
    put(0x160 + 0x38, &ptr(0x250));
    // Vertex buffer: stride 20, data at 0x280, three vertices, declaration at 0x240.
    put(0x200 + 0x08, &20u16.to_le_bytes());
    put(0x200 + 0x10, &ptr(0x280));
    put(0x200 + 0x18, &3u32.to_le_bytes());
    put(0x200 + 0x30, &ptr(0x240));
    // GTAV1 declaration: Position (Float3), Colour0 (four bytes), TexCoord0 (Half2).
    put(0x240, &(1u32 | 1 << 4 | 1 << 6).to_le_bytes());
    put(0x248, &(6u64 | 8 << 16 | 1 << 24).to_le_bytes());
    // Index buffer: three indices at 0x270.
    put(0x250 + 0x08, &3u32.to_le_bytes());
    put(0x250 + 0x10, &ptr(0x270));
    put(0x270, &[0, 0, 1, 0, 2, 0]);
    // Half floats: 0 = 0x0000, 0.25 = 0x3400, 0.5 = 0x3800, 1 = 0x3C00.
    let vertices: [([f32; 3], [u8; 4], [u16; 2]); 3] = [([0.0, 0.0, 0.0], [255, 0, 0, 255], [0, 0x3C00]), ([1.0, 0.0, 0.0], [0, 255, 0, 255], [0x3800, 0x3400]), ([0.0, 1.0, 0.0], [0, 0, 255, 255], [0x3C00, 0])];
    for (k, (p, c, uv)) in vertices.into_iter().enumerate() {
        let at = 0x280 + k * 20;
        put(at, &floats(&p));
        put(at + 12, &c);
        put(at + 16, &uv[0].to_le_bytes());
        put(at + 18, &uv[1].to_le_bytes());
    }

    let version = 165u32;
    let pages = (1 << 27) | 1; // one page of 0x400 bytes
    let mut out = Vec::new();
    for w in [0x3743_5352, version, ((version >> 4) << 28) | pages, (version & 0xF) << 28] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&s).unwrap();
    z.finish().unwrap()
}

/// The element children of `node` called `name`.
fn children<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(move |n| n.has_tag_name(name))
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &'a str) -> roxmltree::Node<'a, 'i> {
    children(node, name).next().unwrap_or_else(|| panic!("no <{name}> in <{}>", node.tag_name().name()))
}

#[test]
fn drawable_dictionary_as_xml() {
    let export = paleto_formats::xml_export::export(&ydd(), "ydd", "dict", &[]).unwrap();
    assert!(export.textures.is_empty());
    let xml = &export.xml;
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DrawableDictionary>\n"), "{xml}");
    let doc = roxmltree::Document::parse(xml).unwrap();
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "DrawableDictionary");
    let items: Vec<_> = children(root, "Item").collect();
    assert_eq!(items.len(), 1, "{xml}");
    let drawable = items[0];
    // Names come from the dictionary's hashes; unknown ones print as hash_XXXXXXXX.
    assert_eq!(child(drawable, "Name").text(), Some("hash_12345678"));

    let models: Vec<_> = children(child(drawable, "DrawableModelsHigh"), "Item").collect();
    assert_eq!(models.len(), 1, "{xml}");
    let geometries: Vec<_> = children(child(models[0], "Geometries"), "Item").collect();
    assert_eq!(geometries.len(), 1, "{xml}");
    let geometry = geometries[0];
    let vb = child(geometry, "VertexBuffer");
    let layout = child(vb, "Layout");
    assert_eq!(layout.attribute("type"), Some("GTAV1"));
    let components: Vec<&str> = layout.children().filter(|n| n.is_element()).map(|n| n.tag_name().name()).collect();
    assert_eq!(components, ["Position", "Colour0", "TexCoord0"]);
    // One row per vertex, components three spaces apart.
    let rows: Vec<&str> = child(vb, "Data").text().unwrap().lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(rows, ["0 0 0   255 0 0 255   0 1", "1 0 0   0 255 0 255   0.5 0.25", "0 1 0   0 0 255 255   1 0"]);
    let indices = child(child(geometry, "IndexBuffer"), "Data").text().unwrap();
    assert_eq!(indices.split_whitespace().collect::<Vec<_>>(), ["0", "1", "2"]);
}

#[test]
fn damaged_dictionaries_are_errors() {
    let data = ydd();
    assert!(paleto_formats::xml_export::export(&data[..20], "ydd", "dict", &[]).is_err());
    assert!(paleto_formats::xml_export::export(b"not a resource", "ydd", "dict", &[]).is_err());
    for n in 0..data.len() {
        let _ = paleto_formats::xml_export::export(&data[..n], "ydd", "dict", &[]);
    }
}

/// A Legacy `.ydd` with one drawable whose `models` high-detail models all point at one list of
/// `geometries_each` geometries (the same triangle each time), in a system page of 1 MiB.
fn shared_models_ydd(models: u16, geometries_each: u16) -> Vec<u8> {
    let mut s = vec![0u8; 0x10_0000];
    let mut put = |at: u64, bytes: &[u8]| s[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
    let ptr = |at: u64| (SYS + at).to_le_bytes();
    let floats = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
    // Dictionary and drawable as in `ydd`, the drawable's high-detail model list at 0x100.
    put(0x20, &ptr(0x40));
    put(0x28, &1u16.to_le_bytes());
    put(0x30, &ptr(0x48));
    put(0x38, &1u16.to_le_bytes());
    put(0x40, &0x1234_5678u32.to_le_bytes());
    put(0x48, &ptr(0x50));
    let d = 0x50;
    put(d + 0x20, &floats(&[0.5, 0.5, 0.0, 1.0]));
    put(d + 0x40, &floats(&[1.0, 1.0, 0.0]));
    put(d + 0x50, &ptr(0x100));
    put(d + 0x70, &floats(&[9998.0; 4]));
    put(d + 0x80, &0xFFu32.to_le_bytes());
    // The model list at 0x100 points at `models` entries from 0x1000, all of one model at 0x9_0000.
    const MODEL: u64 = 0x9_0000;
    put(0x100, &ptr(0x1000));
    put(0x108, &models.to_le_bytes());
    for i in 0..models as u64 {
        put(0x1000 + i * 8, &ptr(MODEL));
    }
    // The model: its geometry list at +0x100, shaders at +0x200, the geometry at +0x300.
    put(MODEL + 0x08, &ptr(MODEL + 0x100));
    put(MODEL + 0x10, &geometries_each.to_le_bytes());
    put(MODEL + 0x20, &ptr(MODEL + 0x200));
    put(MODEL + 0x2C, &0x00FFu16.to_le_bytes());
    for i in 0..geometries_each as u64 {
        put(MODEL + 0x100 + i * 8, &ptr(MODEL + 0x300));
    }
    // Geometry: vertex buffer at +0x400, index buffer at +0x500.
    put(MODEL + 0x300 + 0x18, &ptr(MODEL + 0x400));
    put(MODEL + 0x300 + 0x38, &ptr(MODEL + 0x500));
    // Vertex buffer (Legacy layout): stride 20, data at +0x800, three vertices, declaration at +0x600.
    put(MODEL + 0x400 + 0x08, &20u16.to_le_bytes());
    put(MODEL + 0x400 + 0x10, &ptr(MODEL + 0x800));
    put(MODEL + 0x400 + 0x18, &3u32.to_le_bytes());
    put(MODEL + 0x400 + 0x30, &ptr(MODEL + 0x600));
    put(MODEL + 0x600, &(1u32 | 1 << 4 | 1 << 6).to_le_bytes());
    put(MODEL + 0x608, &(6u64 | 8 << 16 | 1 << 24).to_le_bytes());
    // Index buffer: three indices at +0x700.
    put(MODEL + 0x500 + 0x08, &3u32.to_le_bytes());
    put(MODEL + 0x500 + 0x10, &ptr(MODEL + 0x700));
    put(MODEL + 0x700, &[0, 0, 1, 0, 2, 0]);

    // Header: system page of 0x800 bytes x 512 blocks = 1 MiB (flags: shift 2, count field 32 << 4), version 165.
    let version = 165u32;
    let system_flags = ((version >> 4) << 28) | (32 << 17) | 2;
    let mut out = Vec::new();
    for w in [0x3743_5352, version, system_flags, (version & 0xF) << 28] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&s).unwrap();
    z.finish().unwrap()
}

#[test]
fn models_sharing_one_geometry_list_are_bounded() {
    // 65535 models x 2 geometries = 131070 visits, over the per-drawable budget: refused, and quickly.
    let data = shared_models_ydd(65535, 2);
    let start = std::time::Instant::now();
    let result = paleto_formats::xml_export::export(&data, "ydd", "dict", &[]);
    assert!(start.elapsed() < std::time::Duration::from_secs(60));
    let Err(err) = result else { panic!("the geometry budget should refuse the dictionary") };
    assert!(err.to_string().contains("too many geometries"), "{err}");
}

#[test]
fn texture_folder_never_leaves_the_output() {
    use paleto_formats::xml_export::texture_folder;
    assert_eq!(texture_folder(".."), "textures");
    assert_eq!(texture_folder("."), "textures");
    assert_eq!(texture_folder(""), "textures");
    assert_eq!(texture_folder("prop_probe"), "prop_probe");
}
