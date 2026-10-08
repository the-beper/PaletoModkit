//! Desktop integration for the AppImage.
//!
//! Wayland compositors take a window's icon and name from the `.desktop` file
//! named after its app id, which a bare AppImage does not have. When running
//! as an AppImage, install one (and the icon) into the user's data folder,
//! pointing at the AppImage. Set PALETO_NO_DESKTOP_INTEGRATION=1 to skip it.
//!
//! When the app is also installed otherwise (Flatpak, a package), that
//! installation's entry is used instead: an entry in the user's folder would
//! hide it, and keep hiding it once the AppImage is deleted.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::APP_ID;

const DESKTOP: &str = include_str!("../../../packaging/io.github.the_beper.PaletoModkit.desktop");
const ICON_SVG: &[u8] = include_bytes!("../../../packaging/icons/hicolor/scalable/apps/io.github.the_beper.PaletoModkit.svg");
const ICON_SYMBOLIC: &[u8] =
    include_bytes!("../../../packaging/icons/hicolor/symbolic/apps/io.github.the_beper.PaletoModkit-symbolic.svg");
/// Marks entries written here, so ones installed otherwise are left alone.
const MARKER: &str = "X-Paleto-AppImage=true";

/// Installs or updates the desktop entry when running as an AppImage.
pub fn integrate() {
    let Some(appimage) = std::env::var_os("APPIMAGE") else { return };
    if std::env::var_os("PALETO_NO_DESKTOP_INTEGRATION").is_some() {
        return;
    }
    let Some(data) = data_home() else { return };
    if installed_elsewhere() {
        let _ = remove(&data);
        return;
    }
    if let Ok(true) = install(&data, Path::new(&appimage)) {
        // Let the desktop see the new entry before the window appears.
        let quiet = |cmd: &mut Command| cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = quiet(Command::new("update-desktop-database").arg("-q").arg(data.join("applications")));
        let _ = quiet(&mut Command::new("kbuildsycoca6"));
    }
}

/// Writes the desktop entry and icons under `data`. Returns whether anything changed.
pub fn install(data: &Path, appimage: &Path) -> io::Result<bool> {
    let desktop = data.join("applications").join(format!("{APP_ID}.desktop"));
    let entry = entry(appimage);
    match std::fs::read_to_string(&desktop) {
        Ok(current) if current == entry => return Ok(false),
        Ok(current) if !current.contains(MARKER) => return Ok(false),
        _ => {}
    }
    let icons = data.join("icons/hicolor");
    write(&icons.join(format!("scalable/apps/{APP_ID}.svg")), ICON_SVG)?;
    write(&icons.join(format!("symbolic/apps/{APP_ID}-symbolic.svg")), ICON_SYMBOLIC)?;
    write(&icons.join(format!("256x256/apps/{APP_ID}.png")), crate::ICON_PNG)?;
    write(&desktop, entry.as_bytes())?;
    Ok(true)
}

/// Whether a desktop entry for the app exists in the system data folders
/// (which Flatpak's exports are part of).
fn installed_elsewhere() -> bool {
    let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.split(':').any(|d| Path::new(d).join(format!("applications/{APP_ID}.desktop")).is_file())
}

/// Removes the entry and icons written by [`install`], if they are ours.
pub fn remove(data: &Path) -> io::Result<()> {
    let desktop = data.join("applications").join(format!("{APP_ID}.desktop"));
    if !std::fs::read_to_string(&desktop).is_ok_and(|text| text.contains(MARKER)) {
        return Ok(());
    }
    std::fs::remove_file(desktop)?;
    let icons = data.join("icons/hicolor");
    for icon in [format!("scalable/apps/{APP_ID}.svg"), format!("symbolic/apps/{APP_ID}-symbolic.svg"), format!("256x256/apps/{APP_ID}.png")] {
        let _ = std::fs::remove_file(icons.join(icon));
    }
    Ok(())
}

/// The packaged desktop entry, launching the AppImage.
fn entry(appimage: &Path) -> String {
    let exec = quote(appimage.as_os_str());
    let mut out = String::new();
    for line in DESKTOP.lines() {
        match line.split_once('=') {
            Some(("Exec", _)) => out.push_str(&format!("Exec={exec} %f")),
            Some(("TryExec", _)) => out.push_str(&format!("TryExec={}", appimage.display())),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out.push_str(MARKER);
    out.push('\n');
    out
}

/// Quotes a path for an Exec key (Desktop Entry Specification).
fn quote(path: &OsStr) -> String {
    let mut out = String::from("\"");
    for c in path.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, data)
}

fn data_home() -> Option<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => Some(dir.into()),
        _ => std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_updates_and_respects_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let desktop = data.join(format!("applications/{APP_ID}.desktop"));

        assert!(install(data, Path::new("/opt/My Apps/paleto.AppImage")).unwrap());
        let text = std::fs::read_to_string(&desktop).unwrap();
        assert!(text.contains("Exec=\"/opt/My Apps/paleto.AppImage\" %f\n"));
        assert!(text.contains("TryExec=/opt/My Apps/paleto.AppImage\n"));
        assert!(text.contains(&format!("Icon={APP_ID}\n")));
        assert!(data.join(format!("icons/hicolor/256x256/apps/{APP_ID}.png")).is_file());
        assert!(data.join(format!("icons/hicolor/symbolic/apps/{APP_ID}-symbolic.svg")).is_file());

        // Same AppImage: nothing to do. Moved AppImage: the entry follows it.
        assert!(!install(data, Path::new("/opt/My Apps/paleto.AppImage")).unwrap());
        assert!(install(data, Path::new("/home/me/paleto.AppImage")).unwrap());
        assert!(std::fs::read_to_string(&desktop).unwrap().contains("Exec=\"/home/me/paleto.AppImage\" %f"));

        // Removing it takes the entry and the icons away.
        remove(data).unwrap();
        assert!(!desktop.exists());
        assert!(!data.join(format!("icons/hicolor/256x256/apps/{APP_ID}.png")).exists());

        // An entry installed by install.sh is never replaced nor removed.
        std::fs::write(&desktop, DESKTOP).unwrap();
        assert!(!install(data, Path::new("/elsewhere/paleto.AppImage")).unwrap());
        remove(data).unwrap();
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), DESKTOP);
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(quote(OsStr::new("/a/b\"c$d.AppImage")), "\"/a/b\\\"c\\$d.AppImage\"");
    }
}
