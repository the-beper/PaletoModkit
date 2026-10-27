//! GTA V resource formats: RSC7 containers, texture dictionaries (`.ytd`), models, collision, navmeshes and clip dictionaries.

pub mod awc;
mod bc7;
pub mod bounds;
pub mod clips;
mod dds;
mod decode;
pub mod drawable;
mod encode;
pub mod gxt2;
pub mod hash;
mod meta;
pub mod names;
pub mod navmesh;
mod pages;
pub mod rsc;
pub mod ytd;
pub mod xml_export;
mod ytd_write;

pub use dds::{Image, read as read_dds};
pub use encode::{compress, load as load_image};
pub use meta::MetaFile;
pub use ytd::{Format, Texture, TextureDictionary};
pub use ytd_write::{Edit, edit_textures, replace_textures};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid file: {0}")]
    Format(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    /// An edit that cannot be applied, such as a missing or duplicate name.
    #[error("{0}")]
    Edit(String),
}

impl Texture {
    /// The largest mip level as a PNG file.
    pub fn to_png(&self) -> Result<Vec<u8>, Error> {
        let (w, h, rgba) = self.rgba(0).ok_or_else(|| Error::Format("texture has no data".into()))?;
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_raw(w, h, rgba)
            .expect("buffer matches the size")
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| Error::Format(e.to_string()))?;
        Ok(out.into_inner())
    }
}
