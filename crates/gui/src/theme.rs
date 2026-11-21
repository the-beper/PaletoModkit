//! Light and dark themes: the colours the interface adds to egui's, and the
//! desktop's preference (from the XDG desktop portal, which winit cannot read
//! on Wayland), followed while the app runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{self, Color32};

use crate::settings::Theme;

/// Modified files and unsaved changes.
pub fn modded(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(0xE0, 0xA0, 0x30) } else { Color32::from_rgb(0xA3, 0x5F, 0x00) }
}

pub fn error(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(0xE0, 0x50, 0x50) } else { Color32::from_rgb(0xC6, 0x28, 0x28) }
}

pub fn ok(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(0x50, 0xC0, 0x60) } else { Color32::from_rgb(0x2E, 0x7D, 0x32) }
}

pub fn warning(dark: bool) -> Color32 {
    modded(dark)
}

/// The weak text colour (hints, secondary labels) and the outline of fields, boxes and buttons, each with enough contrast on its own theme's surfaces: 4.5:1 for the text, 3:1 for the outline (WCAG 1.4.3, 1.4.11). egui's are 2.7:1 and no outline. Each theme gets its own value, so they survive a switch.
pub fn readable(ctx: &egui::Context) {
    for (theme, weak, outline) in [(egui::Theme::Dark, 140, 110), (egui::Theme::Light, 102, 138)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals.weak_text_color = Some(Color32::from_gray(weak));
            style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_gray(outline));
        });
    }
}

/// Width in points of a separator or panel line: a whole number of device pixels. egui's 1 point is 1.5 pixels at a scale of 1.5, which comes out one pixel wide in one place and two in another.
fn hairline_width(pixels_per_point: f32) -> f32 {
    pixels_per_point.round().max(1.0) / pixels_per_point
}

/// The scale changes with the text size, the monitor and the keyboard zoom, so this runs every frame.
fn align_hairlines(ctx: &egui::Context) {
    let width = hairline_width(ctx.pixels_per_point());
    let stale = |theme| ctx.style_of(theme).visuals.widgets.noninteractive.bg_stroke.width != width;
    if stale(egui::Theme::Dark) || stale(egui::Theme::Light) {
        ctx.all_styles_mut(|style| style.visuals.widgets.noninteractive.bg_stroke.width = width);
        // The frame in progress has already taken its style.
        ctx.request_repaint();
    }
}

/// The desktop's colour scheme preference: 0 unknown, 1 dark, 2 light (the
/// portal's own values).
#[derive(Clone, Default)]
pub struct SystemTheme(Arc<AtomicU8>);

impl SystemTheme {
    /// Reads the preference, then follows its changes on a thread of its own,
    /// repainting when it changes.
    pub fn watch(ctx: &egui::Context) -> Self {
        let state = SystemTheme::default();
        let (shared, ctx) = (state.0.clone(), ctx.clone());
        let _ = std::thread::Builder::new().name("system theme".into()).spawn(move || {
            zbus::block_on(async move {
                let _ = follow(&shared, &ctx).await;
            })
        });
        state
    }

    pub fn prefers_dark(&self) -> Option<bool> {
        match self.0.load(Ordering::Relaxed) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }
}

async fn follow(state: &AtomicU8, ctx: &egui::Context) -> zbus::Result<()> {
    use futures_lite::StreamExt;
    use zbus::zvariant::{OwnedValue, Value};

    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";
    // The value may come wrapped in a variant (older portals' Read).
    fn scheme(value: &Value) -> u8 {
        match value {
            Value::U32(v) => *v as u8,
            Value::Value(inner) => scheme(inner),
            _ => 0,
        }
    }
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&conn, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings").await?;
    let mut changes = proxy.receive_signal("SettingChanged").await?;
    let current: zbus::Result<OwnedValue> = match proxy.call("ReadOne", &(NAMESPACE, KEY)).await {
        Ok(value) => Ok(value),
        Err(_) => proxy.call("Read", &(NAMESPACE, KEY)).await,
    };
    if let Ok(value) = current {
        state.store(scheme(&value), Ordering::Relaxed);
        ctx.request_repaint();
    }
    while let Some(message) = changes.next().await {
        if let Ok((namespace, key, value)) = message.body().deserialize::<(String, String, OwnedValue)>()
            && namespace == NAMESPACE
            && key == KEY
        {
            state.store(scheme(&value), Ordering::Relaxed);
            ctx.request_repaint();
        }
    }
    Ok(())
}

/// Switches egui to the chosen theme; "same as the system" follows the
/// desktop, else what egui itself detects, else light.
pub fn apply(choice: Theme, system: &SystemTheme, ctx: &egui::Context) {
    let dark = match choice {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => system.prefers_dark().or_else(|| ctx.system_theme().map(|t| t == egui::Theme::Dark)).unwrap_or(false),
    };
    let wanted = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    if ctx.theme() != wanted {
        ctx.set_theme(wanted);
    }
    align_hairlines(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_and_system_preference() {
        let ctx = egui::Context::default();
        let system = SystemTheme::default();
        let theme = |choice, preference: u8| {
            system.0.store(preference, Ordering::Relaxed);
            apply(choice, &system, &ctx);
            ctx.theme()
        };
        assert_eq!(theme(Theme::Light, 1), egui::Theme::Light);
        assert_eq!(theme(Theme::Dark, 2), egui::Theme::Dark);
        assert_eq!(theme(Theme::System, 2), egui::Theme::Light);
        assert_eq!(theme(Theme::System, 1), egui::Theme::Dark);
        // No preference known (and none egui could detect): light.
        assert_eq!(theme(Theme::System, 0), egui::Theme::Light);
        // Every colour differs between the themes.
        for f in [modded, error, ok] {
            assert_ne!(f(true), f(false));
        }
    }

    #[test]
    fn hairlines_are_whole_device_pixels() {
        for ppp in [0.9, 1.0, 1.15, 1.25, 1.5, 1.65, 1.75, 2.0, 2.3, 3.0] {
            let pixels = hairline_width(ppp) * ppp;
            assert!(pixels >= 1.0 && (pixels - pixels.round()).abs() < 1e-4, "{ppp}: {pixels} pixels");
            // Panels reserve room for the width rounded to whole points.
            assert_eq!(hairline_width(ppp).round(), 1.0, "{ppp}");
        }
    }

    #[test]
    fn both_themes_follow_the_scale() {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.5);
        let system = SystemTheme::default();
        ctx.run_ui(egui::RawInput::default(), |ui| apply(Theme::Dark, &system, ui.ctx())).drop_without_applying_deltas();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let width = ctx.style_of(theme).visuals.widgets.noninteractive.bg_stroke.width;
            assert!((width * 1.5 - 2.0).abs() < 1e-4, "{theme:?}: {width}");
        }
    }
}
