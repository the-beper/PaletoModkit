//! The "Find the crashing mod" window: a guided search by halves (see
//! paleto_core::crash_hunt).

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::crash_hunt::{self, Hunt, Part};

use crate::theme;

pub struct CrashHuntWindow {
    hunt: Option<Hunt>,
    /// Parts still turned off with no hunt going on: the culprit found, or
    /// leftovers of a hunt.
    turned_off: Vec<Part>,
}

pub enum Request {
    Close,
    Start,
    /// The answer to the current test: whether the game still crashed.
    Answer(bool),
    Stop,
    /// Look inside this archive (the culprit) for the file.
    Inside(Part),
}

impl CrashHuntWindow {
    pub fn gather(game: &Game) -> Self {
        let hunt = crash_hunt::load(game);
        let turned_off = if hunt.is_none() { crash_hunt::turned_off(game) } else { Vec::new() };
        Self { hunt, turned_off }
    }

    /// What the window shows, for tests.
    pub fn summary(&self) -> String {
        match &self.hunt {
            Some(h) => format!("testing: {} suspects, off: {}", h.suspects.len(), h.disabled.join(", ")),
            None if self.turned_off.is_empty() => "idle".into(),
            None => format!("turned off: {}", self.turned_off.join(", ")),
        }
    }

    pub fn show(&self, ctx: &egui::Context, busy: bool) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "crash_hunt", "Find the crashing mod", 520.0, |ui| self.contents(ui, busy));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&self, ui: &mut egui::Ui, busy: bool) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        let button = |ui: &mut egui::Ui, text: &str| ui.add_enabled(!busy, egui::Button::new(text)).clicked();
        match &self.hunt {
            None => {
                ui.label("For a game that crashes when you do not know which mod causes it:");
                ui.add_space(4.0);
                let steps = [
                    "Half of the suspects (.asi plugins, add-on packs, modified archives) are turned off.",
                    "You start the game and do what made it crash.",
                    "You say whether it still crashed: each answer halves the suspects, until one is left.",
                ];
                for (n, step) in steps.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{}.", n + 1)).strong());
                        ui.label(*step);
                    });
                }
                ui.add_space(4.0);
                crate::widgets::hint(
                    ui,
                    "Parts are moved to the game folder's mods_disabled folder and back. Copies of game files left \
                     unchanged are not suspects. Starting reads the mods folder whole, which can take a while.",
                );
                ui.add_space(6.0);
                if !self.turned_off.is_empty() {
                    ui.label(RichText::new(format!("Turned off: {}", self.turned_off.join(", "))).color(theme::warning(dark)));
                    ui.label("If the game works without it, remove or update that mod; otherwise turn everything back on.");
                    let archive = match self.turned_off.as_slice() {
                        [one] if one.starts_with("mods/") && one.to_ascii_lowercase().ends_with(".rpf") => Some(one.clone()),
                        _ => None,
                    };
                    ui.horizontal(|ui| {
                        if let Some(archive) = archive
                            && ui
                                .add_enabled(!busy, egui::Button::new("Look inside it"))
                                .on_hover_text("The archive may hold several mods: a hunt among its changed files finds the one")
                                .clicked()
                        {
                            request = Some(Request::Inside(archive));
                        }
                        if button(ui, "Turn everything back on") {
                            request = Some(Request::Stop);
                        }
                    });
                } else if ui.add_enabled(!busy, crate::widgets::primary(ui, "Start")).clicked() {
                    request = Some(Request::Start);
                }
            }
            Some(hunt) => {
                if let Some(top) = &hunt.inside {
                    ui.label(format!("Looking inside {top}: the game's own files are put back for the files turned off."));
                }
                let answers = if hunt.steps == 1 { "1 answer".to_string() } else { format!("{} answers", hunt.steps) };
                ui.label(format!("{} suspects left, after {answers}.", hunt.suspects.len()));
                ui.collapsing(format!("Turned off for this test ({})", hunt.disabled.len()), |ui| {
                    for p in &hunt.disabled {
                        ui.label(RichText::new(p).monospace().small());
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Start the game and do what made it crash. Then:").strong());
                ui.horizontal(|ui| {
                    if button(ui, "It crashed") {
                        request = Some(Request::Answer(true));
                    }
                    if button(ui, "It works") {
                        request = Some(Request::Answer(false));
                    }
                    if button(ui, "Stop and turn everything back on") {
                        request = Some(Request::Stop);
                    }
                });
            }
        }
        request
    }
}
