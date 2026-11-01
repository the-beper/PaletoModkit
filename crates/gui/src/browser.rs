//! The file browser: the rows table, the side tree of game folders, and the background
//! listings that fill them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use paleto_core::{ArchivePath, Game, game, vfs};
use paleto_rpf::{Encryption, EntryKind, Keys};

use crate::actions::Action;
use crate::app::{App, Changes, Location, Status, size, LOADING_SHOWN, MODS_LINK, MODS_LINK_HOVER};
use crate::preview::{PreviewSource, extension};
use crate::settings::{FileClass, Grouping, Settings, SortBy};
use crate::task::Task;
use crate::theme;

pub(crate) struct FsEntry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    /// An original archive with a modified copy in the mods folder.
    pub(crate) modded: bool,
}

/// What to do once a location's listing is shown.
#[derive(Default)]
pub(crate) struct AfterListing {
    /// The row to select.
    pub(crate) select: Option<String>,
    /// Read the preview again (it was edited), selecting this texture or sound.
    pub(crate) reload_preview: Option<String>,
}

/// How the history moved for a location asked for: undone if it is never shown.
#[derive(Clone, Copy)]
pub(crate) enum HistoryMove {
    Go,
    Back,
    Forward,
}

/// A location being listed in the background.
pub(crate) struct PendingListing {
    /// As asked for: the original archive when the mods copy is what is shown.
    pub(crate) requested: Location,
    location: Location,
    from_mods: bool,
    redirected: bool,
    after: AfterListing,
    history: Option<HistoryMove>,
}

pub(crate) type Listed = Result<(Option<Encryption>, Vec<Row>), String>;
/// Side tree folders read, with their entries.
pub(crate) type TreeRead = Vec<(PathBuf, Vec<FsEntry>)>;

pub(crate) struct Browser {
    pub(crate) location: Location,
    pub(crate) from_mods: bool,
    /// Opened as the original archive and shown as its mods copy: the side
    /// tree and the history keep the original.
    via_original: bool,
    pub(crate) encryption: Option<Encryption>,
    pub(crate) rows: Vec<Row>,
    pub(crate) selected: BTreeSet<String>,
    anchor: Option<usize>,
    pub(crate) filter: String,
    focus_filter: bool,
    /// Drop the filter's keyboard focus (after moving to another folder).
    blur_filter: bool,
    /// The last row clicked and when, to detect double clicks ourselves:
    /// egui counts a quick second double click as a triple click.
    last_click: Option<(String, f64)>,
    scroll_to: Option<usize>,
}

pub(crate) struct Row {
    pub(crate) name: String,
    pub(crate) kind: RowKind,
    pub(crate) size: u64,
    /// A mods copy of an archive on disk, or (inside a mods copy) differs
    /// from the original game file.
    pub(crate) modded: bool,
    /// Inside a mods copy: not in the original game files.
    pub(crate) added: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RowKind {
    Folder,
    Archive,
    File,
    Resource(u32),
}

impl Row {
    fn class(&self) -> FileClass {
        match self.kind {
            RowKind::Folder => FileClass::Folder,
            RowKind::Archive => FileClass::Archive,
            _ => FileClass::of(&self.name),
        }
    }

    pub(crate) fn is_container(&self) -> bool {
        matches!(self.kind, RowKind::Folder | RowKind::Archive)
    }
}

impl App {
    /// The folder or archive file highlighted in the side tree.
    pub fn tree_selection(&self) -> Option<PathBuf> {
        self.browser.as_ref().map(|b| match self.opened(b) {
            Location::Folder(p) => p,
            Location::Archive(a) => a.disk,
        })
    }

    /// Shows a location without touching the history. Archives show their
    /// mods copy when there is one.
    pub(crate) fn show(&mut self, location: Location) {
        self.show_then(location, AfterListing::default());
    }

    /// Lists a location in the background; it is shown, and `after` done, once listed.
    /// A newer request drops this one.
    pub(crate) fn show_then(&mut self, location: Location, after: AfterListing) {
        self.show_moving(location, after, None);
    }

    pub(crate) fn show_moving(&mut self, location: Location, after: AfterListing, history: Option<HistoryMove>) {
        if self.peek_original.as_ref().is_some_and(|d| !matches!(&location, Location::Archive(a) if &a.disk == d)) {
            self.peek_original = None;
        }
        let peeking = self.peek_original.is_some();
        let requested = location.clone();
        let mut redirected = false;
        let (location, from_mods) = match location {
            Location::Folder(path) => {
                let in_mods = self.game.as_ref().is_some_and(|g| path.starts_with(g.mods_dir()));
                (Location::Folder(path), in_mods)
            }
            Location::Archive(path) => {
                let (path, from_mods) = match &self.game {
                    Some(game) if !self.show_originals && !peeking => match path.in_mods(game) {
                        Some(m) => {
                            redirected = true;
                            (m, true)
                        }
                        None => {
                            let in_mods = path.disk.starts_with(game.mods_dir());
                            (path, in_mods)
                        }
                    },
                    Some(game) => {
                        let in_mods = path.disk.starts_with(game.mods_dir());
                        (path, in_mods)
                    }
                    None => (path, false),
                };
                (Location::Archive(path), from_mods)
            }
        };
        let name = match &requested {
            Location::Folder(p) => p.file_name(),
            Location::Archive(a) => a.inner.last().map(std::ffi::OsStr::new).or_else(|| a.disk.file_name()),
        };
        let label = format!("Reading {}", name.unwrap_or_default().to_string_lossy());
        let (target, game, originals, keys, delay) = (location.clone(), self.game.clone(), self.show_originals, self.keys.clone(), self.load_delay);
        let task = Task::spawn_cancellable(label, &self.ctx, move |_| {
            std::thread::sleep(delay);
            match &target {
                Location::Folder(dir) => folder_rows(dir, game.as_ref(), originals).map(|rows| (None, rows)),
                Location::Archive(path) => archive_rows(path, keys.as_deref()).map(|(enc, rows)| (Some(enc), rows)),
            }
        });
        self.listing_task = Some((PendingListing { requested, location, from_mods, redirected, after, history }, task));
    }

    /// Puts the history back as it was before asking for a place never shown.
    pub(crate) fn undo_history(&mut self, pending: &PendingListing) {
        match pending.history {
            Some(HistoryMove::Go) => {
                self.back.pop();
            }
            Some(HistoryMove::Back) => {
                self.forward.pop();
                self.back.push(pending.requested.clone());
            }
            Some(HistoryMove::Forward) => {
                self.back.pop();
                self.forward.push(pending.requested.clone());
            }
            None => {}
        }
    }

    /// Shows a location once listed.
    pub(crate) fn show_listed(&mut self, pending: PendingListing, listed: Listed, ctx: &egui::Context) {
        let (encryption, mut rows) = match listed {
            Ok(listed) => listed,
            Err(e) => {
                self.status = Status::Error(e);
                self.undo_history(&pending);
                return;
            }
        };
        // A place the user reached, counted in "Recent locations" (in-game only).
        self.record_recent(&pending.requested);
        let PendingListing { location, from_mods, redirected, after, .. } = pending;
        sort_rows(&mut rows, &self.settings);
        // In a mods copy, mark what differs from the original game files.
        if from_mods && let Location::Archive(path) = &location {
            let modified = std::fs::metadata(&path.disk).and_then(|m| m.modified()).ok();
            match self.changes_cache.iter().find(|(l, t, _)| l == &location && *t == modified) {
                Some((_, _, changes)) => mark_changes(&mut rows, changes),
                None => {
                    let (modded, original, keys) = (path.clone(), self.original_archive(path), self.keys.clone());
                    let task = Task::spawn("Comparing with the original", &self.ctx, move || {
                        vfs::compare_folder(&modded, &original, keys.as_deref()).map_err(|e| e.to_string())
                    });
                    self.changes_task = Some((location.clone(), self.keys.is_some(), task));
                }
            }
        }

        // Keep the preview while it belongs to this folder (or its mods copy).
        let here = Some(self.original_of(&location));
        let same = self.preview.as_ref().map(|p| match &p.source {
            PreviewSource::Archive(a) => a.parent().map(|d| self.original_of(&Location::Archive(d))) == here,
            PreviewSource::Disk(d) => d.parent().map(|d| Location::Folder(d.to_path_buf())) == here,
        });
        // A file of another folder (or the other copy of this one) still loading is not wanted any more.
        if self.browser.as_ref().is_some_and(|b| b.location != location) {
            self.preview_task = None;
        }
        match (same, &mut self.preview, &location) {
            (Some(false), _, _) => self.preview = None,
            (Some(true), Some(p), Location::Archive(a)) => p.source = PreviewSource::Archive(a.join(&p.name)),
            _ => {}
        }

        let previous = self.browser.take().filter(|b| b.location == location);
        let moved = previous.is_none();
        // A delete or rename asked about names of the place shown before.
        if moved {
            self.confirm_delete = None;
            self.name_dialog = None;
        }
        let (filter, selected) = previous.map(|b| (b.filter, b.selected)).unwrap_or_default();
        let selected = selected.into_iter().filter(|s| rows.iter().any(|r| &r.name == s)).collect();
        self.browser = Some(Browser {
            location,
            from_mods,
            via_original: redirected,
            encryption,
            rows,
            selected,
            anchor: None,
            filter,
            focus_filter: false,
            blur_filter: moved,
            last_click: None,
            scroll_to: None,
        });
        if let (Some(name), Some(b)) = (after.select, &mut self.browser)
            && let Some(pos) = b.rows.iter().position(|r| r.name == name)
        {
            b.selected = BTreeSet::from([name]);
            b.anchor = Some(pos);
            b.scroll_to = Some(pos);
        }
        if let (Some(selected), Some(name)) = (after.reload_preview, self.previewing().map(str::to_string)) {
            self.preview_select = Some(selected);
            self.apply(Action::Preview(name), ctx);
            // Not left for another file when the reload did not start.
            self.preview_select = None;
        }
    }

    /// The same location in the game folder when it points into the mods folder.
    pub(crate) fn original_of(&self, location: &Location) -> Location {
        let Some(game) = &self.game else { return location.clone() };
        match location {
            // Packs only in the mods folder (add-ons) have no original.
            Location::Archive(a) => match a.disk.strip_prefix(game.mods_dir()) {
                Ok(rel) if game.root.join(rel).is_file() => Location::Archive(ArchivePath { disk: game.root.join(rel), inner: a.inner.clone() }),
                _ => location.clone(),
            },
            Location::Folder(_) => location.clone(),
        }
    }

    /// The location as the user opened it: the original archive when the
    /// browser shows its mods copy in its place.
    pub(crate) fn opened(&self, b: &Browser) -> Location {
        if b.via_original { self.original_of(&b.location) } else { b.location.clone() }
    }

    /// A place inside the archive shown, as the user would reach it: inside
    /// the original when that is what was opened.
    pub(crate) fn within(&self, b: &Browser, target: Location) -> Location {
        match (&b.location, &target) {
            (Location::Archive(here), Location::Archive(there)) if b.via_original && here.disk == there.disk => self.original_of(&target),
            _ => target,
        }
    }

    pub(crate) fn original_archive(&self, path: &ArchivePath) -> ArchivePath {
        match self.original_of(&Location::Archive(path.clone())) {
            Location::Archive(a) => a,
            Location::Folder(_) => unreachable!(),
        }
    }

    /// A folder of the side tree; empty while it is read in the background.
    pub(crate) fn listing(&mut self, dir: &Path) -> &[FsEntry] {
        if !self.tree.contains_key(dir) {
            if self.tree_loading.insert(dir.to_path_buf()) {
                self.tree_queue.push(dir.to_path_buf());
            }
            return &[];
        }
        &self.tree[dir]
    }

    /// Reads the side tree folders asked for in this frame, all in one task.
    pub(crate) fn read_tree(&mut self) {
        if self.tree_task.is_some() || self.tree_queue.is_empty() {
            return;
        }
        let dirs = std::mem::take(&mut self.tree_queue);
        let (game, originals, all_files, delay) = (self.game.clone(), self.show_originals, self.settings.tree_all_files, self.load_delay);
        self.tree_task = Some(Task::spawn_cancellable("Reading folders", &self.ctx, move |cancel| {
            std::thread::sleep(delay);
            dirs.into_iter().take_while(|_| !cancel.is_set()).map(|dir| {
                let entries = tree_entries(&dir, game.as_ref(), originals, all_files);
                (dir, entries)
            }).collect()
        }));
    }

    /// Forgets the side tree, to read it again; folders still being read are dropped.
    pub(crate) fn clear_tree(&mut self) {
        self.tree.clear();
        self.tree_loading.clear();
        self.tree_queue.clear();
        self.tree_task = None;
    }

    pub(crate) fn browser_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy();
        // A listing that takes long enough to notice: the old rows stay, dimmed and inert.
        if let Some((_, task)) = &self.listing_task {
            if task.elapsed() >= LOADING_SHOWN {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.add(egui::Label::new(RichText::new(format!("{}…", task.label)).weak()).truncate());
                });
                ui.disable();
            } else {
                ui.ctx().request_repaint_after(LOADING_SHOWN.saturating_sub(task.elapsed()));
            }
        }
        let Some(b) = &self.browser else {
            if self.listing_task.is_none() {
                ui.centered_and_justified(|ui| ui.label("Select a folder or an archive on the left."));
            }
            return;
        };
        let crumbs = self.breadcrumbs(b);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + self.settings.density.row_padding();
        let settings = self.settings.clone();
        let colored = settings.colored_icons;
        // Keys and tools act on the rows shown: not while another place is being read.
        let modal_open = self.dialog_open() || self.listing_task.is_some();
        let busy = busy || self.listing_task.is_some();
        let b = self.browser.as_mut().unwrap();
        let in_archive = matches!(b.location, Location::Archive(_));

        // Toolbar.
        let selected: Vec<String> = b.selected.iter().cloned().collect();
        let filter_id = egui::Id::unique(FILTER_ID);
        // Icons only when the full labels do not fit; the names show on hover.
        const TOOLS: [&str; 6] = ["➕ Add files…", "📁 Add folder…", "🗀 New folder…", "✏ Rename…", "⬇ Extract…", "🗑 Delete"];
        let spacing = ui.spacing().item_spacing.x + 2.0 * ui.spacing().button_padding.x;
        let needed: f32 = TOOLS
            .iter()
            .map(|t| egui::WidgetText::from(*t).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + spacing)
            .sum();
        let compact = needed > ui.available_width();
        // Wraps onto a second row when even the icons do not fit, so none is cut off.
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let height = crate::widgets::bar_height(ui);
                let button = |icon: &str, name: &str| {
                    egui::Button::new(if compact { icon.to_string() } else { format!("{icon} {name}") }).min_size(egui::vec2(if compact { height } else { 0.0 }, height))
                };
                let only_archives = "Open an .rpf archive to add files to it";
                if ui.add_enabled(in_archive, button("➕", "Add files…")).on_hover_text("Add files").on_disabled_hover_text(only_archives).clicked() {
                    actions.push(Action::PickAdd { folders: false });
                }
                if ui.add_enabled(in_archive, button("📁", "Add folder…")).on_hover_text("Add a folder").on_disabled_hover_text(only_archives).clicked() {
                    actions.push(Action::PickAdd { folders: true });
                }
                if ui.add(button("🗀", "New folder…")).on_hover_text("New folder (Ctrl+Shift+N)").clicked() {
                    actions.push(Action::AskNewFolder);
                }
                if ui.add_enabled(selected.len() == 1, button("✏", "Rename…")).on_hover_text("Rename (F2)").clicked() {
                    actions.push(Action::AskRename(selected[0].clone()));
                }
                if ui
                    .add_enabled(in_archive && !selected.is_empty(), button("⬇", "Extract…"))
                    .on_hover_text("Extract the selection")
                    .on_disabled_hover_text("Select files inside an .rpf archive to extract them")
                    .clicked()
                {
                    actions.push(Action::Extract(selected.clone(), None));
                }
                if ui.add_enabled(!selected.is_empty(), button("🗑", "Delete")).on_hover_text("Delete the selection (Delete)").clicked() {
                    actions.push(Action::AskDelete(selected.clone()));
                }
            });
        });
        ui.separator();

        let filter = b.filter.to_ascii_lowercase();
        let visible: Vec<usize> = (0..b.rows.len()).filter(|&i| b.rows[i].name.to_ascii_lowercase().contains(&filter)).collect();

        // Keyboard: typing filters, arrows move, Enter opens.
        let filter_focused = ui.ctx().memory(|m| m.has_focus(filter_id));
        if !modal_open && (filter_focused || !ui.ctx().egui_wants_keyboard_input()) {
            let (typed, keys) = ui.input(|i| {
                let typed: String = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) if !i.modifiers.command => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                let k = |key| i.key_pressed(key);
                let alt = i.modifiers.alt;
                (
                    typed,
                    (k(egui::Key::ArrowDown), k(egui::Key::ArrowUp), k(egui::Key::Enter), k(egui::Key::Escape), k(egui::Key::Backspace) && !alt, k(egui::Key::Delete)),
                )
            });
            let (down, up, enter, escape, backspace, delete) = keys;
            if !filter_focused && !typed.is_empty() {
                b.filter.push_str(&typed);
                b.focus_filter = true;
                b.selected.clear();
            }
            let pos = b.anchor.filter(|&p| p < visible.len());
            if down || up {
                let next = match pos {
                    None => 0,
                    Some(p) if down => (p + 1).min(visible.len().saturating_sub(1)),
                    Some(p) => p.saturating_sub(1),
                };
                if let Some(&i) = visible.get(next) {
                    b.selected = BTreeSet::from([b.rows[i].name.clone()]);
                    b.anchor = Some(next);
                    b.scroll_to = Some(next);
                }
            }
            if enter {
                // The selection's row (among several, its anchor if selected, else the first); with none, the first match of the filter.
                let target = match b.selected.len() {
                    0 => visible.first().map(|&i| b.rows[i].name.clone()),
                    1 => b.selected.first().cloned(),
                    _ => pos
                        .and_then(|p| visible.get(p))
                        .map(|&i| b.rows[i].name.clone())
                        .filter(|name| b.selected.contains(name))
                        .or_else(|| visible.iter().map(|&i| &b.rows[i].name).find(|name| b.selected.contains(*name)).cloned()),
                };
                if let Some(name) = target {
                    actions.push(Action::Enter(name));
                }
            }
            if escape && !b.filter.is_empty() {
                b.filter.clear();
                ui.ctx().memory_mut(|m| m.surrender_focus(filter_id));
            }
            if !filter_focused && backspace && crumbs.len() > 1 {
                actions.push(Action::Go(crumbs[crumbs.len() - 2].1.clone()));
            }
            if !filter_focused && delete && !b.selected.is_empty() && !busy {
                actions.push(Action::AskDelete(b.selected.iter().cloned().collect()));
            }
            let (rename, new_folder) =
                ui.input(|i| (i.key_pressed(egui::Key::F2), i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::N)));
            if !busy && rename && b.selected.len() == 1 {
                actions.push(Action::AskRename(b.selected.first().unwrap().clone()));
            }
            if !busy && new_folder {
                actions.push(Action::AskNewFolder);
            }
        }
        let filter = b.filter.to_ascii_lowercase();
        let visible: Vec<usize> = (0..b.rows.len()).filter(|&i| b.rows[i].name.to_ascii_lowercase().contains(&filter)).collect();
        if !filter.is_empty() && b.selected.is_empty() && !visible.is_empty() {
            b.selected.insert(b.rows[visible[0]].name.clone());
            b.anchor = Some(0);
            b.scroll_to = Some(0);
        }

        let header = |ui: &mut egui::Ui, label: &str, column: SortBy, actions: &mut Vec<Action>| {
            let arrow = match (settings.sort_by == column, settings.descending) {
                (true, false) => " ⏶",
                (true, true) => " ⏷",
                _ => "",
            };
            let response = ui.add(egui::Label::new(RichText::new(format!("{label}{arrow}")).strong()).sense(egui::Sense::click()));
            if response.clicked() {
                let mut s = settings.clone();
                s.descending = s.sort_by == column && !s.descending;
                s.sort_by = column;
                actions.push(Action::Settings(s));
            }
        };
        // Type and size give up room first when the list is narrow; the name keeps the rest.
        let width = ui.available_width();
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(110.0).clip(true))
            .column(Column::exact((width * 0.3).clamp(70.0, 130.0)).clip(true))
            .column(Column::exact((width * 0.2).clamp(62.0, 100.0)).clip(true));
        if let Some(row) = b.scroll_to.take() {
            table = table.scroll_to_row(row, None);
        }
        table
            .header(row_height, |mut h| {
                h.col(|ui| header(ui, "Name", SortBy::Name, actions));
                h.col(|ui| header(ui, "Type", SortBy::Type, actions));
                h.col(|ui| header(ui, "Size", SortBy::Size, actions));
            })
            .body(|body| {
                body.rows(row_height, visible.len(), |mut row| {
                    let pos = row.index();
                    let r = &b.rows[visible[pos]];
                    row.set_selected(b.selected.contains(&r.name));
                    let class = r.class();
                    let kind = match r.kind {
                        RowKind::Folder => "Folder".to_string(),
                        RowKind::Archive => "Archive".to_string(),
                        RowKind::File => extension(&r.name),
                        RowKind::Resource(v) => format!("{} (v{v})", extension(&r.name)),
                    };
                    row.col(|ui| {
                        let icon = RichText::new(class.icon());
                        ui.label(if colored { icon.color(class.color(ui.visuals().dark_mode)) } else { icon });
                        let name = RichText::new(&r.name);
                        let response = ui.label(if r.modded { name.color(theme::modded(ui.visuals().dark_mode)) } else { name });
                        if r.modded {
                            let hover = match (b.from_mods, r.added) {
                                (true, true) => "Not in the original game files",
                                (true, false) => "Different from the original game file",
                                (false, _) => MODS_LINK_HOVER,
                            };
                            // A game file the game reads from the mods folder instead.
                            if !b.from_mods {
                                ui.label(RichText::new(MODS_LINK).color(theme::modded(ui.visuals().dark_mode))).on_hover_text(hover);
                            }
                            response.on_hover_text(hover);
                        }
                    });
                    row.col(|ui| {
                        ui.label(kind);
                    });
                    row.col(|ui| {
                        if r.kind != RowKind::Folder {
                            ui.label(size(r.size));
                        }
                    });
                    let response = row.response();
                    if response.clicked() {
                        let (modifiers, now) = response.ctx.input(|i| (i.modifiers, i.time));
                        let delay = response.ctx.options(|o| o.input_options.max_double_click_delay);
                        let double = b.last_click.as_ref().is_some_and(|(name, t)| *name == r.name && now - t <= delay);
                        b.last_click = if double { None } else { Some((r.name.clone(), now)) };
                        if double {
                            actions.push(Action::Enter(r.name.clone()));
                        }
                        if modifiers.shift {
                            let anchor = b.anchor.unwrap_or(pos);
                            let (lo, hi) = (anchor.min(pos), anchor.max(pos));
                            if !modifiers.command {
                                b.selected.clear();
                            }
                            b.selected.extend((lo..=hi).map(|p| b.rows[visible[p]].name.clone()));
                        } else if modifiers.command {
                            if !b.selected.remove(&r.name) {
                                b.selected.insert(r.name.clone());
                            }
                            b.anchor = Some(pos);
                        } else {
                            b.selected = BTreeSet::from([r.name.clone()]);
                            b.anchor = Some(pos);
                        }
                    }
                    let name = r.name.clone();
                    let targets = if b.selected.contains(&name) { b.selected.iter().cloned().collect() } else { vec![name.clone()] };
                    let container = r.is_container();
                    response.context_menu(|ui| {
                        if container && ui.button("Open").clicked() {
                            actions.push(Action::Enter(name.clone()));
                        }
                        if !container && ui.button("Preview").clicked() {
                            actions.push(Action::Preview(name.clone()));
                        }
                        if in_archive && ui.add_enabled(!busy, egui::Button::new("Extract…")).clicked() {
                            actions.push(Action::Extract(targets.clone(), None));
                        }
                        if ui.add_enabled(!busy && targets.len() == 1, egui::Button::new("Rename…")).clicked() {
                            actions.push(Action::AskRename(name.clone()));
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Delete…")).clicked() {
                            actions.push(Action::AskDelete(targets.clone()));
                        }
                    });
                });
            });
    }

}

/// The rows of a folder on disk; files with a copy in the mods folder are marked.
fn folder_rows(dir: &Path, game: Option<&Game>, show_originals: bool) -> Result<Vec<Row>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let is_archive = !is_dir && name.to_ascii_lowercase().ends_with(".rpf");
        let kind = if is_dir {
            RowKind::Folder
        } else if is_archive {
            RowKind::Archive
        } else {
            RowKind::File
        };
        let modded = !is_dir && has_mods_copy(game, show_originals, &path);
        rows.push(Row { name, kind, size: meta.map_or(0, |m| m.len()), modded, added: false });
    }
    Ok(rows)
}

fn archive_rows(path: &ArchivePath, keys: Option<&Keys>) -> Result<(Encryption, Vec<Row>), String> {
    let (archive, dir) = path.open(keys).map_err(|e| format!("{}: {e}", path.disk.display()))?;
    let rows = archive
        .children(dir)
        .map(|i| {
            let e = &archive.entries[i];
            let kind = match e.kind {
                EntryKind::Directory { .. } => RowKind::Folder,
                EntryKind::Binary { .. } if e.is_archive() => RowKind::Archive,
                EntryKind::Binary { .. } => RowKind::File,
                EntryKind::Resource { .. } => RowKind::Resource(e.resource_version().unwrap_or_default()),
            };
            Row { name: e.name.clone(), kind, size: e.size(), modded: false, added: false }
        })
        .collect();
    Ok((archive.encryption, rows))
}

fn has_mods_copy(game: Option<&Game>, show_originals: bool, path: &Path) -> bool {
    let Some(game) = game else { return false };
    if show_originals {
        return false;
    }
    match path.strip_prefix(&game.root) {
        Ok(rel) if !rel.starts_with("mods") => game::resolve_ci(&game.mods_dir(), &rel.to_string_lossy()).is_file(),
        _ => false,
    }
}

/// The entries of a side tree folder: folders and archives, other files too
/// with `all_files`; archives with a copy in the mods folder are marked.
fn tree_entries(dir: &Path, game: Option<&Game>, show_originals: bool, all_files: bool) -> Vec<FsEntry> {
    let mut entries: Vec<FsEntry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = path.is_dir();
            let is_archive = name.to_ascii_lowercase().ends_with(".rpf");
            if name.starts_with('.') || (!is_dir && !is_archive && !all_files) {
                return None;
            }
            let modded = !is_dir && has_mods_copy(game, show_originals, &path);
            Some(FsEntry { path, name, is_dir, modded })
        })
        .collect();
    entries.sort_by_key(|e| (!e.is_dir, e.name.to_ascii_lowercase()));
    entries
}

/// The filter box of the file list (in the menu bar).
const FILTER_ID: &str = "browser filter";

/// The box that filters the file list by name, at `rect`; typing in the list goes there.
pub(crate) fn filter_box(ui: &mut egui::Ui, b: &mut Browser, rect: egui::Rect) {
    let id = egui::Id::unique(FILTER_ID);
    let field = egui::TextEdit::singleline(&mut b.filter)
        .id(id)
        .hint_text("🔍 Filter this folder…")
        .desired_width(rect.width())
        .min_size(rect.size())
        .align(egui::Align2::LEFT_CENTER);
    let response = ui.put(rect, field);
    if b.blur_filter {
        b.blur_filter = false;
        response.surrender_focus();
    }
    if b.focus_filter {
        b.focus_filter = false;
        response.request_focus();
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            let end = egui::text::CCursor::new(b.filter.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        }
    }
    if response.changed() {
        b.selected.clear();
        b.anchor = None;
    }
}

/// Marks the rows that differ from the original game files.
pub(crate) fn mark_changes(rows: &mut [Row], changes: &Changes) {
    for row in rows {
        let change = changes.get(&row.name.to_ascii_lowercase());
        row.modded = change.is_some();
        row.added = change == Some(&vfs::Change::Added);
    }
}

/// Sorts rows within their group (see [`Grouping`]) by the chosen column.
pub(crate) fn sort_rows(rows: &mut [Row], settings: &Settings) {
    let group = |r: &Row| -> (bool, String) {
        match settings.grouping {
            Grouping::FoldersFirst => (r.kind != RowKind::Folder, String::new()),
            Grouping::ByType => (r.kind != RowKind::Folder, extension(&r.name)),
            Grouping::None => (false, String::new()),
        }
    };
    rows.sort_by(|a, b| {
        let by_name = || a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase());
        let within = match settings.sort_by {
            SortBy::Name => by_name(),
            SortBy::Type => extension(&a.name).cmp(&extension(&b.name)).then_with(by_name),
            SortBy::Size => a.size.cmp(&b.size).then_with(by_name),
        };
        let within = if settings.descending { within.reverse() } else { within };
        group(a).cmp(&group(b)).then(within)
    });
}
