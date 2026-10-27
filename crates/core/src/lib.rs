//! Game folder, mods folder and OIV package handling.

pub mod addon;
pub mod cleanup;
pub mod convert;
pub mod crash_hunt;
pub mod crash_report;
pub mod diagnosis;
pub mod export;
pub mod game;
pub mod loader;
pub mod oiv;
pub mod prefix;
pub mod scripthook;
pub mod shared_textures;
pub mod snapshots;
pub mod texture_batch;
pub mod text;
pub mod vfs;
pub mod xml;

pub use game::{Edition, Game};
pub use oiv::Package;
pub use vfs::ArchivePath;

/// Writes an executable script for a test. Run while another thread forks, a
/// file this process wrote can fail with "Text file busy" (the fork holds it
/// open for writing until it runs its program): `cp` writes it instead.
#[cfg(test)]
pub(crate) fn write_script(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let draft = path.with_extension("draft");
    std::fs::write(&draft, text).unwrap();
    assert!(std::process::Command::new("cp").arg(&draft).arg(path).status().unwrap().success());
    std::fs::remove_file(&draft).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Rpf(#[from] paleto_rpf::Error),
    #[error("{0} is not a GTA V folder (no GTA5.exe or GTA5_Enhanced.exe)")]
    NotAGameFolder(std::path::PathBuf),
    #[error("download failed: {0}")]
    Download(String),
    #[error("package: {0}")]
    Package(String),
    #[error("xml: {0}")]
    Xml(String),
    #[error("conversion: {0}")]
    Convert(String),
    #[error("prefix: {0}")]
    Prefix(String),
    /// Stopped because nobody wants the result any more.
    #[error("cancelled")]
    Cancelled,
}
