//! The exports of the previewed content (textures, the CodeWalker XML model, sounds) and the
//! extraction of archive entries into a folder, shared with the toolbar's Extract action.

use std::path::{Path, PathBuf};

use eframe::egui;
use paleto_core::{ArchivePath, vfs};
use paleto_rpf::Keys;

use crate::actions::Action;
use crate::app::App;
use crate::audio::AudioRequest;
use crate::export_window::ExportWindow;
use crate::textures::Export;

impl App {
    pub fn export_textures(&mut self, export: Export, dest: &Path, ctx: &egui::Context) {
        self.apply(Action::ExportTextures(export, Some(dest.to_path_buf())), ctx);
    }

    /// Exports the previewed model as CodeWalker XML with its textures into a folder.
    pub fn export_model(&mut self, dest: &Path, ctx: &egui::Context) {
        self.apply(Action::ExportModel(Some(dest.to_path_buf())), ctx);
    }

    /// Replaces a sound of the previewed audio container with a WAV or MP3 file.
    pub fn replace_sound(&mut self, id: u32, file: &Path, ctx: &egui::Context) {
        self.apply(Action::ReplaceSound(id, Some(file.to_path_buf())), ctx);
    }

    /// Extracts the named entries of the open archive folder into `dir`, as the toolbar does.
    pub fn extract_to(&mut self, names: Vec<String>, dir: &Path, ctx: &egui::Context) {
        self.apply(Action::Extract(names, Some(dir.to_path_buf())), ctx);
    }

    /// Exports every sound of the previewed audio container into `dir`.
    pub fn export_sounds(&mut self, dir: &Path, ctx: &egui::Context) {
        self.apply(Action::ExportSounds(AudioRequest::ExportAll, Some(dir.to_path_buf())), ctx);
    }

    /// Changes listed and selected in the export window, if open.
    pub fn export_counts(&self) -> Option<(usize, usize)> {
        self.export.as_ref().map(ExportWindow::counts)
    }

    pub fn open_export(&mut self, ctx: &egui::Context) {
        self.apply(Action::Export(true), ctx);
    }
}

/// Extracts the named entries (folders with all they hold) of `location` into `dest`: into a game folder one
/// transaction for all of them, through the checks of every other change to a game (see [`vfs::write_into`]).
/// Returns how many files, and what the writer noted.
pub(crate) fn extract(location: &ArchivePath, keys: Option<&Keys>, names: &[String], dest: &Path) -> Result<(usize, Vec<String>), String> {
    let (archive, dir) = location.open(keys).map_err(|e| e.to_string())?;
    let mut files: Vec<(String, usize)> = Vec::new();
    for name in names {
        let idx = archive.children(dir).find(|&i| &archive.entries[i].name == name).ok_or(format!("{name} not found"))?;
        if archive.entries[idx].is_dir() {
            files.extend(archive.walk(idx).into_iter().filter(|(_, i)| !archive.entries[*i].is_dir()).map(|(p, i)| (format!("{name}/{p}"), i)));
        } else {
            files.push((name.clone(), idx));
        }
    }
    // Every name is checked before anything is written: the archive is not trusted.
    for (rel, _) in &files {
        vfs::extract_path(dest, rel).map_err(|e| e.to_string())?;
    }
    let (_, notes) = vfs::write_into(dest, "extracting files", |out| {
        for (rel, i) in &files {
            let data = archive.read(*i, keys).map_err(|e| paleto_core::Error::Io(std::io::Error::other(format!("{rel}: {e}"))))?;
            out.write(rel, &data)?;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    Ok((files.len(), notes))
}

/// `message` with what the writer noted when it finished (a backup it could not remove), if anything.
pub(crate) fn with_notes(message: String, notes: &[String]) -> String {
    if notes.is_empty() { message } else { format!("{message} ({})", notes.join("; ")) }
}

/// The file as editable text, if it looks like text.
pub(crate) enum ExportDest {
    File(PathBuf),
    Folder(PathBuf),
}

impl ExportDest {
    /// The folder written into, and the name of the file if the user picked one.
    pub(crate) fn folder_and_name(&self) -> (&Path, Option<String>) {
        match self {
            ExportDest::File(path) => (path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")), Some(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
            ExportDest::Folder(dir) => (dir, None),
        }
    }
}

pub(crate) fn is_ytd(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ytd")
}
