//! Read-only hex dump of files that have no other preview.

use eframe::egui::{self, text::LayoutJob};

/// How much of a file is kept and shown; the rest is left out, and the summary says so.
pub const MAX_HEX: usize = 8 << 20;
/// Bytes in a row: 16, or 8 when the panel is too narrow for them.
const PER_ROW: usize = 16;

pub struct HexView {
    data: Vec<u8>,
    /// The size of the whole file, when only its start is shown.
    total: Option<u64>,
}

impl HexView {
    /// `total` is the file's size when `data` is only its start; longer data is cut to [`MAX_HEX`].
    pub fn new(mut data: Vec<u8>, total: Option<u64>) -> Self {
        let total = total.or_else(|| (data.len() > MAX_HEX).then_some(data.len() as u64));
        data.truncate(MAX_HEX);
        data.shrink_to_fit();
        HexView { data, total }
    }

    pub fn summary(&self) -> String {
        match self.total {
            Some(total) => format!("shown as hex: the first {} of {}", crate::app::size(self.data.len() as u64), crate::app::size(total)),
            None => "shown as hex".into(),
        }
    }

    pub fn ui(&self, ui: &mut egui::Ui) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let (weak, strong) = (ui.visuals().weak_text_color(), ui.visuals().text_color());
        // Offset, 3 characters per byte, the gaps, and the text.
        let char_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let per_row = if char_width * (10 + 4 * PER_ROW + 2) as f32 <= ui.available_width() { PER_ROW } else { PER_ROW / 2 };
        let rows = self.data.len().div_ceil(per_row);
        // Only the rows in sight are laid out, so a large file scrolls as fast as a small one.
        egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_height, rows, |ui, range| {
            for i in range {
                let offset = i * per_row;
                let (at, bytes) = row(offset, &self.data[offset..(offset + per_row).min(self.data.len())], per_row);
                let mut job = LayoutJob::default();
                job.append(&at, 0.0, egui::TextFormat::simple(font.clone(), weak));
                job.append(&bytes, 0.0, egui::TextFormat::simple(font.clone(), strong));
                ui.add(egui::Label::new(job).extend());
            }
        });
    }
}

/// One row of the dump: the offset, then the bytes in hex and as ASCII ('.' for the rest).
/// A row of 16 has a gap after the 8th byte.
fn row(offset: usize, bytes: &[u8], per_row: usize) -> (String, String) {
    let mut hex = String::with_capacity(3 * per_row + 2);
    for i in 0..per_row {
        if i == 8 {
            hex.push(' ');
        }
        match bytes.get(i) {
            Some(b) => hex.push_str(&format!("{b:02X} ")),
            None => hex.push_str("   "),
        }
    }
    let ascii: String = bytes.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' }).collect();
    (format!("{offset:08X}  "), format!("{hex} {ascii}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_pad_the_hex_and_dot_the_unprintable() {
        let (at, bytes) = row(0x10, b"Hi\0\xFF", 16);
        assert_eq!(at, "00000010  ");
        assert_eq!(bytes, format!("48 69 00 FF {} Hi..", " ".repeat(3 * 12 + 1)));
        let (_, full) = row(0, b"0123456789abcdef", 16);
        assert_eq!(full, "30 31 32 33 34 35 36 37  38 39 61 62 63 64 65 66  0123456789abcdef");
        let (_, narrow) = row(8, b"89abcdef", 8);
        assert_eq!(narrow, "38 39 61 62 63 64 65 66  89abcdef");
    }

    #[test]
    fn long_data_is_cut_and_says_so() {
        let view = HexView::new(vec![0; MAX_HEX + (1 << 20)], None);
        assert_eq!(view.data.len(), MAX_HEX);
        assert_eq!(view.summary(), "shown as hex: the first 8.0 MB of 9.0 MB");
        let view = HexView::new(vec![0; 32], Some(3 << 30));
        assert_eq!(view.summary(), "shown as hex: the first 32 B of 3.0 GB");
        assert_eq!(HexView::new(vec![1, 2], None).summary(), "shown as hex");
    }
}
