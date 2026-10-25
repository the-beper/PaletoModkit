//! The "Crash detection" window: what the game's crash dumps say, the likely
//! causes with a fix for each, and the search by elimination (see
//! paleto_core::crash_hunt), in one place.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::crash_hunt::{self, Hunt, Part};
use paleto_core::crash_report::{self, CrashReport, CrashSummary, LogFindings};
use paleto_core::diagnosis::{self, Cause, Fix, Severity};
use paleto_rpf::Keys;

use crate::mods_check::age;
use crate::task::Task;
use crate::theme;

/// What the window asks the app to do.
pub enum Request {
    Close,
    UpdateScriptHook,
    ModsInGame,
    /// Turn this `.asi` plugin off.
    TurnOff(String),
    HuntStart,
    /// The answer to the current test: whether the game still crashed.
    HuntAnswer(bool),
    HuntStop,
    /// Look inside this archive (the culprit) for the file.
    HuntInside(Part),
}

/// One crash looked at: its report and the causes it points to.
struct Detail {
    /// `None`: the game never crashed (no dump); `Err`: the dump is unreadable.
    report: Result<Option<CrashReport>, String>,
    causes: Vec<Cause>,
}

impl Detail {
    fn load(game: &Game, keys: Option<&Keys>, dump: Option<PathBuf>, logs: &[LogFindings]) -> Self {
        let report = match dump.or_else(|| crash_report::latest_dump(game)) {
            None => Ok(None),
            Some(dump) => crash_report::report_of(game, &dump, keys).map(Some).map_err(|e| e.to_string()),
        };
        let causes = diagnosis::causes(game, report.as_ref().ok().and_then(Option::as_ref), logs);
        Self { report, causes }
    }
}

/// Everything read off the UI thread when the window opens.
struct Loaded {
    history: Vec<CrashSummary>,
    logs: Vec<LogFindings>,
    detail: Detail,
}

pub struct CrashWindow {
    game: Game,
    keys: Option<Arc<Keys>>,
    ctx: egui::Context,
    loading: Option<Task<Loaded>>,
    /// The detail of another crash of the history, being read.
    selecting: Option<Task<Detail>>,
    data: Option<Loaded>,
    hunt: Option<Hunt>,
    /// Parts still turned off with no hunt going on: the culprit found, or
    /// leftovers of a hunt.
    turned_off: Vec<Part>,
    /// Scroll to the elimination section, once.
    scroll_to_hunt: bool,
    /// Open the plugin logs, once.
    open_logs: bool,
}

impl CrashWindow {
    pub fn gather(game: &Game, keys: Option<Arc<Keys>>, ctx: &egui::Context) -> Self {
        let mut w = Self {
            game: game.clone(),
            keys,
            ctx: ctx.clone(),
            loading: None,
            selecting: None,
            data: None,
            hunt: None,
            turned_off: Vec::new(),
            scroll_to_hunt: false,
            open_logs: false,
        };
        w.reload(None);
        w
    }

    /// Reads everything again (after a hunt step or a plugin turned off), keeping the crash shown.
    pub fn reload(&mut self, keep: Option<PathBuf>) {
        let keep = keep.or_else(|| self.selected().cloned());
        self.hunt = crash_hunt::load(&self.game);
        self.turned_off = if self.hunt.is_none() { crash_hunt::turned_off(&self.game) } else { Vec::new() };
        self.selecting = None;
        let (game, keys) = (self.game.clone(), self.keys.clone());
        self.loading = Some(Task::spawn("Reading the crash dumps", &self.ctx, move || {
            let logs = crash_report::plugin_logs(&game);
            let detail = Detail::load(&game, keys.as_deref(), keep, &logs);
            Loaded { history: crash_report::history(&game), logs, detail }
        }));
    }

    /// Whether a background read is going on.
    pub fn loading(&self) -> bool {
        self.loading.is_some() || self.selecting.is_some()
    }

    fn selected(&self) -> Option<&PathBuf> {
        match &self.data.as_ref()?.detail.report {
            Ok(Some(r)) => Some(&r.dump),
            _ => None,
        }
    }

    fn select(&mut self, dump: PathBuf) {
        let Some(data) = &self.data else { return };
        let (game, keys, logs) = (self.game.clone(), self.keys.clone(), data.logs.clone());
        self.selecting = Some(Task::spawn("Reading a crash dump", &self.ctx, move || Detail::load(&game, keys.as_deref(), Some(dump), &logs)));
    }

    fn poll(&mut self) {
        if let Some(loaded) = self.loading.as_ref().and_then(Task::poll) {
            self.loading = None;
            self.data = Some(loaded);
        }
        if let Some(detail) = self.selecting.as_ref().and_then(Task::poll) {
            self.selecting = None;
            if let Some(data) = &mut self.data {
                data.detail = detail;
            }
        }
    }

    /// What the window shows, for tests.
    pub fn summary(&self) -> String {
        let hunt = match &self.hunt {
            Some(h) => format!("testing: {} suspects, off: {}", h.suspects.len(), h.disabled.join(", ")),
            None if self.turned_off.is_empty() => "idle".into(),
            None => format!("turned off: {}", self.turned_off.join(", ")),
        };
        let Some(data) = &self.data else { return format!("loading\nhunt: {hunt}") };
        let mut lines = Vec::new();
        match &data.detail.report {
            Ok(Some(r)) => {
                lines.push(format!("shown: {}", r.dump.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                lines.push(format!("verdict: {}", r.verdict(self.game.edition.exe_name())));
            }
            Ok(None) => lines.push("no crash".into()),
            Err(e) => lines.push(format!("unreadable: {e}")),
        }
        lines.extend(data.detail.causes.iter().map(|c| format!("cause: {:?}: {}", c.severity, c.text)));
        lines.push(format!("history: {}", data.history.len()));
        lines.push(format!("hunt: {hunt}"));
        lines.join("\n")
    }

    pub fn show(&mut self, ctx: &egui::Context, busy: bool) -> Option<Request> {
        self.poll();
        let (request, closed) = crate::widgets::dialog(ctx, "crash", "Crash detection", 600.0, |ui| self.contents(ui, busy));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<Request> {
        let mut request = None;
        let hunting = self.hunt.is_some();
        self.header(ui);
        // A hunt going on comes first, with the rest folded away.
        if hunting {
            request = self.hunt_section(ui, busy);
        }
        let mut fix = None;
        if let Some(data) = &self.data {
            crate::widgets::section(ui, "Likely causes");
            fix = causes_ui(ui, &data.detail.causes, busy);
        }
        match fix {
            Some(Fix::UpdateScriptHook) => request = Some(Request::UpdateScriptHook),
            Some(Fix::TurnOff(part)) => request = Some(Request::TurnOff(part)),
            Some(Fix::ModsInGame) => request = Some(Request::ModsInGame),
            Some(Fix::Eliminate) => self.scroll_to_hunt = true,
            Some(Fix::OpenLog(_)) => self.open_logs = true,
            None => {}
        }
        if !hunting {
            request = self.hunt_section(ui, busy).or(request);
        }
        self.history_section(ui, hunting);
        self.details_section(ui, hunting);
        self.logs_section(ui, hunting);
        request
    }

    fn header(&self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let Some(data) = &self.data else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the crash dumps…");
            });
            return;
        };
        match &data.detail.report {
            Err(e) => {
                ui.label(RichText::new(format!("Could not read the crash dump: {e}")).color(theme::error(dark)));
            }
            Ok(None) => {
                ui.label(RichText::new("No crash dump yet: the game has not crashed").strong());
                crate::widgets::hint(ui, "The game writes one in its CrashLogs folder when it does.");
            }
            Ok(Some(r)) => {
                let ago = r.time.and_then(|t| t.elapsed().ok()).map(|d| format!(" {} ago", age(d.as_secs()))).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("The game crashed{ago}")).strong());
                    if self.selecting.is_some() {
                        ui.spinner();
                    }
                });
                if r.same_place > 0 {
                    let times = if r.same_place == 1 { "once more".to_string() } else { format!("{} more times", r.same_place) };
                    ui.label(format!("The game crashed at the same place {times} before: a cause that is still there."));
                }
            }
        }
    }

    fn hunt_section(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        let button = |ui: &mut egui::Ui, text: &str| ui.add_enabled(!busy, egui::Button::new(text)).clicked();
        crate::widgets::section(ui, "Find the culprit by elimination");
        if std::mem::take(&mut self.scroll_to_hunt) {
            ui.scroll_to_cursor(Some(egui::Align::Min));
        }
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
                            request = Some(Request::HuntInside(archive));
                        }
                        if button(ui, "Turn everything back on") {
                            request = Some(Request::HuntStop);
                        }
                    });
                } else if ui.add_enabled(!busy, crate::widgets::primary(ui, "Start")).clicked() {
                    request = Some(Request::HuntStart);
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
                        request = Some(Request::HuntAnswer(true));
                    }
                    if button(ui, "It works") {
                        request = Some(Request::HuntAnswer(false));
                    }
                    if button(ui, "Stop and turn everything back on") {
                        request = Some(Request::HuntStop);
                    }
                });
            }
        }
        request
    }

    fn history_section(&mut self, ui: &mut egui::Ui, hunting: bool) {
        let Some(data) = &self.data else { return };
        if data.history.is_empty() {
            return;
        }
        crate::widgets::section(ui, "Crashes");
        let selected = self.selected().cloned();
        let mut clicked = None;
        egui::CollapsingHeader::new(format!("History ({} crashes)", data.history.len())).id_salt(("crash-history", hunting)).show(ui, |ui| {
            for c in &data.history {
                let when = c.time.and_then(|t| t.elapsed().ok()).map(|d| format!("{} ago", age(d.as_secs()))).unwrap_or_else(|| "some time ago".into());
                let place = match &c.place {
                    Some((name, offset)) => format!("{name} + 0x{offset:X}"),
                    None => "outside every module".into(),
                };
                let again = if c.same_place > 0 { format!(" · same place ×{}", c.same_place + 1) } else { String::new() };
                let text = format!("{when} · {} · {place}{again}", crash_report::code_name(c.code));
                if ui.selectable_label(selected.as_ref() == Some(&c.dump), text).on_hover_text(c.dump.display().to_string()).clicked() {
                    clicked = Some(c.dump.clone());
                }
            }
        });
        if let Some(dump) = clicked.filter(|d| selected.as_ref() != Some(d)) {
            self.select(dump);
        }
    }

    fn details_section(&self, ui: &mut egui::Ui, hunting: bool) {
        let Some(Loaded { detail: Detail { report: Ok(Some(r)), .. }, .. }) = &self.data else { return };
        let dark = ui.visuals().dark_mode;
        egui::CollapsingHeader::new("Details").id_salt(("crash-details-head", hunting)).default_open(!hunting).show(ui, |ui| {
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
        });
    }

    fn logs_section(&mut self, ui: &mut egui::Ui, hunting: bool) {
        let Some(data) = &self.data else { return };
        if data.logs.is_empty() {
            return;
        }
        let mut header = egui::CollapsingHeader::new(format!("Plugin logs ({} with errors)", data.logs.len())).id_salt(("crash-logs", hunting));
        if std::mem::take(&mut self.open_logs) {
            header = header.open(Some(true));
        }
        header.show(ui, |ui| {
            for log in &data.logs {
                let name = log.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let ago = log.time.and_then(|t| t.elapsed().ok()).map(|d| format!(", {} ago", age(d.as_secs()))).unwrap_or_default();
                ui.label(RichText::new(format!("{name}{ago}")).strong());
                for line in &log.lines {
                    ui.add(egui::Label::new(RichText::new(line).monospace().small()).wrap());
                }
                ui.add_space(4.0);
            }
        });
    }
}

/// The causes with their fix buttons; the fix clicked, if any.
fn causes_ui(ui: &mut egui::Ui, causes: &[Cause], busy: bool) -> Option<Fix> {
    let dark = ui.visuals().dark_mode;
    if causes.is_empty() {
        crate::widgets::hint(ui, "Nothing stands out: no crash, no incompatible Script Hook V, no plugin to blame.");
        return None;
    }
    let mut clicked = None;
    for cause in causes {
        let (icon, color) = match cause.severity {
            Severity::Problem => ("✖", theme::error(dark)),
            Severity::Warning => ("⚠", theme::warning(dark)),
            Severity::Info => ("•", ui.visuals().weak_text_color()),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(icon).color(color));
            ui.label(&cause.text);
        });
        // The elimination search is the next section.
        if let Some(fix) = cause.fix.as_ref().filter(|f| !matches!(f, Fix::Eliminate)) {
            let text = match fix {
                Fix::UpdateScriptHook => "Update Script Hook V…".to_string(),
                Fix::TurnOff(part) => format!("Turn off {part}"),
                Fix::ModsInGame => "Open Mods in game".to_string(),
                Fix::Eliminate => unreachable!("filtered out above"),
                Fix::OpenLog(_) => "Show the log".to_string(),
            };
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let enabled = !busy || matches!(fix, Fix::OpenLog(_));
                if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                    clicked = Some(fix.clone());
                }
            });
        }
        ui.add_space(2.0);
    }
    clicked
}
