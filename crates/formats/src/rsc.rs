//! RSC7 resource containers: a 16-byte header followed by a raw deflate
//! stream holding the system pages and then the graphics pages.
//!
//! Pointers inside a resource are virtual: 0x5xxxxxxx addresses the system
//! pages, 0x6xxxxxxx the graphics pages.

use std::io::Read;

use flate2::read::DeflateDecoder;

use crate::Error;

const MAGIC: u32 = 0x3743_5352; // "RSC7"
const SYSTEM_BASE: u64 = 0x5000_0000;
const GRAPHICS_BASE: u64 = 0x6000_0000;

pub struct Resource {
    /// Resource version: 13 for Legacy texture dictionaries, 5 for Enhanced ones.
    pub version: u32,
    pub system: Vec<u8>,
    pub graphics: Vec<u8>,
}

impl Resource {
    /// Parses a standalone resource file (as extracted from an archive).
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        if data.len() < 16 || u32_at(data, 0) != MAGIC {
            return Err(Error::Format("not an RSC7 resource".into()));
        }
        let version = u32_at(data, 4);
        let system_size = page_size(u32_at(data, 8));
        let graphics_size = page_size(u32_at(data, 12));
        // The sizes come from the file (up to gigabytes): never more than they
        // say, and memory taken as the data proves it, beyond a first guess.
        let total = system_size + graphics_size;
        let mut all = Vec::with_capacity(total.min(data.len() * 8));
        DeflateDecoder::new(&data[16..]).take(total as u64).read_to_end(&mut all)?;
        let split = system_size.min(all.len());
        let graphics = all.split_off(split);
        Ok(Resource { version, system: all, graphics })
    }

    /// `len` bytes at a virtual address, or fewer if the page ends first.
    pub fn bytes(&self, ptr: u64, len: usize) -> Result<&[u8], Error> {
        let (segment, offset) = match ptr >> 28 {
            5 => (&self.system, ptr - SYSTEM_BASE),
            6 => (&self.graphics, ptr - GRAPHICS_BASE),
            _ => return Err(Error::Format(format!("invalid pointer {ptr:#x}"))),
        };
        let start = offset as usize;
        if start > segment.len() {
            return Err(Error::Format(format!("pointer {ptr:#x} out of range")));
        }
        Ok(&segment[start..segment.len().min(start.saturating_add(len))])
    }

    /// A fixed-size block at a virtual address.
    pub fn block(&self, ptr: u64, len: usize) -> Result<&[u8], Error> {
        let b = self.bytes(ptr, len)?;
        if b.len() < len {
            return Err(Error::Format(format!("block at {ptr:#x} is truncated")));
        }
        Ok(b)
    }

    /// A NUL-terminated string at a virtual address.
    pub fn string(&self, ptr: u64) -> Result<String, Error> {
        let b = self.bytes(ptr, 256)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }
}

/// Size in bytes of the pages described by a page-flags word.
pub fn page_size(flags: u32) -> usize {
    let counts = ((flags >> 27) & 0x1)
        + (((flags >> 26) & 0x1) << 1)
        + (((flags >> 25) & 0x1) << 2)
        + (((flags >> 24) & 0x1) << 3)
        + (((flags >> 17) & 0x7F) << 4)
        + (((flags >> 11) & 0x3F) << 5)
        + (((flags >> 7) & 0xF) << 6)
        + (((flags >> 5) & 0x3) << 7)
        + (((flags >> 4) & 0x1) << 8);
    (0x200usize << (flags & 0xF)) * counts as usize
}

pub(crate) fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn resource(system_flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = [MAGIC, 13, system_flags, 0].iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        deflate.write_all(payload).unwrap();
        out.extend(deflate.finish().unwrap());
        out
    }

    /// Page sizes from the header are believed only as far as the data goes.
    #[test]
    fn sizes_from_the_header() {
        // Pages said to take about 100 GB, holding 1 KB.
        let res = Resource::parse(&resource(u32::MAX, &[7; 1024])).unwrap();
        assert_eq!(res.system, [7; 1024]);
        // One page of 512 bytes, from data that inflates to 10 MB.
        let res = Resource::parse(&resource(0x0800_0000, &vec![0; 10 << 20])).unwrap();
        assert_eq!((res.system.len(), res.graphics.len()), (512, 0));
    }
}
