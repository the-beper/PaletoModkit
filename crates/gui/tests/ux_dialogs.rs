//! What every dialog has in common: the title as strong as the section titles, padding of at least the margin all round, one of three widths, a close button as tall as any control, and picture captions under the pictures.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use egui::accesskit::Role;
use egui::{Color32, Id, Rect};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use paleto_modkit::App;
use paleto_modkit::app::Info;

/// The window margin, which is also the least a dialog's text is from its border.
const MARGIN: f32 = 8.0;

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

fn zip_of(path: &Path, files: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in files {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 30, 30, 255])).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

/// The app over a fake Enhanced game with a loose file in its folder, which Delete refuses with a long explanation.
fn start(dir: &Path) -> Harness<'static, App> {
    let root = dir.join("Grand Theft Auto V Enhanced");
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
    std::fs::write(root.join("loose.txt"), b"x").unwrap();
    let mut harness = Harness::builder().with_size([1000.0, 700.0]).build_eframe(|cc| App::new(cc));
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(&root, &ctx);
    settle(&mut harness);
    harness
}

/// A super package of two modules, one with a before/after pair and one with a captioned picture, and a plain one without pictures.
fn packages(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let metadata = |name: &str| format!("<metadata><name>{name}</name><version><major>1</major><minor>0</minor></version><author><displayName>Me</displayName></author><description>d</description></metadata>");
    let module = |id: &str, media: &str| format!(r#"<module id="{id}" required="false" name="{id}">{media}<install><content><add source="{id}\a.asi">scripts\{id}.asi</add></content></install></module>"#);
    let with_pictures = format!(
        r#"<superpackage version="1.0" id="{{x}}" target="Five">{}<modules>{}{}</modules></superpackage>"#,
        metadata("Pics"),
        module("Pair", r#"<media><compare before="media/a.png" after="media/a.png"/></media>"#),
        module("Single", r#"<media><image src="media/a.png" caption="Dusk"/></media>"#)
    );
    let without_pictures = format!(r#"<superpackage version="1.0" id="{{x}}" target="Five">{}<modules>{}</modules></superpackage>"#, metadata("Bare"), module("Base", ""));
    let (pics, bare, oiv) = (dir.join("pics.oivs"), dir.join("bare.oivs"), dir.join("plain.oiv"));
    zip_of(&pics, &[("super.xml", with_pictures.as_bytes()), ("media/a.png", &png()), ("content/Pair/a.asi", b"a"), ("content/Single/a.asi", b"a")]);
    zip_of(&bare, &[("super.xml", without_pictures.as_bytes()), ("content/Base/a.asi", b"a")]);
    let assembly = r#"<package version="2.2" target="Five"><metadata><name>Plain</name></metadata><content/></package>"#;
    zip_of(&oiv, &[("assembly.xml", assembly.as_bytes())]);
    (pics, bare, oiv)
}

fn open_package(harness: &mut Harness<'_, App>, package: &Path) {
    let ctx = harness.ctx.clone();
    harness.state_mut().open_package(package, &ctx);
    harness.run_steps(10);
}

/// The outside of the dialog the app calls `id`.
fn dialog_rect(harness: &Harness<'_, App>, id: &str) -> Rect {
    harness.ctx.memory(|m| m.area_rect(Id::unique(id))).unwrap_or_else(|| panic!("the dialog {id} is not open"))
}

fn assert_width(harness: &Harness<'_, App>, id: &str) {
    let width = dialog_rect(harness, id).width();
    let sizes = App::dialog_widths(&harness.ctx);
    assert!(sizes.iter().any(|size| (size - width).abs() <= 1.5), "{id} is {width} wide; a dialog is one of {sizes:?}");
}

/// The text of the dialog starts at least a margin from the border, on the left and at the top.
fn assert_padding(harness: &Harness<'_, App>, id: &str, first_label: &str) {
    let (dialog, label) = (dialog_rect(harness, id), harness.get_by_label(first_label).rect());
    assert!(label.left() - dialog.left() >= MARGIN, "{id}: {first_label:?} is {} from the left border", label.left() - dialog.left());
    assert!(label.top() - dialog.top() >= MARGIN, "{id}: {first_label:?} is {} from the top border", label.top() - dialog.top());
}

fn collect_texts(shape: &egui::Shape, out: &mut Vec<(String, Color32)>) {
    match shape {
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect_texts(shape, out)),
        egui::Shape::Text(text) => {
            let own = text.galley.job.sections.first().map_or(Color32::PLACEHOLDER, |section| section.format.color);
            let color = text.override_text_color.unwrap_or(if own == Color32::PLACEHOLDER { text.fallback_color } else { own });
            out.push((text.galley.text().to_string(), color));
        }
        _ => {}
    }
}

fn luma(color: Color32) -> f32 {
    (0.2126 * f32::from(color.r()) + 0.7152 * f32::from(color.g()) + 0.0722 * f32::from(color.b())) / 255.0
}

/// The title is painted at least as far from the dialog's background as plain text and as strong text are.
fn assert_title_not_weaker(harness: &Harness<'_, App>, title: &str) {
    let mut texts = Vec::new();
    harness.output().shapes.iter().for_each(|clipped| collect_texts(&clipped.shape, &mut texts));
    let (_, color) = texts.iter().find(|(text, _)| text == title).unwrap_or_else(|| panic!("{title:?} is not painted; the texts are {:?}", texts.iter().map(|(t, _)| t).collect::<Vec<_>>()));
    let visuals = harness.ctx.global_style().visuals.clone();
    let contrast = |c: Color32| (luma(c) - luma(visuals.window_fill)).abs();
    assert!(contrast(*color) + 0.01 >= contrast(visuals.text_color()), "the title {title:?} ({color:?}) is weaker than the text ({:?})", visuals.text_color());
    assert!(contrast(*color) + 0.01 >= contrast(visuals.strong_text_color()), "the title {title:?} ({color:?}) is weaker than a section title ({:?})", visuals.strong_text_color());
}

/// The close button of a titled dialog is as tall as the other controls.
fn assert_close_button(harness: &Harness<'_, App>) {
    let least = harness.ctx.global_style().spacing.interact_size.y;
    let height = harness.get_by_role_and_label(Role::Button, "✖").rect().height();
    assert!(height >= least - 0.01, "the close button is {height} tall, a control is at least {least}");
}

#[test]
fn the_delete_confirmation_has_the_padding_title_and_width_of_every_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start(dir.path());
    harness.state_mut().ask_delete(&["loose.txt"]);
    harness.run_steps(8);
    assert_padding(&harness, "delete", "Delete this item?");
    assert_title_not_weaker(&harness, "Delete this item?");
    assert_width(&harness, "delete");
}

#[test]
fn a_titled_dialog_has_a_close_button_as_tall_as_a_control() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start(dir.path());
    let ctx = harness.ctx.clone();
    harness.state_mut().show_info(Some(Info::GettingStarted), &ctx);
    harness.run_steps(8);
    assert_close_button(&harness);
    assert_padding(&harness, "info", "Getting started");
    assert_title_not_weaker(&harness, "Getting started");
    assert_width(&harness, "info");
    harness.state_mut().show_info(None, &ctx);
    harness.run_steps(3);

    harness.state_mut().offer_crash(SystemTime::now() - Duration::from_secs(60), "The crash happened in Trainer.asi.");
    harness.run_steps(8);
    assert_close_button(&harness);
    assert_padding(&harness, "notice", "The game crashed");
    assert_title_not_weaker(&harness, "The game crashed");
    assert_width(&harness, "notice");
}

#[test]
fn the_install_dialog_of_a_package_without_pictures_has_the_medium_width() {
    let dir = tempfile::tempdir().unwrap();
    let (_, bare, oiv) = packages(dir.path());
    for (package, name) in [(bare, "Bare"), (oiv, "Plain")] {
        let mut harness = start(&dir.path().join(name));
        open_package(&mut harness, &package);
        assert_padding(&harness, "install", name);
        assert_title_not_weaker(&harness, name);
        assert_width(&harness, "install");
    }
}

#[test]
fn picture_captions_are_under_the_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let (pics, _, _) = packages(dir.path());
    let mut harness = start(&dir.path().join("game"));
    open_package(&mut harness, &pics);
    let images_bottom = |harness: &Harness<'_, App>| {
        let dialog = dialog_rect(harness, "install");
        let bottoms: Vec<f32> = harness.query_all_by_role(Role::Image).filter(|image| dialog.contains(image.rect().center())).map(|image| image.rect().bottom()).collect();
        assert!(!bottoms.is_empty(), "no picture is shown in the dialog");
        bottoms.into_iter().fold(f32::MIN, f32::max)
    };
    // The first part with pictures is the pair.
    let bottom = images_bottom(&harness);
    for name in ["Before", "After"] {
        let top = harness.get_by_label(name).rect().top();
        assert!(top >= bottom - 0.5, "{name} starts at {top}, above the bottom of the pictures at {bottom}");
    }
    harness.get_by_role_and_label(Role::CheckBox, "Single").click();
    harness.run_steps(3);
    let (bottom, top) = (images_bottom(&harness), harness.get_by_label("Dusk").rect().top());
    assert!(top >= bottom - 0.5, "the caption starts at {top}, above the bottom of the picture at {bottom}");
}
