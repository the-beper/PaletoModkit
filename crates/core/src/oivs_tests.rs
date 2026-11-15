//! Super packages (`.oivs`): what `super.xml` offers, what a selection installs, and what is refused.

use std::path::{Path, PathBuf};

use paleto_rpf::ArchiveEditor;

use crate::fixtures::*;
use crate::game::Edition;
use crate::oiv::{Media, Selection};
use crate::Package;

const SUPER: &str = r#"<superpackage version="1.0" target="Five"><gameversion> Gen9 </gameversion>
<metadata><name>Pack</name><version><major>1</major><minor>2</minor></version><author><displayName>Me</displayName></author><description><![CDATA[Many things]]></description></metadata>
<colors><headerBackground useBlackTextColor="False">$FF1e1e1e</headerBackground></colors>
<modules>
<module id="base" required="True" name="Base"><description><![CDATA[The base]]></description>
  <install><content><archive path="x64\a.rpf" createIfNotExist="False" type="RPF7"><add source="base\a.txt">a.txt</add><add source="base\set.dat">set.dat</add></archive></content></install></module>
<module id="extra" required="false" default="true" name="Extra">
  <media><compare title="T" before="media/b.png" after="media/a.png"/><image src="media/i.png" caption="C"/></media>
  <install><folder source="extra\loose"/><content><add source="extra\e.asi">scripts\e.asi</add></content></install></module>
<module id="off" name=""><install><content><add source="off\o.asi">scripts\o.asi</add></content></install></module>
</modules>
<groups>
<group id="alt" type="single" title="Alternative" allowNone="true" default="none">
  <option id="one" name="One"><install><content><archive path="x64\a.rpf" type="RPF7"><add source="alt\one\set.dat">set.dat</add></archive></content></install></option>
  <option id="two" name="Two"><install><content><archive path="x64\a.rpf" type="RPF7"><add source="alt\two\set.dat">set.dat</add></archive></content></install></option>
</group>
<group id="must" allowNone="false" default="nope">
  <option id="x" name="X"><install><content><add source="must\x.asi">scripts\m.asi</add></content></install></option>
</group>
</groups></superpackage>"#;

const FILES: &[(&str, &[u8])] = &[
    ("content/base/a.txt", b"base-a"),
    ("content/base/set.dat", b"base-set"),
    ("content/extra/e.asi", b"e"),
    // The folder's own spelling is kept for the names below it.
    ("content/Extra/Loose/scripts/Loose.ini", b"loose"),
    ("content/Extra/Loose/x64/data/f.dat", b"f"),
    ("content/off/o.asi", b"o"),
    ("content/alt/one/set.dat", b"one-set"),
    ("content/alt/two/set.dat", b"two-set"),
    ("content/must/x.asi", b"m"),
    ("media/a.png", b"after"),
    ("media/b.png", b"before"),
];

/// A super package `dir/name` with this `super.xml` and these files.
fn oivs(dir: &Path, name: &str, manifest: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let entries = std::iter::once(("super.xml".to_string(), manifest.as_bytes().to_vec())).chain(files.iter().map(|(n, d)| (n.to_string(), d.to_vec())));
    oiv_raw(dir, name, entries, true)
}

#[test]
fn reads_what_a_super_package_offers() {
    let dir = tempfile::tempdir().unwrap();
    let mut pkg = Package::open(&oivs(dir.path(), "p.oivs", SUPER, FILES)).unwrap();
    let m = &pkg.metadata;
    assert_eq!((m.name.as_str(), m.version.as_str(), m.author.as_str(), m.description.as_str(), m.format_version.as_str()), ("Pack", "1.2", "Me", "Many things", "1.0"));
    assert_eq!(m.game_version, Some(Edition::Enhanced));
    let choices = pkg.choices.clone().unwrap();
    let modules: Vec<(&str, bool, bool)> = choices.modules.iter().map(|m| (m.part.label(), m.required, m.default)).collect();
    assert_eq!(modules, [("Base", true, false), ("Extra", false, true), ("off", false, false)]);
    assert_eq!(choices.modules[0].part.description, "The base");
    assert_eq!(choices.modules[1].part.media, [
        Media::Compare { title: "T".into(), before: "media/b.png".into(), after: "media/a.png".into() },
        Media::Image { src: "media/i.png".into(), caption: "C".into() },
    ]);
    let groups: Vec<(&str, bool, Option<usize>, usize)> = choices.groups.iter().map(|g| (g.label(), g.allow_none, g.default, g.options.len())).collect();
    // A group that must have an option and names none it has starts on its first.
    assert_eq!(groups, [("Alternative", true, None, 2), ("must", false, Some(0), 1)]);
    assert_eq!(choices.default_selection(), Selection { modules: vec![true, true, false], groups: vec![None, Some(0)] });
    assert!(choices.installs_anything(&choices.default_selection()));
    assert_eq!(pkg.media("media/a.png").as_deref(), Some(b"after".as_slice()));
    assert_eq!(pkg.media("\\media\\B.PNG").as_deref(), Some(b"before".as_slice()));
    assert_eq!(pkg.media("media/i.png"), None);
}

#[test]
fn installs_the_chosen_parts_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let game = game_folder(dir.path(), Edition::Enhanced);
    let mut pkg = Package::open(&oivs(dir.path(), "p.oivs", SUPER, FILES)).unwrap();
    let selection = pkg.choices.as_ref().unwrap().parse_selection("alt=two").unwrap();
    pkg.select(&selection).unwrap();
    pkg.install_with(&game, None).unwrap();

    let root = &game.root;
    let rpf = ArchiveEditor::open(&root.join("mods/x64/a.rpf"), None).unwrap();
    assert_eq!(rpf.read("a.txt", None).unwrap(), b"base-a");
    // The option comes after the module and replaces its file.
    assert_eq!(rpf.read("set.dat", None).unwrap(), b"two-set");
    assert_eq!(std::fs::read(root.join("scripts/e.asi")).unwrap(), b"e");
    assert_eq!(std::fs::read(root.join("scripts/Loose.ini")).unwrap(), b"loose");
    assert_eq!(std::fs::read(root.join("mods/x64/data/f.dat")).unwrap(), b"f");
    assert_eq!(std::fs::read(root.join("scripts/m.asi")).unwrap(), b"m");
    assert!(!root.join("scripts/o.asi").exists(), "a module that was not chosen");
    assert_eq!(ArchiveEditor::open(&root.join("x64/a.rpf"), None).unwrap().read("a.txt", None).unwrap(), b"a", "the original");
}

#[test]
fn a_selection_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = Package::open(&oivs(dir.path(), "p.oivs", SUPER, FILES)).unwrap();
    let choices = pkg.choices.as_ref().unwrap();
    assert_eq!(choices.parse_selection(" OFF; -extra, alt = One ").unwrap(), Selection { modules: vec![true, false, true], groups: vec![Some(0), Some(0)] });
    assert_eq!(choices.parse_selection("alt=one,alt=none,!extra").unwrap(), Selection { modules: vec![true, false, false], groups: vec![None, Some(0)] });
    for (spec, error) in [
        ("nope", "the package has no module nope"),
        ("zz=one", "the package has no group zz"),
        ("alt=three", "the group alt has no option three"),
        ("-base", "the module base is required"),
        ("must=none", "must needs one of its options"),
    ] {
        assert!(choices.parse_selection(spec).unwrap_err().to_string().contains(error), "{spec}");
    }
    let mut pkg = pkg;
    assert!(pkg.select(&Selection { modules: vec![true], groups: vec![None, None] }).is_err());
}

#[test]
fn refuses_what_it_cannot_read_safely() {
    let dir = tempfile::tempdir().unwrap();
    let error = |manifest: &str, files: &[(&str, &[u8])]| Package::open(&oivs(dir.path(), "bad.oivs", manifest, files)).err().unwrap().to_string();
    assert!(error(&SUPER.replace(r#"type="single""#, r#"type="multi""#), FILES).contains("unsupported group type multi"));
    assert!(error(&SUPER.replace(r#"version="1.0""#, r#"version="2.0""#), FILES).contains("unsupported package format 2.0"));
    assert!(error(&SUPER.replace("superpackage", "package"), FILES).contains("root is not <superpackage>"));
    assert!(error(&SUPER.replace("<folder ", "<script "), FILES).contains("unknown install item <script>"));
    // A file below a folder whose name leads out of the game folder.
    let evil = [FILES, &[("content/extra/loose/../../../evil.asi", b"x".as_slice())]].concat();
    assert!(error(SUPER, &evil).contains("unsafe path"));
    // One folder named over and over adds no more files than a package may hold.
    let many = SUPER.replace(r#"<folder source="extra\loose"/>"#, &r#"<folder source="extra\loose"/>"#.repeat(20));
    let limits = crate::budget::Limits { entries: 30, ..crate::budget::Limits::PACKAGE };
    let err = Package::open_with(&oivs(dir.path(), "many.oivs", &many, FILES), limits).err().unwrap();
    assert!(err.to_string().contains("folders add more files than a package may hold"), "{err}");
}

#[test]
fn an_empty_folder_source_adds_nothing_and_a_version_is_not_needed() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = SUPER.replace(r#"<folder source="extra\loose"/>"#, r#"<folder source=" \ "/><folder/>"#).replace(r#"<superpackage version="1.0""#, "<superpackage");
    let game = game_folder(dir.path(), Edition::Enhanced);
    let mut pkg = Package::open(&oivs(dir.path(), "p.oivs", &manifest, FILES)).unwrap();
    assert_eq!(pkg.metadata.format_version, "");
    pkg.install_with(&game, None).unwrap();
    // Not the whole of content/ into the game folder.
    assert!(!game.root.join("base/a.txt").exists() && !game.root.join("scripts/Loose.ini").exists());
    assert_eq!(std::fs::read(game.root.join("scripts/e.asi")).unwrap(), b"e");
}

#[test]
fn a_plain_package_says_its_edition_too() {
    let dir = tempfile::tempdir().unwrap();
    let assembly = |extra: &str| format!(r#"<package version="2.2" target="Five">{extra}<metadata><name>X</name></metadata><content/></package>"#);
    let open = |name: &str, extra: &str| Package::open(&oiv_raw(dir.path(), name, [("assembly.xml".to_string(), assembly(extra).into_bytes())], true)).unwrap();
    assert_eq!(open("a.oiv", "<gameversion>gen8</gameversion>").metadata.game_version, Some(Edition::Legacy));
    assert_eq!(open("b.oiv", "<metadata><gameversion>Enhanced</gameversion></metadata>").metadata.game_version, Some(Edition::Enhanced));
    let plain = open("c.oiv", "");
    assert_eq!(plain.metadata.game_version, None);
    assert!(plain.choices.is_none());
}

#[test]
fn legacy_files_of_every_part_are_found() {
    let res = |v: u32| [b"RSC7".as_slice(), &v.to_le_bytes(), &((v >> 4) << 28).to_le_bytes(), &((v & 15) << 28).to_le_bytes(), b"payload"].concat();
    let manifest = r#"<superpackage version="1.0"><modules>
        <module id="car" name="Car"><install><content><archive path="x64e.rpf" type="RPF7"><add source="car\car.yft">car.yft</add></archive></content></install></module>
        </modules></superpackage>"#;
    let dir = tempfile::tempdir().unwrap();
    let car = res(162);
    let mut pkg = Package::open(&oivs(dir.path(), "car.oivs", manifest, &[("content/car/car.yft", &car)])).unwrap();
    // Not chosen, yet in the package.
    assert!(!pkg.choices.as_ref().unwrap().installs_anything(&pkg.choices.as_ref().unwrap().default_selection()));
    assert_eq!(pkg.legacy_files(None).unwrap(), ["car/car.yft"]);
}

#[test]
fn the_size_of_a_picture_is_read_from_its_header() {
    let dir = tempfile::tempdir().unwrap();
    let head = [b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".as_slice(), &1280u32.to_be_bytes(), &720u32.to_be_bytes(), b"and the rest"].concat();
    let mut pkg = Package::open(&oivs(dir.path(), "p.oivs", SUPER, &[("media/a.png", &head), ("media/b.png", b"not a picture at all")])).unwrap();
    assert_eq!(pkg.media_size("\\Media\\A.png"), Some((1280, 720)));
    assert_eq!(pkg.media_size("media/b.png"), None);
    assert_eq!(pkg.media_size("media/none.png"), None);
}
