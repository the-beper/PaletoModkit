//! The "Snapshots" window: save the mods folder as it is, and go back to a
//! saved state when a change breaks the game.

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::snapshots::{self, Snapshot};

use crate::mods_check::age;
use crate::theme;

pub struct SnapshotsWindow {
    list: Vec<Snapshot>,
    /// Copies share their data (btrfs, XFS): saving is instant and free.
    shared: bool,
    label: String,
    /// The snapshot whose restore awaits confirmation.
    confirm_restore: Option<String>,
    /// Save the current mods folder before restoring.
    save_first: bool,
}

pub enum Request {
    Close,
    Save(String),
    /// Restore this snapshot, saving the current mods folder first if true.
    Restore(String, bool),
    Delete(String),
}

impl SnapshotsWindow {
    pub fn gather(game: &Game) -> Self {
        Self {
            list: snapshots::list(game),
            shared: snapshots::shares_copies(&game.root),
            label: String::new(),
            confirm_restore: None,
            save_first: true,
        }
    }

    /// The labels listed, for tests.
    pub fn labels(&self) -> Vec<String> {
        self.list.iter().map(|s| s.label.clone()).collect()
    }

    pub fn show(&mut self, ctx: &egui::Context, busy: bool) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "snapshots", "Snapshots of the mods folder", 480.0, |ui| self.contents(ui, busy));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        ui.label("A snapshot keeps the whole mods folder as it is now: if a change breaks the game, restore it to go back.");
        ui.label(
            RichText::new(if self.shared {
                "On this filesystem a snapshot is instant and takes no space until files change."
            } else {
                "On this filesystem a snapshot is a full copy: it takes as much space as the mods folder."
            })
            .weak(),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.label).hint_text("Note, e.g. before VisualV").desired_width(260.0));
            if ui.add_enabled(!busy, crate::widgets::primary(ui, "Save a snapshot")).clicked() {
                request = Some(Request::Save(std::mem::take(&mut self.label)));
            }
        });
        crate::widgets::section(ui, "Saved snapshots");
        if self.list.is_empty() {
            ui.label(RichText::new("No snapshots yet.").weak());
        }
        egui::ScrollArea::vertical().max_height(crate::widgets::dialog_list_height(ui.ctx())).show(ui, |ui| {
            for s in &self.list {
                ui.horizontal(|ui| {
                    let ago = s.time().and_then(|t| t.elapsed().ok()).map(|d| format!("{} ago", age(d.as_secs()))).unwrap_or_default();
                    ui.label(if s.label.is_empty() { RichText::new("(no note)").weak() } else { RichText::new(&s.label) });
                    ui.label(RichText::new(ago).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!busy, egui::Button::new("Delete")).on_hover_text("Delete this snapshot; the mods folder stays as it is").clicked() {
                            request = Some(Request::Delete(s.id.clone()));
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Restore…")).on_hover_text("Put the mods folder back as it was then").clicked() {
                            self.confirm_restore = Some(s.id.clone());
                        }
                    });
                });
            }
        });
        if let Some(id) = self.confirm_restore.clone() {
            ui.separator();
            ui.label(RichText::new("The mods folder will be replaced by this snapshot.").color(theme::warning(dark)));
            ui.checkbox(&mut self.save_first, "Save the current mods folder as a snapshot first");
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, crate::widgets::danger(ui, "Replace the mods folder")).clicked() {
                    request = Some(Request::Restore(id, self.save_first));
                    self.confirm_restore = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_restore = None;
                }
            });
        }
        request
    }
}
