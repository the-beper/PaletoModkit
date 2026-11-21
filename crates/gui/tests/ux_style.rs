//! The numbers a UX audit measured, held for every theme, text size and density: control height, text and outline contrast, smallest text.

use std::sync::Arc;

use egui::{Color32, TextStyle, Theme};
use paleto_modkit::Settings;
use paleto_modkit::settings::{Density, TextSize};

struct Case {
    name: String,
    style: Arc<egui::Style>,
    zoom: f32,
    density: Density,
}

/// Every theme under every text size and density, as the app leaves the context after `Settings::apply`.
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for (text_size, text_name) in TextSize::ALL {
        for (density, density_name) in Density::ALL {
            let ctx = egui::Context::default();
            Settings { text_size, density, ..Settings::default() }.apply(&ctx);
            // The zoom factor takes effect at the start of the next pass.
            ctx.run_ui(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
            for theme in [Theme::Light, Theme::Dark] {
                // Switch the way the app does: the style read is the one the widgets get.
                ctx.set_theme(theme);
                out.push(Case { name: format!("{theme:?}, {text_name} text, {density_name} density"), style: ctx.global_style(), zoom: ctx.zoom_factor(), density });
            }
        }
    }
    out
}

fn luminance(color: Color32) -> f64 {
    let linear = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

/// WCAG contrast ratio between two opaque colours.
fn contrast(a: Color32, b: Color32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `top` over an opaque `below`. The channels of a `Color32` are premultiplied.
fn over(top: Color32, below: Color32) -> Color32 {
    let keep = f32::from(255 - top.a()) / 255.0;
    let mix = |t: u8, b: u8| (f32::from(t) + f32::from(b) * keep).round().min(255.0) as u8;
    Color32::from_rgb(mix(top.r(), below.r()), mix(top.g(), below.g()), mix(top.b(), below.b()))
}

fn surfaces(style: &egui::Style) -> [(&'static str, Color32); 4] {
    let v = &style.visuals;
    [("panel_fill", v.panel_fill), ("window_fill", v.window_fill), ("faint_bg_color", over(v.faint_bg_color, v.panel_fill)), ("extreme_bg_color", v.extreme_bg_color)]
}

#[test]
fn controls_are_tall_enough() {
    for case in cases() {
        let wanted = match case.density {
            Density::Compact => 20.0,
            Density::Normal => 24.0,
            Density::Comfortable => 28.0,
        };
        let height = case.style.spacing.interact_size.y;
        assert!(height >= wanted, "{}: interact_size.y {height} < {wanted}", case.name);
    }
}

#[test]
fn weak_text_is_readable_on_every_surface() {
    for case in cases() {
        let weak = case.style.visuals.weak_text_color();
        for (surface, color) in surfaces(&case.style) {
            let ratio = contrast(weak, color);
            assert!(ratio >= 4.5, "{}: weak text on {surface} is {ratio:.2}:1", case.name);
        }
    }
}

#[test]
fn component_outlines_are_visible() {
    for case in cases() {
        let stroke = case.style.visuals.widgets.inactive.bg_stroke;
        assert!(stroke.width >= 1.0, "{}: outline is {} wide", case.name, stroke.width);
        let (panel, window, extreme) = (case.style.visuals.panel_fill, case.style.visuals.window_fill, case.style.visuals.extreme_bg_color);
        for (surface, color) in [("panel_fill", panel), ("window_fill", window), ("extreme_bg_color", extreme)] {
            let ratio = contrast(stroke.color, color);
            assert!(ratio >= 3.0, "{}: outline on {surface} is {ratio:.2}:1", case.name);
        }
    }
}

#[test]
fn smallest_text_stays_legible() {
    for case in cases() {
        let small = case.style.text_styles[&TextStyle::Small].size * case.zoom;
        assert!(small >= 10.8, "{}: Small text is {small} px", case.name);
        // The other sizes keep egui's, scaled by 1.1.
        for (text_style, size) in [(TextStyle::Body, 14.3), (TextStyle::Heading, 19.8)] {
            let got = case.style.text_styles[&text_style].size;
            assert!((got - size).abs() < 0.01, "{}: {text_style} is {got}", case.name);
        }
    }
}

#[test]
fn contrast_function_matches_known_values() {
    assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-9);
    assert!((contrast(Color32::from_gray(119), Color32::WHITE) - 4.48).abs() < 0.01);
}
