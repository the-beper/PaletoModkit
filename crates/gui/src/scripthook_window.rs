//! The "Script Hook V" window: the installed and the latest version, whether
//! it suits the game, and the update.

use eframe::egui::{self, RichText};
use paleto_core::Game;
use paleto_core::scripthook::{self, Compat, Release};

use crate::task::Task;
use crate::theme;

const PAGE: &str = "https://www.dev-c.com/gtav/scripthookv/";

/// The latest release, as far as it is known.
enum Latest {
    /// Not looked for (offline checks are off).
    NotChecked,
    Fetching(Task<Result<Release, String>>),
    Done(Result<Release, String>),
}

pub struct ScriptHookWindow {
    game: Game,
    installed: Option<String>,
    game_version: Option<[u16; 4]>,
    compat: Compat,
    latest: Latest,
}

pub enum Request {
    Close,
    Retry,
    Update(Release),
}

/// How well the installed Script Hook V suits the game. Its log tells only up to the
/// last game run: when the DLL is newer than the log (just updated), the log is ignored.
pub fn compat_now(game: &Game) -> Compat {
    let modified = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let dll = modified(paleto_core::game::resolve_ci(&game.root, "ScriptHookV.dll"));
    let log = modified(paleto_core::game::resolve_ci(&game.root, "ScriptHookV.log"));
    if dll.zip(log).is_some_and(|(dll, log)| dll > log) {
        scripthook::judge(game.edition, scripthook::installed(game).as_deref(), scripthook::game_version(game), None, false)
    } else {
        scripthook::compat(game)
    }
}

/// "1.0.1158.16".
pub fn format_version(v: [u16; 4]) -> String {
    format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
}

/// Whether `release` is newer than what is installed (or nothing is).
pub fn is_update(installed: Option<&str>, release: &Release) -> bool {
    installed.is_none_or(|i| scripthook::newer(&release.version, i))
}

/// The sentence for an incompatible Script Hook V, with what to do about it.
pub fn incompatible_text(supports: &str, game: &str, installed: Option<&str>, latest: Option<&Release>) -> String {
    let base = format!("Script Hook V supports {supports} but the game is {game}: script mods will not run until a newer Script Hook V is released.");
    match latest {
        Some(r) if is_update(installed, r) => format!("Script Hook V supports {supports} but the game is {game}: update Script Hook V to v{}.", r.version),
        Some(_) => format!("Script Hook V supports {supports} but the game is {game}, and it is already the latest release: wait for the author's update (script mods will not run until then)."),
        None => base,
    }
}

impl ScriptHookWindow {
    /// `known` is a release found earlier; otherwise it is fetched when `online`.
    pub fn gather(game: &Game, known: Option<Release>, online: bool, ctx: &egui::Context) -> Self {
        let latest = match known {
            Some(r) => Latest::Done(Ok(r)),
            None if online => Latest::Fetching(fetch(ctx)),
            None => Latest::NotChecked,
        };
        let mut window = Self { game: game.clone(), installed: None, game_version: None, compat: Compat::Unknown, latest };
        window.refresh_facts();
        window
    }

    /// Reads the installed version and the compatibility again (after an update).
    pub fn refresh_facts(&mut self) {
        self.installed = scripthook::installed(&self.game);
        self.game_version = scripthook::game_version(&self.game);
        self.compat = compat_now(&self.game);
    }

    /// Looks for the latest release again.
    pub fn retry(&mut self, ctx: &egui::Context) {
        self.latest = Latest::Fetching(fetch(ctx));
    }

    pub fn poll(&mut self) {
        if let Latest::Fetching(task) = &self.latest
            && let Some(result) = task.poll()
        {
            self.latest = Latest::Done(result);
        }
    }

    fn release(&self) -> Option<&Release> {
        match &self.latest {
            Latest::Done(Ok(r)) => Some(r),
            _ => None,
        }
    }

    fn compat_text(&self) -> String {
        match &self.compat {
            Compat::Confirmed => "works with this game version".into(),
            Compat::Likely => "built for this game version, not run yet: start the game once to confirm".into(),
            Compat::Incompatible { supports, game } => incompatible_text(supports, game, self.installed.as_deref(), self.release()),
            Compat::Unknown => "unknown".into(),
        }
    }

    /// The window's text, for tests.
    pub fn summary(&self) -> String {
        let mut lines = vec![format!("installed: {}", self.installed.as_deref().map_or("not installed".into(), |v| format!("v{v}")))];
        lines.push(match &self.latest {
            Latest::NotChecked => "latest: not checked".into(),
            Latest::Fetching(_) => "latest: checking".into(),
            Latest::Done(Ok(r)) => format!("latest: v{}", r.version),
            Latest::Done(Err(e)) => format!("latest: {e}"),
        });
        lines.push(format!("game: {}", self.game_version.map_or("unknown".into(), format_version)));
        lines.push(format!("compatibility: {}", self.compat_text()));
        if let Some(r) = self.release().filter(|r| is_update(self.installed.as_deref(), r)) {
            lines.push(format!("update: v{}", r.version));
        }
        lines.join("\n")
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog_sized(ctx, "script_hook", "Script Hook V", 520.0, false, |ui| self.contents(ui));
        if closed { Some(Request::Close) } else { request }
    }

    fn contents(&self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let dark = ui.visuals().dark_mode;
        egui::Grid::new("script-hook").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label("Installed");
            match &self.installed {
                Some(v) => ui.label(format!("v{v}")),
                None => ui.label(RichText::new("not installed").weak()),
            };
            ui.end_row();
            ui.label("Latest");
            match &self.latest {
                Latest::NotChecked => {
                    ui.label(RichText::new("not checked").weak());
                }
                Latest::Fetching(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking dev-c.com…");
                    });
                }
                Latest::Done(Ok(r)) => {
                    let supports = if r.supports.is_empty() { String::new() } else { format!(" ({})", r.supports) };
                    ui.label(format!("v{}{supports}", r.version));
                }
                Latest::Done(Err(e)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("Could not reach dev-c.com: {e}")).color(theme::warning(dark)));
                        if ui.button("Retry").clicked() {
                            request = Some(Request::Retry);
                        }
                    });
                }
            }
            ui.end_row();
            ui.label("Game version");
            ui.label(self.game_version.map_or("unknown".into(), format_version));
            ui.end_row();
        });
        ui.add_space(6.0);
        let (icon, color) = match &self.compat {
            Compat::Confirmed => ("✔", theme::ok(dark)),
            Compat::Likely => ("•", ui.visuals().weak_text_color()),
            Compat::Incompatible { .. } => ("✖", theme::error(dark)),
            Compat::Unknown => ("?", ui.visuals().weak_text_color()),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(icon).color(color));
            let text = self.compat_text();
            ui.label(RichText::new(if self.compat == Compat::Unknown { "Compatibility unknown".to_string() } else { capital(&text) }).color(color));
        });
        if let Some(r) = self.release().filter(|r| is_update(self.installed.as_deref(), r)) {
            ui.add_space(6.0);
            let label = if self.installed.is_some() { format!("Update to v{}", r.version) } else { format!("Install v{}", r.version) };
            if ui.add(crate::widgets::primary(ui, label)).on_hover_text("Downloads it from dev-c.com and replaces ScriptHookV.dll in the game folder, without a backup").clicked() {
                request = Some(Request::Update(r.clone()));
            }
        }
        ui.add_space(6.0);
        ui.hyperlink_to("dev-c.com/gtav/scripthookv", PAGE);
        request
    }
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Looks up the latest release on the author's page.
pub fn fetch(ctx: &egui::Context) -> Task<Result<Release, String>> {
    Task::spawn("Looking for the latest Script Hook V", ctx, || scripthook::latest().map_err(|e| e.to_string()))
}
