//! Laying out blocks into resource pages, following CodeWalker's
//! `ResourceBuilder.AssignPositions2` (MIT), whose output the game accepts.
//!
//! A segment is made of pages of up to five sizes (base, 2×, 4×, 8×, 16×);
//! a block never crosses a page boundary. The page-flags word stores the
//! base size shift and the number of pages of each size.

use crate::Error;

const ALIGN: usize = 16;
/// The game allows 128 pages for both segments together.
pub const MAX_PAGES: u32 = 128;

pub struct Layout {
    /// Offset of each block from the start of the segment.
    pub offsets: Vec<usize>,
    /// Page flags (without the version nibble).
    pub flags: u32,
    /// Total segment size.
    pub size: usize,
    pub pages: u32,
}

/// Packs blocks of the given sizes. With `root_first`, block 0 goes first
/// in the first page (the system segment's root structure).
pub fn layout(sizes: &[usize], root_first: bool, max_pages: u32) -> Result<Layout, Error> {
    if sizes.is_empty() {
        return Ok(Layout { offsets: Vec::new(), flags: 0, size: 0, pages: 0 });
    }
    let max_block = *sizes.iter().max().unwrap();
    let min_block = *sizes.iter().min().unwrap();
    let mut shift = 0u32;
    let base = |shift: u32| 0x2000usize << shift;
    while (base(shift) < min_block || base(shift) * 16 < max_block) && shift < 0xF {
        shift += 1;
    }
    if base(shift) * 16 < max_block {
        return Err(Error::Unsupported("a block is too large for a resource page".into()));
    }

    // Root first, the rest largest first.
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    let start = usize::from(root_first);
    order[start..].sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));

    loop {
        let base_size = base(shift);
        let mut largest = 0;
        while base_size << largest < max_block {
            largest += 1;
        }
        // Used bytes of every page, per page size; and (size index, page, offset) per block.
        let mut pages: [Vec<usize>; 5] = Default::default();
        let mut placed = vec![(0usize, 0usize, 0usize); sizes.len()];
        for (n, &block) in order.iter().enumerate() {
            let size = sizes[block];
            if n == 0 {
                pages[largest].push(size);
                placed[block] = (largest, 0, 0);
                continue;
            }
            let mut fit = 0;
            while size > base_size << fit && fit < largest {
                fit += 1;
            }
            // An existing page of this size or larger with room, else a new one.
            let existing = (fit..=largest).find_map(|i| {
                pages[i].iter_mut().enumerate().find_map(|(p, used)| {
                    let offset = used.next_multiple_of(ALIGN);
                    (offset + size <= base_size << i).then(|| {
                        *used = offset + size;
                        (i, p, offset)
                    })
                })
            });
            placed[block] = existing.unwrap_or_else(|| {
                pages[fit].push(size);
                (fit, pages[fit].len() - 1, 0)
            });
        }

        let counts: Vec<u32> = pages.iter().map(|p| p.len() as u32).collect();
        let total: u32 = counts.iter().sum();
        let fits = total <= max_pages && counts[0] <= 0x7F && counts[1] <= 0x3F && counts[2] <= 0xF && counts[3] <= 3 && counts[4] <= 1;
        if !fits {
            if shift >= 0xF {
                return Err(Error::Unsupported("too much data for a resource".into()));
            }
            shift += 1;
            continue;
        }

        // Largest pages come first in the segment.
        let mut page_start = [0usize; 5];
        let mut size = 0;
        for i in (0..5).rev() {
            page_start[i] = size;
            size += (base_size << i) * counts[i] as usize;
        }
        let offsets = placed.iter().map(|&(i, p, o)| page_start[i] + (base_size << i) * p + o).collect();
        let flags = shift | (counts[4] << 4) | (counts[3] << 5) | (counts[2] << 7) | (counts[1] << 11) | (counts[0] << 17);
        return Ok(Layout { offsets, flags, size, pages: total });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsc::page_size;

    #[test]
    fn blocks_stay_inside_pages() {
        let sizes = [64, 0x9000, 300, 0x2000, 16, 0x41000, 7];
        let l = layout(&sizes, true, MAX_PAGES).unwrap();
        assert_eq!(l.offsets[0], 0);
        assert_eq!(page_size(l.flags), l.size);
        // No overlaps, everything inside the segment.
        let mut spans: Vec<(usize, usize)> = l.offsets.iter().zip(&sizes).map(|(&o, &s)| (o, o + s)).collect();
        spans.sort();
        assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0));
        assert!(spans.last().unwrap().1 <= l.size);
    }
}
