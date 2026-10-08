//! The "Export mods as OIV" window: pick changes of the mods folder and
//! write them as an OIV package.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use paleto_core::export::{Changed, Kind, Metadata};

pub struct ExportWindow {
    /// `None` while the mods folder is compared with the game's files.
    changes: Option<Vec<Changed>>,
    selected: Vec<bool>,
    filter: String,
    meta: Metadata,
}

pub enum Request {
    Close,
    /// Write these changes as a package.
    Write(PathBuf, Vec<Changed>, Metadata),
}

/// What a change belongs to, for grouping: its add-on pack's folder, or the
/// archive (or folder) of the game it is in.
fn group(c: &Changed) -> String {
    if c.kind == Kind::Pack {
        return c.path.splitn(5, '/').take(4).collect::<Vec<_>>().join("/");
    }
    match c.path.to_ascii_lowercase().find(".rpf/") {
        Some(i) => c.path[..i + 4].to_string(),
        None => c.path.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string()),
    }
}

impl ExportWindow {
    pub fn new() -> Self {
        Self {
            changes: None,
            selected: Vec::new(),
            filter: String::new(),
            meta: Metadata { name: "My mods".into(), version: "1.0".into(), author: String::new(), description: String::new() },
        }
    }

    pub fn set_changes(&mut self, changes: Vec<Changed>) {
        self.selected = vec![true; changes.len()];
        self.changes = Some(changes);
    }

    /// How many changes are listed and selected, for tests.
    pub fn counts(&self) -> (usize, usize) {
        (self.changes.as_ref().map_or(0, Vec::len), self.selected.iter().filter(|s| **s).count())
    }

    pub fn show(&mut self, ctx: &egui::Context, busy: bool) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "export", "Export mods as an OIV package", 620.0, |ui| self.contents(ui, busy));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<Request> {
        let mut request = None;
        let Self { changes, selected, filter, meta } = self;
        let Some(changes) = changes.as_ref() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Comparing the mods folder with the game's files… (reading the archives can take a minute)");
            });
            return None;
        };
        if changes.is_empty() {
            ui.label("The mods folder holds no change to the game's files.");
            return None;
        }
        ui.label(
            "The package holds the files you changed, added or deleted inside the game's archives, and your add-on packs \
             (with their DLC list entry). Installed on a clean mods folder, it rebuilds these changes.",
        );
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(filter).hint_text("Filter, e.g. timecycle").desired_width(220.0));
            let shown: Vec<usize> = (0..changes.len()).filter(|&i| matches(filter, &changes[i])).collect();
            if ui.button("Select shown").clicked() {
                shown.iter().for_each(|&i| selected[i] = true);
            }
            if ui.button("Unselect shown").clicked() {
                shown.iter().for_each(|&i| selected[i] = false);
            }
        });
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, c) in changes.iter().enumerate() {
            if matches(filter, c) {
                groups.entry(group(c)).or_default().push(i);
            }
        }
        egui::ScrollArea::vertical().id_salt("changes").max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
            for (name, items) in &groups {
                let on = items.iter().filter(|&&i| selected[i]).count();
                ui.horizontal(|ui| {
                    let mut all = on == items.len();
                    if ui.checkbox(&mut all, "").changed() {
                        items.iter().for_each(|&i| selected[i] = all);
                    }
                    egui::CollapsingHeader::new(format!("{name}  ({on} of {})", items.len())).id_salt(name).show(ui, |ui| {
                        for &i in items {
                            let c = &changes[i];
                            let rest = c.path.strip_prefix(name.as_str()).unwrap_or(&c.path).trim_start_matches('/');
                            let kind = match c.kind {
                                Kind::Added => "new",
                                Kind::Modified => "changed",
                                Kind::Deleted => "deleted",
                                Kind::Pack => "pack",
                            };
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut selected[i], RichText::new(if rest.is_empty() { &c.path } else { rest }).monospace().small());
                                ui.label(RichText::new(kind).weak().small());
                            });
                        }
                    });
                });
            }
        });
        crate::widgets::section(ui, "Package");
        egui::Grid::new("export-meta").num_columns(2).show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut meta.name);
            ui.end_row();
            ui.label("Version");
            ui.text_edit_singleline(&mut meta.version);
            ui.end_row();
            ui.label("Author");
            ui.text_edit_singleline(&mut meta.author);
            ui.end_row();
        });
        let chosen: Vec<Changed> = changes.iter().zip(selected.iter()).filter(|(_, s)| **s).map(|(c, _)| c.clone()).collect();
        ui.horizontal(|ui| {
            let label = if chosen.len() == 1 { "Save 1 change as…".to_string() } else { format!("Save {} changes as…", chosen.len()) };
            if ui.add_enabled(!busy && !chosen.is_empty(), crate::widgets::primary(ui, label)).clicked() {
                let file = format!("{}.oiv", meta.name.trim());
                if let Some(path) = rfd::FileDialog::new().add_filter("OpenIV package", &["oiv"]).set_file_name(file).save_file() {
                    let mut meta = meta.clone();
                    meta.description = format!("Made with Paleto Modkit ({} changes).", chosen.len());
                    request = Some(Request::Write(path, chosen, meta));
                }
            }
        });
        request
    }

}

fn matches(filter: &str, c: &Changed) -> bool {
    filter.is_empty() || c.path.to_ascii_lowercase().contains(&filter.to_ascii_lowercase())
}
