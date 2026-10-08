//! The "Last crash" window: what the game's newest crash dump says.

use eframe::egui::{self, RichText};
use paleto_core::crash_report::CrashReport;

use crate::mods_check::age;
use crate::theme;

pub struct CrashWindow {
    /// `None`: the game never crashed (no dump); `Err`: the dump is unreadable.
    report: Result<Option<CrashReport>, String>,
    exe: &'static str,
}

pub enum Request {
    Close,
    /// Open "Find the crashing mod".
    Hunt,
}

impl CrashWindow {
    pub fn new(report: Result<Option<CrashReport>, String>, exe: &'static str) -> Self {
        Self { report, exe }
    }

    /// The verdict shown, for tests.
    pub fn verdict(&self) -> Option<String> {
        self.report.as_ref().ok()?.as_ref().map(|r| r.verdict(self.exe))
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "crash", "Last crash", 560.0, |ui| self.contents(ui));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        let r = match &self.report {
            Err(e) => {
                ui.label(RichText::new(format!("Could not read the crash dump: {e}")).color(theme::error(dark)));
                return None;
            }
            Ok(None) => {
                ui.label("No crash dump: the game has not crashed (it writes one in its CrashLogs folder when it does).");
                return None;
            }
            Ok(Some(r)) => r,
        };
        let ago = r.time.and_then(|t| t.elapsed().ok()).map(|d| format!("{} ago", age(d.as_secs()))).unwrap_or_default();
        ui.label(RichText::new(format!("The game crashed {ago}")).strong());
        ui.add_space(4.0);
        ui.label(RichText::new(r.verdict(self.exe)).color(theme::warning(dark)));
        if r.same_place > 0 {
            let times = if r.same_place == 1 { "once more".to_string() } else { format!("{} more times", r.same_place) };
            ui.label(format!("The game crashed at the same place {times} before: a cause that is still there."));
        }
        crate::widgets::section(ui, "Details");
        egui::Grid::new("crash-details").num_columns(2).show(ui, |ui| {
            ui.label("Exception");
            ui.label(format!("{} (0x{:08X})", r.code_name(), r.code));
            ui.end_row();
            ui.label("Where");
            ui.label(match &r.module {
                Some((name, offset)) => format!("{name} + 0x{offset:X}"),
                None => format!("0x{:X}, outside every module", r.address),
            });
            ui.end_row();
            if let Some((kind, addr)) = r.access {
                ui.label("Access");
                ui.label(format!("{kind} 0x{addr:X}"));
                ui.end_row();
            }
            if !r.stack.is_empty() {
                ui.label("On the stack");
                ui.add(egui::Label::new(r.stack.join(", ")).wrap());
                ui.end_row();
            }
            if let Some(archive) = &r.last_archive {
                ui.label("Last archive");
                ui.add(egui::Label::new(archive.describe()).wrap());
                ui.end_row();
            }
            if let Some((mounted, limit)) = r.archives {
                ui.label("Archives");
                ui.label(format!("{mounted} of {limit}"));
                ui.end_row();
            }
        });
        let tight = r.tight_stores();
        if !tight.is_empty() {
            crate::widgets::section(ui, "Asset stores with no room left");
            egui::Grid::new("crash-stores").num_columns(3).show(ui, |ui| {
                for s in tight {
                    let color = if s.full() { theme::error(dark) } else { theme::warning(dark) };
                    ui.label(RichText::new(&s.name).monospace());
                    ui.label(format!("{} of {}", s.used, s.size));
                    ui.label(RichText::new(if s.full() { "full" } else { "nearly full" }).color(color));
                    ui.end_row();
                }
            });
            crate::widgets::hint(ui, "The game allows this many files of each kind; gameconfig.xml sets the sizes.");
        }
        if !r.context.is_empty() {
            ui.collapsing("Game state at the crash", |ui| {
                for line in &r.context {
                    ui.label(RichText::new(line).monospace().small());
                }
            });
        }
        ui.label(RichText::new(r.dump.display().to_string()).weak().small());
        ui.add_space(6.0);
        if ui.add(crate::widgets::primary(ui, "Find the crashing mod…")).clicked() {
            request = Some(Request::Hunt);
        }
        request
    }
}
