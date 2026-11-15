//! Super packages (.oivs) in the install dialog: the choices and their pictures, what an install writes of them, and the warning for a package made for the other edition.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use paleto_core::oiv::Selection;
use paleto_modkit::App;

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..500 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(3);
}

fn png(color: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba(color)).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

fn zip_of(path: &Path, files: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in files {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

/// A super package of a module `base` (required or not), an optional module `extra` with a before/after pair, and a group of two options (or none) whose first has a picture and whose second names a picture the package lacks.
fn super_package(dir: &Path, game_version: &str, base_required: bool) -> PathBuf {
    let xml = format!(
        r#"<superpackage version="1.0" id="{{x}}" target="Five">{game_version}
<metadata><name>Pack</name><version><major>1</major><minor>0</minor></version><author><displayName>Me</displayName></author><description>d</description></metadata>
<modules>
<module id="base" required="{base_required}" name="Base"><description>Always needed</description><install><content><add source="base\a.asi">scripts\a.asi</add></content></install></module>
<module id="extra" required="false" name="Extra"><description>Optional extra</description><media><compare before="media/b.png" after="media/a.png"/></media><install><content><add source="extra\b.asi">scripts\b.asi</add></content></install></module>
</modules>
<groups><group id="g" type="single" title="Choice" allowNone="true" default="none">
<option id="one" name="One"><media><image src="media/a.png" caption="Dusk"/></media><install><content><add source="g\one.asi">scripts\c.asi</add></content></install></option>
<option id="two" name="Two"><media><image src="media/missing.png"/></media><install><content><add source="g\two.asi">scripts\c.asi</add></content></install></option>
</group></groups></superpackage>"#
    );
    let path = dir.join("pack.oivs");
    zip_of(
        &path,
        &[
            ("super.xml", xml.as_bytes()),
            ("content/base/a.asi", b"base"),
            ("content/extra/b.asi", b"extra"),
            ("content/g/one.asi", b"one"),
            ("content/g/two.asi", b"two"),
            ("media/a.png", &png([200, 30, 30, 255])),
            ("media/b.png", &png([30, 30, 200, 255])),
        ],
    );
    path
}

/// The dialog of `package` over a fake Enhanced game folder.
fn open(dir: &Path, package: &Path) -> (Harness<'static, App>, PathBuf) {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness.state_mut().open_package(package, &ctx);
    // A dialog grows to its size over a few frames; a click must find its controls where they stay.
    harness.run_steps(10);
    (harness, root)
}

fn labels(harness: &Harness<'_, App>) -> Vec<String> {
    harness.query_all_by_label_contains("").filter_map(|n| n.accesskit_node().label()).collect()
}

fn selection(modules: &[bool], groups: &[Option<usize>]) -> Option<Selection> {
    Some(Selection { modules: modules.to_vec(), groups: groups.to_vec() })
}

#[test]
fn the_dialog_lists_the_modules_and_the_options() {
    let dir = tempfile::tempdir().unwrap();
    let (harness, _) = open(dir.path(), &super_package(dir.path(), "", true));
    harness.get_by_label("Pack");
    harness.get_by_label("What to install");
    let base = harness.get_by_role_and_label(Role::CheckBox, "Base (required)");
    assert!(base.accesskit_node().is_disabled());
    harness.get_by_label("Always needed");
    let extra = harness.get_by_role_and_label(Role::CheckBox, "Extra");
    assert!(!extra.accesskit_node().is_disabled());
    harness.get_by_label("Optional extra");
    harness.get_by_label("Choice");
    for option in ["One", "Two", "None"] {
        harness.get_by_role_and_label(Role::RadioButton, option);
    }
    assert_eq!(harness.state().install_selection().cloned(), selection(&[true, false], &[None]));
}

#[test]
fn the_pictures_follow_the_item_last_clicked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _) = open(dir.path(), &super_package(dir.path(), "", true));
    // The first item with pictures is shown at first.
    harness.get_by_label("Before");
    harness.get_by_label("After");
    assert!(harness.query_by_label("Dusk").is_none());

    harness.get_by_role_and_label(Role::RadioButton, "One").click();
    harness.run_steps(3);
    harness.get_by_label("Dusk");
    assert!(harness.query_by_label("Before").is_none());

    // A picture the package lacks shows nothing.
    harness.get_by_role_and_label(Role::RadioButton, "Two").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Dusk").is_none() && harness.query_by_label("Before").is_none(), "{:?}", labels(&harness));
}

/// What the dialog chooses is what the install gets (`Package::select` before installing; the install itself needs the
/// game's keys, which a fake game has none of, and is tested in paleto-core).
#[test]
fn ticking_and_choosing_make_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _) = open(dir.path(), &super_package(dir.path(), "", false));
    assert_eq!(harness.state().install_selection().cloned(), selection(&[false, false], &[None]));
    harness.get_by_role_and_label(Role::CheckBox, "Extra").click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::RadioButton, "Two").click();
    harness.run_steps(3);
    assert_eq!(harness.state().install_selection().cloned(), selection(&[false, true], &[Some(1)]));
    harness.get_by_role_and_label(Role::RadioButton, "None").click();
    harness.get_by_role_and_label(Role::CheckBox, "Extra").click();
    harness.run_steps(3);
    assert_eq!(harness.state().install_selection().cloned(), selection(&[false, false], &[None]));

    // A required module cannot be unticked.
    let dir = tempfile::tempdir().unwrap();
    let (mut harness, _) = open(dir.path(), &super_package(dir.path(), "", true));
    harness.get_by_role_and_label(Role::CheckBox, "Base (required)").click();
    harness.run_steps(3);
    assert_eq!(harness.state().install_selection().cloned(), selection(&[true, false], &[None]));
}

#[test]
fn a_package_made_for_the_other_edition_warns() {
    let dir = tempfile::tempdir().unwrap();
    let (harness, _) = open(dir.path(), &super_package(dir.path(), "<gameversion>legacy</gameversion>", true));
    harness.get_by_label("⚠");
    harness.get_by_label_contains("This package is made for GTA V Legacy; the chosen game is Enhanced");

    let dir = tempfile::tempdir().unwrap();
    let (harness, _) = open(dir.path(), &super_package(dir.path(), "<gameversion>enhanced</gameversion>", true));
    assert!(!labels(&harness).iter().any(|l| l.contains("This package is made for")), "{:?}", labels(&harness));

    // A plain package says it in the same way.
    let dir = tempfile::tempdir().unwrap();
    let oiv = dir.path().join("plain.oiv");
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>Plain</name></metadata><gameversion>legacy</gameversion><content/></package>"#;
    zip_of(&oiv, &[("assembly.xml", assembly.as_bytes())]);
    let (harness, _) = open(dir.path(), &oiv);
    harness.get_by_label_contains("This package is made for GTA V Legacy; the chosen game is Enhanced");
    assert!(!labels(&harness).iter().any(|l| l == "What to install"));
}
