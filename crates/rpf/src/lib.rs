//! Reading GTA V RPF7 archives (Legacy and Enhanced).

pub mod archive;
pub mod crypto;
pub mod edit;
mod hashes;
pub mod keys;

pub use archive::{Archive, Encryption, Entry, EntryKind, StoredFile};
pub use edit::ArchiveEditor;
pub use keys::Keys;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0} not found in the game executable")]
    KeyNotFound(&'static str),
    #[error("stored keys are invalid")]
    InvalidKeys,
    #[error("this archive is encrypted: game keys are required")]
    KeysRequired,
    #[error("{0} is not an RPF7 archive")]
    NotAnArchive(String),
    #[error("{0} is not a file")]
    NotAFile(String),
    #[error("corrupt archive: {0}")]
    Corrupt(&'static str),
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0} is not a folder")]
    NotADirectory(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("{0} is in the archive twice (names differ only in case, or a file and a folder share it)")]
    DuplicateName(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("{0} is too large for an RPF7 archive")]
    TooLarge(String),
    #[error("not enough space to edit the archive in place ({needed} bytes needed)")]
    NoSpace { needed: u64 },
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}
