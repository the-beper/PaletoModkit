//! Paleto Modkit: archive browser, mods folder and package installer.

pub mod app;
pub mod appimage;
mod audio;
mod highlight;
mod model;
mod crash_hunt_window;
mod crash_window;
mod export_window;
mod mods_check;
mod prefix_window;
mod snapshots_window;
pub mod settings;
mod task;
mod textures;
mod theme;
mod widgets;
#[cfg(target_os = "linux")]
pub mod single_instance;
#[cfg(target_os = "linux")]
mod wayland_activation;
#[cfg(target_os = "linux")]
mod wayland_dnd;

pub use app::{App, Location};
pub use settings::Settings;
pub use textures::{Export, TextureEdit};

pub const NAME: &str = "Paleto Modkit";
/// Reverse-DNS application ID: desktop file, icon and AppStream names.
pub const APP_ID: &str = "io.github.the_beper.PaletoModkit";

pub(crate) const ICON_PNG: &[u8] = include_bytes!("../../../packaging/icons/hicolor/256x256/apps/io.github.the_beper.PaletoModkit.png");

/// Whether this runs inside a Flatpak sandbox, which sees only some folders.
pub(crate) fn sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// The application icon for the window.
pub fn icon() -> eframe::egui::IconData {
    let img = image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png).expect("bundled icon").to_rgba8();
    let (width, height) = img.dimensions();
    eframe::egui::IconData { rgba: img.into_raw(), width, height }
}
