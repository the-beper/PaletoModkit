//! The "Hash generator" window: the hashes of names and the names of hashes,
//! with or without a game open. The game hashes names by Jenkins one-at-a-time
//! ([`paleto_core::hashes`]); resolution is dictionary based: the built-in
//! metadata names and the names of the user's store ([`paleto_core::names_store`]).

use eframe::egui::{self, RichText};
use paleto_core::hashes::{self, Mode};
use paleto_core::names_store;
use paleto_formats::Names;

pub struct HashesWindow {
    mode: Mode,
    /// The names to hash, one per line.
    generate: String,
    /// The values to resolve, one per line.
    resolve: String,
    /// The names the user pasted to save, one per line.
    pasted: String,
    /// How the last import went (None: nothing imported yet this window).
    feedback: Option<String>,
    /// The built-in names plus the names of the store ("yours").
    learned: Names,
    /// How many names the store holds.
    yours: usize,
    /// The resolve rows, remade when the text changes: the store of names is not read again per frame.
    resolved: Vec<Resolved>,
    /// The generate rows, same.
    generated: Vec<Generated>,
}

struct Resolved {
    input: String,
    /// Every known name, or "unknown"; "not a value" when the text is none of the forms.
    names: String,
}

struct Generated {
    input: String,
    hash: u32,
}

pub enum Request {
    Close,
}

impl HashesWindow {
    pub fn gather() -> Self {
        let store = names_store::path();
        let mut learned = Names::new();
        names_store::seed(&mut learned, &store);
        let yours = names_store::load(&store).len();
        Self {
            mode: Mode::default(),
            generate: String::new(),
            resolve: String::new(),
            pasted: String::new(),
            feedback: None,
            learned,
            yours,
            resolved: Vec::new(),
            generated: Vec::new(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Request> {
        let (_, closed) = crate::widgets::dialog(ctx, "hashes", "Hash generator and name resolver", 660.0, |ui| self.contents(ui));
        closed.then_some(Request::Close)
    }

    /// The generate rows again, for the current text and mode.
    fn regen(&mut self) {
        self.generated = self.generate.lines().filter(|l| !l.trim().is_empty()).map(|input| Generated { input: input.to_string(), hash: self.mode.hash(input) }).collect();
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("The game hashes names without a way back: a hash is only resolved through the names known here.").weak());
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Mode").weak());
        for mode in Mode::ALL {
            let hover = match mode {
                Mode::Lower => "Hashes the text lowercased, the way the game hashes asset names and text labels",
                Mode::Exact => "Hashes the bytes as they are, the way metadata names (CMapData) are hashed",
                Mode::Awc => "The 29-bit id an audio container (.awc) names its sounds with",
            };
            if ui.radio(self.mode == mode, mode_ui_text(mode)).on_hover_text(hover).clicked() {
                self.mode = mode;
                self.regen();
            }
        }
        ui.add_space(6.0);
        if ui
            .add(egui::TextEdit::multiline(&mut self.generate).hint_text("Names to hash, one per line").desired_rows(2).desired_width(f32::INFINITY).code_editor())
            .changed()
        {
            self.regen();
        }
        if !self.generated.is_empty() {
            ui.add_space(4.0);
            egui::Grid::new("hashed").striped(true).num_columns(4).min_col_width(110.0).max_col_width(315.0).show(ui, |ui| {
                for row in &self.generated {
                    ui.label(&row.input);
                    value_button(ui, hashes::as_hex(row.hash));
                    value_button(ui, hashes::as_unsigned(row.hash));
                    value_button(ui, hashes::as_signed(row.hash));
                    ui.end_row();
                }
            });
        }
        ui.add_space(8.0);
        if ui
            .add(egui::TextEdit::multiline(&mut self.resolve).hint_text("Values to resolve, one per line (0x3779A091, B779A091, hash_…, decimal)").desired_rows(2).desired_width(f32::INFINITY).code_editor())
            .changed()
        {
            self.resolved = self.resolve.lines().filter(|l| !l.trim().is_empty()).map(|input| self.resolved_row(input)).collect();
        }
        if !self.resolved.is_empty() {
            ui.add_space(4.0);
            egui::Grid::new("resolved").striped(true).num_columns(2).min_col_width(110.0).max_col_width(430.0).show(ui, |ui| {
                for row in &self.resolved {
                    ui.label(&row.input);
                    ui.label(&row.names);
                    ui.end_row();
                }
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(match self.yours {
                0 => "The resolution also knows names you saved: import a plain-text list, one name per line.".to_string(),
                n => format!("The resolution also knows {n} names of yours."),
            }));
        });
        ui.horizontal(|ui| {
            if ui.button("Save these names…").on_hover_text("Save the names pasted below (one name per line) to the store").clicked() {
                match names_store::import_text(&self.pasted, &names_store::path()) {
                    Ok(0) => self.feedback = Some("They are in the store already; nothing was added.".into()),
                    Ok(n) => {
                        self.feedback = Some(if n == 1 { "1 name saved.".into() } else { format!("{n} names saved.") });
                        self.pasted.clear();
                        self.reload();
                    }
                    Err(e) => self.feedback = Some(format!("! not saved: {e}")),
                }
            }
            if ui.button("Import a text file…").on_hover_text("Add the names of a plain-text file (one per line) to the store").clicked()
                && let Some(files) = rfd::FileDialog::new().set_title("Names, one per line").pick_files()
            {
                self.import_files(files);
            }
            if let Some(feedback) = &self.feedback {
                ui.label(RichText::new(feedback).weak());
            }
        });
        ui.add(egui::TextEdit::multiline(&mut self.pasted).hint_text("Names to save, one per line").desired_rows(2).desired_width(f32::INFINITY).code_editor());
    }

    /// One row of the resolve pane: the value read back (None: not a hash as
    /// text) and every name that claims it (the dictionaries are not inverted,
    /// so an unknown value stays unknown).
    fn resolved_row(&self, input: &str) -> Resolved {
        let value = hashes::parse_value(input);
        let names = match value {
            Some(hash) if self.learned.lookup(hash).is_some() => self
                .learned
                .candidates(hash)
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            Some(_) => "unknown".to_string(),
            None => "not a value".to_string(),
        };
        Resolved { input: input.to_string(), names }
    }

    /// Import files into the store, then learn them again.
    fn import_files(&mut self, files: Vec<std::path::PathBuf>) {
        let mut added = 0usize;
        let mut failed: Option<String> = None;
        for file in files {
            match names_store::import(&file, &names_store::path()) {
                Ok(n) => added += n,
                Err(e) => failed = Some(format!("{e}")),
            }
        }
        self.feedback = Some(match (&failed, added) {
            (None, 0) => "They are in the store already; nothing was added.".into(),
            (None, 1) => "1 name imported.".into(),
            (None, n) => format!("{n} names imported."),
            (Some(e), n) => format!("{n} names imported; one list failed: {e}"),
        });
        if added > 0 {
            self.reload();
        }
    }

    /// The names of the store again, after a change to it.
    fn reload(&mut self) {
        self.learned = Names::new();
        names_store::seed(&mut self.learned, &names_store::path());
        self.yours = names_store::load(&names_store::path()).len();
        self.resolved = self.resolve.lines().filter(|l| !l.trim().is_empty()).map(|input| self.resolved_row(input)).collect();
    }
}

/// A number shown as one text; clicking copies it.
fn value_button(ui: &mut egui::Ui, text: String) {
    if ui.small_button(&text).on_hover_text("Click to copy").clicked() {
        ui.ctx().copy_text(text);
    }
}

/// The label of a mode in the radio: the short one, with nothing else on the line.
fn mode_ui_text(mode: Mode) -> &'static str {
    match mode {
        Mode::Lower => "lowercase (as the game hashes asset names)",
        Mode::Exact => "exact bytes (metadata names: CMapData)",
        Mode::Awc => "AWC 29-bit (sound ids)",
    }
}
