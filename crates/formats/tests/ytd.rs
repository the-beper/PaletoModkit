//! Texture dictionaries built by hand, in both layouts.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use paleto_formats::{Format, TextureDictionary};

const SYS: u64 = 0x5000_0000;
const GFX: u64 = 0x6000_0000;

/// One 8x4 A8R8G8B8 texture with two mip levels, named "probe".
fn build(enhanced: bool) -> Vec<u8> {
    let mut system = vec![0u8; 0x200];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    // Dictionary: texture list at 0x40 with one entry.
    put(0x30, &(SYS + 0x40).to_le_bytes());
    put(0x38, &1u16.to_le_bytes());
    put(0x40, &(SYS + 0x50).to_le_bytes());
    let tex = 0x50;
    put(tex + 0x28, &(SYS + 0x180).to_le_bytes());
    if enhanced {
        put(tex + 0x08, &(8u32 * 4 + 4 * 2).to_le_bytes()); // block count
        put(tex + 0x0C, &4u32.to_le_bytes()); // block stride
        put(tex + 0x18, &8u16.to_le_bytes());
        put(tex + 0x1A, &4u16.to_le_bytes());
        put(tex + 0x1F, &[0x57]); // B8G8R8A8_UNORM
        put(tex + 0x22, &[2]);
        put(tex + 0x38, &GFX.to_le_bytes());
        put(tex + 0x30, &(SYS + tex as u64 + 0x58).to_le_bytes()); // embedded shader view
    } else {
        put(tex + 0x50, &8u16.to_le_bytes());
        put(tex + 0x52, &4u16.to_le_bytes());
        put(tex + 0x58, &21u32.to_le_bytes()); // D3DFMT_A8R8G8B8
        put(tex + 0x5D, &[2]);
        put(tex + 0x70, &GFX.to_le_bytes());
    }
    put(0x180, b"probe\0");

    // Mip 0: pixel i is (B, G, R, A) = (i, 2i, 3i, 255); mip 1: all grey.
    let mut graphics = vec![0u8; 0x200];
    for i in 0..32 {
        graphics[i * 4..i * 4 + 4].copy_from_slice(&[i as u8, 2 * i as u8, 3 * i as u8, 255]);
    }
    graphics[128..128 + 32].fill(0x80);

    let version: u32 = if enhanced { 5 } else { 13 };
    let one_page = 1 << 27; // one page of 0x200 bytes
    let mut out = Vec::new();
    for w in [0x3743_5352, version, ((version >> 4) << 28) | one_page, ((version & 0xF) << 28) | one_page] {
        out.extend_from_slice(&u32::to_le_bytes(w));
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&system).unwrap();
    z.write_all(&graphics).unwrap();
    z.finish().unwrap()
}

#[test]
fn legacy_and_enhanced_layouts() {
    for enhanced in [false, true] {
        let ytd = TextureDictionary::parse(&build(enhanced)).unwrap();
        assert_eq!(ytd.enhanced, enhanced);
        let [tex] = &ytd.textures[..] else { panic!("one texture expected") };
        assert_eq!((tex.name.as_str(), tex.width, tex.height, tex.levels), ("probe", 8, 4, 2));
        assert_eq!(tex.format, Format::Bgra8);

        let (w, h, rgba) = tex.rgba(0).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(&rgba[5 * 4..6 * 4], &[15, 10, 5, 255]);
        let (w, h, rgba) = tex.rgba(1).unwrap();
        assert_eq!((w, h), (4, 2));
        assert!(rgba.iter().all(|&c| c == 0x80));

        let dds = tex.to_dds();
        assert_eq!(&dds[..4], b"DDS ");
        assert_eq!(u32::from_le_bytes(dds[28..32].try_into().unwrap()), 2, "mip count");
        assert_eq!(dds.len(), 128 + 8 * 4 * 4 + 4 * 2 * 4);
        assert_eq!(&tex.to_png().unwrap()[1..4], b"PNG");
    }
}

#[test]
fn rejects_other_files() {
    assert!(TextureDictionary::parse(b"<xml/>").is_err());
    let mut wrong_version = build(false);
    wrong_version[4] = 7;
    assert!(TextureDictionary::parse(&wrong_version).is_err());
}

/// A DDS file with a 16x8 BC1 texture and its 4 mip levels.
fn bc1_dds() -> Vec<u8> {
    let mut dds = vec![0u8; 128];
    let mut put = |at: usize, v: u32| dds[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(0, u32::from_le_bytes(*b"DDS "));
    put(4, 124);
    put(8, 0x2_1007);
    put(12, 8);
    put(16, 16);
    put(28, 4);
    put(76, 32);
    put(80, 0x4);
    put(84, u32::from_le_bytes(*b"DXT1"));
    // 16x8, 8x4, 4x2, 2x1: 8 + 2 + 1 + 1 blocks of 8 bytes; colour 0 pure green.
    for _ in 0..12 {
        dds.extend_from_slice(&[0xE0, 0x07, 0xE0, 0x07, 0, 0, 0, 0]);
    }
    dds
}

#[test]
fn replace_texture_from_dds() {
    for enhanced in [false, true] {
        let rebuilt = paleto_formats::replace_textures(&build(enhanced), &[("PROBE", &bc1_dds())]).unwrap();
        let ytd = TextureDictionary::parse(&rebuilt).unwrap();
        let tex = &ytd.textures[0];
        assert_eq!((tex.name.as_str(), tex.width, tex.height, tex.levels, tex.format), ("probe", 16, 8, 4, Format::Bc1));
        let (_, _, rgba) = tex.rgba(0).unwrap();
        assert_eq!(&rgba[..4], &[0, 255, 0, 255]);
        // Exporting and importing again gives the same file contents.
        let again = paleto_formats::replace_textures(&rebuilt, &[("probe", &tex.to_dds())]).unwrap();
        assert_eq!(TextureDictionary::parse(&again).unwrap().textures[0].data, tex.data);
    }
}

#[test]
fn replace_errors() {
    let ytd = build(false);
    let err = paleto_formats::replace_textures(&ytd, &[("missing", &bc1_dds())]).unwrap_err();
    assert!(err.to_string().contains("no texture named missing"));
    assert!(paleto_formats::replace_textures(&ytd, &[("probe", b"not a dds")]).is_err());
    // BC6H has no Legacy format code.
    let mut bc6 = bc1_dds();
    bc6[84..88].copy_from_slice(b"DX10");
    let mut ext = vec![0u8; 20];
    ext[..4].copy_from_slice(&95u32.to_le_bytes());
    ext[12..16].copy_from_slice(&1u32.to_le_bytes());
    bc6.splice(128..128, ext);
    bc6.extend(std::iter::repeat_n(0, 96));
    let err = paleto_formats::replace_textures(&ytd, &[("probe", &bc6)]).unwrap_err();
    assert!(err.to_string().contains("BC6H"), "{err}");
}

/// A PNG file of the given size and colour.
fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[test]
fn add_remove_and_rename_textures() {
    use paleto_formats::{Edit, edit_textures};
    for enhanced in [false, true] {
        let (red, glass) = (png(64, 32, [255, 0, 0, 255]), png(16, 16, [0, 0, 255, 128]));
        let edits = [
            Edit::Add { name: "Red", file: &red, format: None },
            Edit::Add { name: "glass", file: &glass, format: None },
            Edit::Add { name: "raw", file: &red, format: Some(Format::Bgra8) },
            Edit::Rename { from: "PROBE", to: "probe_renamed" },
        ];
        let edited = edit_textures(&build(enhanced), &edits).unwrap();
        let ytd = TextureDictionary::parse(&edited).unwrap();
        let mut names: Vec<&str> = ytd.textures.iter().map(|t| t.name.as_str()).collect();
        // Stored in hash order.
        let sorted = {
            let mut n = names.clone();
            n.sort_by_key(|n| joaat(n));
            n
        };
        assert_eq!(names, sorted);
        names.sort();
        assert_eq!(names, ["Red", "glass", "probe_renamed", "raw"]);

        let tex = |name: &str| ytd.textures.iter().find(|t| t.name == name).unwrap();
        // Opaque pictures become BC1, transparent ones BC3; mipmaps down to 4 pixels.
        let r = tex("Red");
        assert_eq!((r.format, r.width, r.height, r.levels), (Format::Bc1, 64, 32, 4));
        assert_eq!(&r.rgba(3).unwrap().2[..4], &[255, 0, 0, 255]);
        let g = tex("glass");
        assert_eq!((g.format, g.levels), (Format::Bc3, 3));
        let px = &g.rgba(0).unwrap().2[..4];
        assert!(px[2] == 255 && px[3].abs_diff(128) <= 2, "{px:?}");
        assert_eq!(&tex("raw").rgba(0).unwrap().2[..4], &[255, 0, 0, 255]);
        assert_eq!(tex("probe_renamed").format, Format::Bgra8);

        // Removing every texture leaves a valid empty dictionary, which can grow again.
        let removals: Vec<Edit> = ytd.textures.iter().map(|t| Edit::Remove(&t.name)).collect();
        let empty = edit_textures(&edited, &removals).unwrap();
        assert!(TextureDictionary::parse(&empty).unwrap().textures.is_empty());
        let again = edit_textures(&empty, &[Edit::Add { name: "new", file: &red, format: None }]).unwrap();
        assert_eq!(TextureDictionary::parse(&again).unwrap().textures[0].name, "new");
    }
}

#[test]
fn replace_from_png_keeps_the_format() {
    use paleto_formats::{Edit, edit_textures};
    let edits = [Edit::Replace { name: "probe", file: &png(8, 8, [10, 20, 30, 255]), format: None }];
    let ytd = TextureDictionary::parse(&edit_textures(&build(false), &edits).unwrap()).unwrap();
    let tex = &ytd.textures[0];
    assert_eq!((tex.format, tex.width, tex.height, tex.levels), (Format::Bgra8, 8, 8, 2));
    assert_eq!(&tex.rgba(1).unwrap().2[..4], &[10, 20, 30, 255]);
}

#[test]
fn edit_errors() {
    use paleto_formats::{Edit, edit_textures};
    let ytd = build(false);
    let pic = png(4, 4, [0; 4]);
    let err = |edit: Edit| edit_textures(&ytd, &[edit]).unwrap_err().to_string();
    assert_eq!(err(Edit::Add { name: "Probe", file: &pic, format: None }), "a texture named Probe already exists");
    assert_eq!(err(Edit::Add { name: "", file: &pic, format: None }), "invalid texture name \"\"");
    assert_eq!(err(Edit::Remove("nope")), "no texture named nope");
    assert_eq!(err(Edit::Rename { from: "nope", to: "x" }), "no texture named nope");
    assert!(err(Edit::Add { name: "x", file: &pic, format: Some(Format::Bc6h) }).contains("BC6H"));
    assert!(err(Edit::Add { name: "x", file: b"GIF89a", format: None }).contains("DDS or PNG"));
    // Changing only the case of a name is allowed.
    let renamed = edit_textures(&ytd, &[Edit::Rename { from: "probe", to: "PROBE" }]).unwrap();
    assert_eq!(TextureDictionary::parse(&renamed).unwrap().textures[0].name, "PROBE");
}

fn joaat(name: &str) -> u32 {
    let mut h: u32 = 0;
    for b in name.to_ascii_lowercase().bytes() {
        h = h.wrapping_add(b as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}
