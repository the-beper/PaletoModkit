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
/// desktop, else what egui itself detects, else dark.
pub fn apply(choice: Theme, system: &SystemTheme, ctx: &egui::Context) {
    let dark = match choice {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => system.prefers_dark().or_else(|| ctx.system_theme().map(|t| t == egui::Theme::Dark)).unwrap_or(true),
    };
    let wanted = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    if ctx.theme() != wanted {
        ctx.set_theme(wanted);
    }
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
        // No preference known (and none egui could detect): dark.
        assert_eq!(theme(Theme::System, 0), egui::Theme::Dark);
        // Every colour differs between the themes.
        for f in [modded, error, ok] {
            assert_ne!(f(true), f(false));
        }
    }
}
