//! The "Compare with original" window: a file of the mods folder against the game's own, read-only. Text and XML line by
//! line (unified or side by side), anything else byte by byte with a two-buffer hex view.

use std::ops::Range;
use std::path::Path;

use eframe::egui::{self, Color32, RichText, text::LayoutJob};
use paleto_core::ArchivePath;
use paleto_core::diff::{self, ByteDiff, Compared, Comparison, Line, Op, Outcome};

use crate::actions::Action;
use crate::app::{App, size};

/// Unchanged lines kept around each change.
const CONTEXT: usize = 3;
/// Rows of the hex view, and how many differing stretches the list shows without scrolling.
const HEX_ROWS: usize = 16;
const RUNS_HEIGHT: f32 = 110.0;
/// The space between the two halves of a side by side row.
const SEPARATOR: f32 = 8.0;

/// The most characters of a line shown.
const MAX_SHOWN: usize = 2000;

/// What the comparison task brings back.
pub(crate) struct Done {
    pub(crate) original: Option<ArchivePath>,
    pub(crate) modified: ArchivePath,
    pub(crate) compared: Compared,
}

pub(crate) enum Request {
    Close,
}

pub(crate) struct CompareWindow {
    name: String,
    modified: String,
    original: Option<String>,
    kind: &'static str,
    notes: Vec<String>,
    body: Body,
}

enum Body {
    Message(String),
    Lines(LineView),
    Bytes(ByteView),
    Large { sizes: [u64; 2], same_stored: Option<bool> },
}

impl CompareWindow {
    /// `root` is the game folder, left out of the paths shown.
    pub(crate) fn new(root: Option<&Path>, done: Done) -> Self {
        let Done { original, modified, compared } = done;
        let Compared { comparison: Comparison { kind, outcome, notes }, original: before, modified: after } = compared;
        // The path of an original that is not there (the mods folder adds the file) says nothing.
        let original = original.filter(|_| !matches!(outcome, Outcome::Added));
        let body = match outcome {
            Outcome::Identical => Body::Message("The mods copy is the same as the original".into()),
            Outcome::Added => Body::Message("The game has no original of this file: the mods folder adds it".into()),
            Outcome::Deleted => Body::Message("The original has this file; the mods copy does not".into()),
            Outcome::Lines { same_content: true, .. } => Body::Message("Same content: only the formatting differs".into()),
            Outcome::Lines { lines, .. } => Body::Lines(LineView::new(lines)),
            Outcome::Bytes(diff) => Body::Bytes(ByteView::new(diff, before.unwrap_or_default(), after.unwrap_or_default())),
            Outcome::Large { sizes, same_stored } => Body::Large { sizes, same_stored },
        };
        let name = modified.inner.last().cloned().unwrap_or_else(|| modified.disk.file_name().unwrap_or_default().to_string_lossy().into_owned());
        CompareWindow { name, modified: shown(root, &modified), original: original.as_ref().map(|p| shown(root, p)), kind: kind.label(), notes, body }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<Request> {
        let screen = ctx.content_rect();
        let mut open = true;
        egui::Window::new("Compare with original")
            .id(egui::Id::unique("compare window"))
            .open(&mut open)
            .resizable(true)
            .default_size([(screen.width() - 80.0).min(980.0), (screen.height() - 120.0).min(680.0)])
            .show(ctx, |ui| self.contents(ui));
        (!open).then_some(Request::Close)
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(&self.name).strong().size(egui::TextStyle::Heading.resolve(ui.style()).size));
        ui.add(egui::Label::new(format!("Mods copy: {}", self.modified)).selectable(true));
        ui.add(egui::Label::new(match &self.original {
            Some(path) => format!("Original: {path}"),
            None => "Original: none".to_string(),
        }).selectable(true));
        ui.label(format!("Kind: {}", self.kind));
        for note in &self.notes {
            crate::widgets::hint(ui, note);
        }
        ui.separator();
        match &mut self.body {
            Body::Message(text) => {
                ui.label(text.as_str());
            }
            Body::Lines(view) => view.ui(ui),
            Body::Bytes(view) => view.ui(ui),
            Body::Large { sizes, same_stored } => {
                ui.label(format!("Original: {}", byte_size(sizes[0])));
                ui.label(format!("Mods copy: {}", byte_size(sizes[1])));
                ui.label(stored_text(*same_stored));
            }
        }
    }

    /// What the window shows, in words (for the tests).
    pub(crate) fn summary(&self) -> String {
        let mut out = vec![self.name.clone(), format!("Mods copy: {}", self.modified), match &self.original {
            Some(path) => format!("Original: {path}"),
            None => "Original: none".to_string(),
        }, format!("Kind: {}", self.kind)];
        out.extend(self.notes.iter().cloned());
        match &self.body {
            Body::Message(text) => out.push(text.clone()),
            Body::Lines(view) => out.push(view.summary()),
            Body::Bytes(view) => out.push(view.summary()),
            Body::Large { sizes, same_stored } => {
                out.push(format!("Original: {}", byte_size(sizes[0])));
                out.push(format!("Mods copy: {}", byte_size(sizes[1])));
                out.push(stored_text(*same_stored).into());
            }
        }
        out.join("\n")
    }
}

impl App {
    /// Compares the previewed file of the mods folder with the game's original, as the preview's button does.
    #[doc(hidden)]
    pub fn compare_with_original(&mut self, ctx: &egui::Context) {
        self.apply(Action::CompareWithOriginal, ctx);
    }

    /// What the "Compare with original" window shows, in words, while it is open.
    #[doc(hidden)]
    pub fn compare_summary(&self) -> Option<String> {
        self.compare.as_ref().map(CompareWindow::summary)
    }
}

/// The path as the window shows it: below the game folder when it is there, then the path inside the archive.
fn shown(root: Option<&Path>, path: &ArchivePath) -> String {
    let disk = root.and_then(|root| path.disk.strip_prefix(root).ok()).unwrap_or(&path.disk);
    match path.inner.is_empty() {
        true => disk.display().to_string(),
        false => format!("{}/{}", disk.display(), path.inner_str()),
    }
}

fn byte_size(n: u64) -> String {
    if n < 1024 { format!("{n} bytes") } else { format!("{} ({n} bytes)", size(n)) }
}

fn stored_text(same: Option<bool>) -> &'static str {
    match same {
        Some(true) => "The stored bytes are the same",
        Some(false) => "The stored bytes differ",
        None => "The stored bytes were not compared",
    }
}

/// A line comparison turned into the rows to draw, once, when it arrives: only the changes with their context, and
/// between them how many lines are left out.
struct LineView {
    lines: Vec<Line>,
    unified: Vec<UnifiedRow>,
    split: Vec<SplitRow>,
    side_by_side: bool,
    removed: usize,
    added: usize,
    /// Digits of the largest line number, and characters of the longest line: the width of the rows.
    digits: usize,
    columns: usize,
}

/// A row of the unified view: a line (by its index) or a gap of unchanged lines.
enum UnifiedRow {
    Line(usize),
    Gap(usize),
}

/// A row of the side by side view: the original's line and the mods copy's (by index), or a gap.
enum SplitRow {
    Pair(Option<usize>, Option<usize>),
    Gap(usize),
}

impl LineView {
    fn new(mut lines: Vec<Line>) -> Self {
        // A minified file is one huge line: past this, a line is cut where it is shown (the comparison saw all of it).
        for line in lines.iter_mut().filter(|l| l.text.chars().count() > MAX_SHOWN) {
            line.text = line.text.chars().take(MAX_SHOWN).chain(['…']).collect();
        }
        let hunks = diff::hunks(&lines, CONTEXT);
        let (mut unified, mut split) = (Vec::new(), Vec::new());
        let mut at = 0;
        for hunk in hunks.iter().cloned() {
            if hunk.start > at {
                unified.push(UnifiedRow::Gap(hunk.start - at));
                split.push(SplitRow::Gap(hunk.start - at));
            }
            at = hunk.end;
            unified.extend(hunk.clone().map(UnifiedRow::Line));
            pair(&lines, hunk, &mut split);
        }
        if !hunks.is_empty() && at < lines.len() {
            unified.push(UnifiedRow::Gap(lines.len() - at));
            split.push(SplitRow::Gap(lines.len() - at));
        }
        let count = |op| lines.iter().filter(|l| l.op == op).count();
        let (removed, added) = (count(Op::Removed), count(Op::Added));
        let digits = lines.iter().flat_map(|l| [l.old, l.new]).flatten().max().unwrap_or(0).to_string().len();
        let columns = lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
        LineView { lines, unified, split, side_by_side: false, removed, added, digits, columns }
    }

    fn summary(&self) -> String {
        let mut out = vec![format!("{} removed, {} added ({})", self.removed, self.added, if self.side_by_side { "side by side" } else { "unified" })];
        out.extend(self.lines.iter().filter(|l| l.op != Op::Same).map(|l| format!("{} {}", mark(l.op), l.text)));
        out.join("\n")
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.side_by_side, false, "Unified");
            ui.selectable_value(&mut self.side_by_side, true, "Side by side");
            crate::widgets::hint(ui, format!("{} removed, {} added", self.removed, self.added));
        });
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let char_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let dark = ui.visuals().dark_mode;
        let (error, ok) = (crate::theme::error(dark), crate::theme::ok(dark));
        let look = Look {
            height: ui.text_style_height(&egui::TextStyle::Monospace),
            font,
            weak: ui.visuals().weak_text_color(),
            strong: ui.visuals().text_color(),
            faint: ui.visuals().faint_bg_color,
            error,
            ok,
            removed: error.gamma_multiply(0.18),
            added: ok.gamma_multiply(0.18),
        };
        let viewport = ui.available_width() - ui.spacing().scroll.allocated_width();
        // Two numbers, a mark and a space, then the text.
        let unified_width = (char_width * (2 * self.digits + 4 + self.columns) as f32 + 8.0).max(viewport);
        let half = (char_width * (self.digits + 1 + self.columns) as f32 + 8.0).max((viewport - SEPARATOR) / 2.0);
        let rows = if self.side_by_side { self.split.len() } else { self.unified.len() };
        ui.scope(|ui| {
            // The rows touch, so a tinted block has no stripes.
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::both().id_salt(("compare lines", self.side_by_side)).auto_shrink(false).show_rows(ui, look.height, rows, |ui, range| {
                for row in range {
                    if self.side_by_side {
                        self.split_row(ui, &look, &self.split[row], half);
                    } else {
                        self.unified_row(ui, &look, &self.unified[row], unified_width);
                    }
                }
            });
        });
    }

    fn unified_row(&self, ui: &mut egui::Ui, look: &Look, row: &UnifiedRow, width: f32) {
        let line = match *row {
            UnifiedRow::Gap(n) => return gap_row(ui, look, n, width),
            UnifiedRow::Line(i) => &self.lines[i],
        };
        let number = |n: Option<usize>| n.map_or_else(|| " ".repeat(self.digits), |n| format!("{n:>width$}", width = self.digits));
        let (tint, color) = match line.op {
            Op::Same => (None, look.strong),
            Op::Removed => (Some(look.removed), look.error),
            Op::Added => (Some(look.added), look.ok),
        };
        let mut job = LayoutJob::default();
        job.append(&format!("{} {} ", number(line.old), number(line.new)), 0.0, egui::TextFormat::simple(look.font.clone(), look.weak));
        job.append(&format!("{} ", mark(line.op)), 0.0, egui::TextFormat::simple(look.font.clone(), color));
        job.append(&line.text, 0.0, egui::TextFormat::simple(look.font.clone(), look.strong));
        paint_row(ui, look, width, tint, job, format!("{} {}", mark(line.op), line.text));
    }

    fn split_row(&self, ui: &mut egui::Ui, look: &Look, row: &SplitRow, half: f32) {
        let width = 2.0 * half + SEPARATOR;
        let (left, right) = match *row {
            SplitRow::Gap(n) => return gap_row(ui, look, n, width),
            SplitRow::Pair(left, right) => (left, right),
        };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, look.height), egui::Sense::hover());
        let cells = [
            (egui::Rect::from_min_size(rect.min, egui::vec2(half, look.height)), left, look.removed, false),
            (egui::Rect::from_min_size(rect.min + egui::vec2(half + SEPARATOR, 0.0), egui::vec2(half, look.height)), right, look.added, true),
        ];
        let mut label = Vec::new();
        for (cell, index, tint, modified) in cells {
            let painter = ui.painter().with_clip_rect(cell);
            let Some(line) = index.map(|i| &self.lines[i]) else {
                painter.rect_filled(cell, egui::CornerRadius::ZERO, look.faint);
                label.push(String::new());
                continue;
            };
            if line.op != Op::Same {
                painter.rect_filled(cell, egui::CornerRadius::ZERO, tint);
            }
            let number = if modified { line.new } else { line.old };
            let mut job = LayoutJob::default();
            job.append(&format!("{:>width$} ", number.map_or_else(String::new, |n| n.to_string()), width = self.digits), 0.0, egui::TextFormat::simple(look.font.clone(), look.weak));
            job.append(&line.text, 0.0, egui::TextFormat::simple(look.font.clone(), look.strong));
            let galley = painter.layout_job(job);
            painter.galley(cell.min + egui::vec2(2.0, 0.0), galley, look.strong);
            label.push(line.text.clone());
        }
        let label = format!("{} | {}", label[0], label[1]);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    }
}

/// The colours and the font the rows are drawn with.
struct Look {
    font: egui::FontId,
    height: f32,
    weak: Color32,
    strong: Color32,
    faint: Color32,
    error: Color32,
    ok: Color32,
    removed: Color32,
    added: Color32,
}

/// A row of `width`, with its text drawn at the left (it is not a label of its own: a screen reader and the tests get `label`).
fn paint_row(ui: &mut egui::Ui, look: &Look, width: f32, tint: Option<Color32>, job: LayoutJob, label: String) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, look.height), egui::Sense::hover());
    if let Some(tint) = tint {
        ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, tint);
    }
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(rect.min, galley, look.strong);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
}

fn gap_row(ui: &mut egui::Ui, look: &Look, n: usize, width: f32) {
    let text = if n == 1 { "… 1 unchanged line".to_string() } else { format!("… {n} unchanged lines") };
    let mut job = LayoutJob::default();
    job.append(&text, 0.0, egui::TextFormat::simple(look.font.clone(), look.weak));
    paint_row(ui, look, width, Some(look.faint), job, text);
}

fn mark(op: Op) -> char {
    match op {
        Op::Same => ' ',
        Op::Removed => '-',
        Op::Added => '+',
    }
}

/// The rows of a hunk side by side: unchanged lines on both sides, and a changed block's removed and added lines paired
/// row by row.
fn pair(lines: &[Line], hunk: Range<usize>, out: &mut Vec<SplitRow>) {
    fn flush(removed: &mut Vec<usize>, added: &mut Vec<usize>, out: &mut Vec<SplitRow>) {
        for k in 0..removed.len().max(added.len()) {
            out.push(SplitRow::Pair(removed.get(k).copied(), added.get(k).copied()));
        }
        removed.clear();
        added.clear();
    }
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for i in hunk {
        match lines[i].op {
            Op::Same => {
                flush(&mut removed, &mut added, out);
                out.push(SplitRow::Pair(Some(i), Some(i)));
            }
            Op::Removed => removed.push(i),
            Op::Added => added.push(i),
        }
    }
    flush(&mut removed, &mut added, out);
}

/// Two files compared byte by byte: their sizes, SHA-256, the stretches that differ, and a hex view of both at the
/// selected one.
struct ByteView {
    diff: ByteDiff,
    original: Vec<u8>,
    modified: Vec<u8>,
    selected: usize,
}

impl ByteView {
    fn new(diff: ByteDiff, original: Vec<u8>, modified: Vec<u8>) -> Self {
        ByteView { diff, original, modified, selected: 0 }
    }

    /// Where the hex view starts: the selected stretch.
    fn offset(&self) -> usize {
        self.diff.runs.get(self.selected).map_or(0, |&(offset, _)| offset as usize)
    }

    fn summary(&self) -> String {
        let d = &self.diff;
        let mut out = vec![format!("Original: {}, SHA-256 {}", byte_size(d.sizes[0]), d.sha256[0]), format!("Mods copy: {}, SHA-256 {}", byte_size(d.sizes[1]), d.sha256[1])];
        out.extend(d.runs.iter().map(|&(offset, len)| format!("Differs at 0x{offset:08X}: {}", bytes_text(len))));
        if d.more {
            out.push("and more".into());
        }
        out.push(format!("Hex view from 0x{:08X}", self.offset() / 16 * 16));
        out.join("\n")
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().id_salt("compare bytes").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("compare sums").num_columns(4).show(ui, |ui| {
                for (side, i) in [("Original", 0), ("Mods copy", 1)] {
                    ui.label(side);
                    ui.label(byte_size(self.diff.sizes[i]));
                    ui.label(RichText::new("SHA-256").weak());
                    ui.add(egui::Label::new(RichText::new(&self.diff.sha256[i]).monospace()).selectable(true));
                    ui.end_row();
                }
            });
            let runs = self.diff.runs.len();
            crate::widgets::section(ui, "Differences");
            ui.horizontal(|ui| {
                if ui.add_enabled(self.selected > 0, egui::Button::new("⏶ Previous")).on_hover_text("Previous difference").clicked() {
                    self.selected -= 1;
                }
                if ui.add_enabled(self.selected + 1 < runs, egui::Button::new("⏷ Next")).on_hover_text("Next difference").clicked() {
                    self.selected += 1;
                }
                let more = if self.diff.more { " or more" } else { "" };
                crate::widgets::hint(ui, format!("Difference {} of {runs}{more}", self.selected + 1));
            });
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0 * ui.spacing().button_padding.y;
            egui::ScrollArea::vertical().id_salt("compare runs").max_height(RUNS_HEIGHT).show_rows(ui, row_height, runs, |ui, range| {
                for i in range {
                    let (offset, len) = self.diff.runs[i];
                    let text = RichText::new(format!("0x{offset:08X}  {}", bytes_text(len))).monospace();
                    if ui.selectable_label(i == self.selected, text).clicked() {
                        self.selected = i;
                    }
                }
            });
            if self.diff.more {
                crate::widgets::hint(ui, "…and more");
            }
            crate::widgets::section(ui, "Bytes");
            crate::widgets::hint(ui, "The original on the left, the mods copy on the right; the bytes that differ are tinted");
            crate::hex_view::compare(ui, &self.original, &self.modified, self.offset(), HEX_ROWS);
        });
    }
}

fn bytes_text(len: u64) -> String {
    if len == 1 { "1 byte".to_string() } else { format!("{len} bytes") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(op: Op, text: &str, old: Option<usize>, new: Option<usize>) -> Line {
        Line { op, text: text.into(), old, new }
    }

    #[test]
    fn rows_keep_the_changes_with_their_context_and_say_what_is_left_out() {
        let mut lines: Vec<Line> = (1..=20).map(|n| line(Op::Same, &format!("l{n}"), Some(n), Some(n))).collect();
        lines[9] = line(Op::Removed, "old", Some(10), None);
        lines.insert(10, line(Op::Added, "new", None, Some(10)));
        let view = LineView::new(lines);
        // A gap, 3 lines of context, the 2 changed lines, 3 of context and a gap.
        assert_eq!((view.removed, view.added), (1, 1));
        assert_eq!(view.unified.len(), 1 + 3 + 2 + 3 + 1);
        assert!(matches!(view.unified[0], UnifiedRow::Gap(6)));
        assert!(matches!(view.unified.last(), Some(UnifiedRow::Gap(7))));
        // The removed and the added line share a row.
        assert!(view.split.iter().any(|r| matches!(r, SplitRow::Pair(Some(9), Some(10)))));
        assert_eq!(view.split.len(), view.unified.len() - 1);
    }
}
