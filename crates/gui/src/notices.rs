//! Checks made in the background when the app starts, and the small dialogs
//! that report their findings one at a time: a Script Hook V update, an
//! incompatible Script Hook V, a crash since the last run.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};
use paleto_core::scripthook::{self, Compat, Release};
use paleto_core::{Game, crash_report, loader};

use crate::mods_check::age;
use crate::scripthook_window::{compat_now, incompatible_text, is_update};
use crate::task::Task;
use crate::theme;

pub enum Notice {
    /// A newer Script Hook V than the installed one.
    Update { release: Release, installed: Option<String> },
    /// The installed Script Hook V does not suit the game; the text is made when the notice comes up.
    Incompatible(String),
    /// A crash since the last run: when it happened (seconds since the epoch) and what it means.
    Crash { time: u64, verdict: String },
}

impl Notice {
    fn rank(&self) -> u8 {
        match self {
            Notice::Update { .. } => 0,
            Notice::Incompatible(_) => 1,
            Notice::Crash { .. } => 2,
        }
    }
}

/// What the user chose in a notice.
pub enum Reply {
    Update(Release),
    OpenScriptHook,
    OpenCrash,
    Dismiss,
}

struct Findings {
    release: Option<Release>,
    /// The newest crash dump's time.
    newest_crash: Option<u64>,
    crash: Option<(u64, String)>,
}

#[derive(Default)]
pub struct Startup {
    /// Whether to look on the internet (off in tests).
    pub online: bool,
    /// Asked to run, once a game is chosen.
    wanted: bool,
    started: bool,
    task: Option<Task<Findings>>,
    queue: VecDeque<Notice>,
    shown: Option<Notice>,
    /// The time of the newest crash already reported.
    pub last_seen_crash: Option<u64>,
    /// The latest Script Hook V release found.
    pub latest: Option<Release>,
    /// A Script Hook V zip to install instead of downloading one (tests).
    pub zip: Option<Vec<u8>>,
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn check(game: &Game, online: bool, last_seen: Option<u64>) -> Findings {
    let release = if online && loader::script_hook(game).is_some() { scripthook::latest().ok() } else { None };
    let newest = crash_report::history(game).into_iter().next();
    let newest_crash = newest.as_ref().and_then(|c| c.time.or_else(|| std::fs::metadata(&c.dump).and_then(|m| m.modified()).ok())).map(secs);
    let crash = match (newest, newest_crash, last_seen) {
        (Some(c), Some(time), Some(seen)) if time > seen => crash_report::report_of(game, &c.dump, None).ok().map(|r| (time, r.verdict(game.edition.exe_name()))),
        _ => None,
    };
    Findings { release, newest_crash, crash }
}

impl Startup {
    /// Asks for the checks; they run now if a game is chosen, else when one is.
    pub fn want(&mut self) {
        self.wanted = true;
    }

    /// Starts the checks once, when asked and a game is chosen.
    pub fn begin(&mut self, game: Option<&Game>, ctx: &egui::Context) {
        let (true, false, Some(game)) = (self.wanted, self.started, game) else { return };
        self.started = true;
        let (game, online, seen) = (game.clone(), self.online, self.last_seen_crash);
        self.task = Some(Task::spawn("Checking for news", ctx, move || check(&game, online, seen)));
    }

    /// Takes in the findings when the checks are done.
    pub fn poll(&mut self, game: Option<&Game>) {
        let Some(findings) = self.task.as_ref().and_then(Task::poll) else { return };
        self.task = None;
        if self.last_seen_crash.is_none() {
            // Crashes from now on are news, also when there is none yet.
            self.last_seen_crash = Some(findings.newest_crash.unwrap_or_else(|| secs(SystemTime::now())));
        }
        if let Some(release) = findings.release {
            self.offer_release(game, release);
        }
        // Whether the installed Script Hook V suits the game is judged when the notice comes up.
        self.enqueue(Notice::Incompatible(String::new()));
        if let Some((time, verdict)) = findings.crash {
            self.enqueue(Notice::Crash { time, verdict });
        }
    }

    /// Whether the checks are running.
    pub fn running(&self) -> bool {
        self.task.is_some()
    }

    /// A release was found: remembered, and announced if it is newer than the installed one.
    pub fn offer_release(&mut self, game: Option<&Game>, release: Release) {
        self.latest = Some(release.clone());
        let Some(game) = game.filter(|g| loader::script_hook(g).is_some()) else { return };
        let installed = scripthook::installed(game);
        if is_update(installed.as_deref(), &release) {
            self.enqueue(Notice::Update { release, installed });
        }
    }

    /// A crash newer than the last seen one was found.
    pub fn offer_crash(&mut self, time: SystemTime, verdict: String) {
        self.enqueue(Notice::Crash { time: secs(time), verdict });
    }

    fn enqueue(&mut self, notice: Notice) {
        if matches!(notice, Notice::Incompatible(_)) && self.queue.iter().any(|n| matches!(n, Notice::Incompatible(_))) {
            return;
        }
        let at = self.queue.iter().position(|n| n.rank() > notice.rank()).unwrap_or(self.queue.len());
        self.queue.insert(at, notice);
    }

    /// Brings up the next notice that still applies.
    pub fn promote(&mut self, game: Option<&Game>) {
        while self.shown.is_none() {
            match self.queue.pop_front() {
                None => return,
                Some(Notice::Incompatible(_)) => {
                    let Some(game) = game else { continue };
                    if let Compat::Incompatible { supports, game: version } = compat_now(game) {
                        let text = incompatible_text(&supports, &version, scripthook::installed(game).as_deref(), self.latest.as_ref());
                        self.shown = Some(Notice::Incompatible(text));
                    }
                }
                Some(notice) => self.shown = Some(notice),
            }
        }
    }

    /// Closes the notice on screen; a crash is marked as seen.
    pub fn dismiss(&mut self) {
        if let Some(Notice::Crash { time, .. }) = self.shown.take() {
            self.last_seen_crash = Some(self.last_seen_crash.map_or(time, |t| t.max(time)));
        }
        self.shown = None;
    }

    /// Whether a notice is on screen.
    pub fn open(&self) -> bool {
        self.shown.is_some()
    }

    /// The notice on screen, in words, for tests.
    pub fn text(&self) -> Option<String> {
        Some(match self.shown.as_ref()? {
            Notice::Update { release, installed } => update_text(release, installed.as_deref()),
            Notice::Incompatible(text) => format!("Script Hook V is not compatible with this game version\n{text}"),
            Notice::Crash { time, verdict } => format!("{}\n{verdict}", crash_text(*time)),
        })
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<Reply> {
        let notice = self.shown.as_ref()?;
        let title = match notice {
            Notice::Update { .. } => "Script Hook V update",
            Notice::Incompatible(_) => "Script Hook V is not compatible with this game version",
            Notice::Crash { .. } => "The game crashed",
        };
        let (reply, closed) = crate::widgets::dialog_sized(ctx, "notice", title, 480.0, false, |ui| {
            let dark = ui.visuals().dark_mode;
            let mut reply = None;
            match notice {
                Notice::Update { release, installed } => {
                    ui.label(update_text(release, installed.as_deref()));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(crate::widgets::primary(ui, "Update now")).on_hover_text("Replaces ScriptHookV.dll in the game folder, without a backup").clicked() {
                            reply = Some(Reply::Update(release.clone()));
                        }
                        if ui.button("Not now").clicked() {
                            reply = Some(Reply::Dismiss);
                        }
                    });
                }
                Notice::Incompatible(text) => {
                    ui.label(RichText::new(text).color(theme::error(dark)));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(crate::widgets::primary(ui, "Open Script Hook V")).clicked() {
                            reply = Some(Reply::OpenScriptHook);
                        }
                        if ui.button("Close").clicked() {
                            reply = Some(Reply::Dismiss);
                        }
                    });
                }
                Notice::Crash { time, verdict } => {
                    ui.label(RichText::new(crash_text(*time)).strong());
                    ui.label(RichText::new(verdict).color(theme::warning(dark)));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(crate::widgets::primary(ui, "Open Crash detection")).clicked() {
                            reply = Some(Reply::OpenCrash);
                        }
                        if ui.button("Dismiss").clicked() {
                            reply = Some(Reply::Dismiss);
                        }
                    });
                }
            }
            reply
        });
        if closed { Some(Reply::Dismiss) } else { reply }
    }
}

fn update_text(release: &Release, installed: Option<&str>) -> String {
    format!("Script Hook V v{} is available (installed: {}).", release.version, installed.map_or("unknown version".to_string(), |v| format!("v{v}")))
}

fn crash_text(time: u64) -> String {
    let ago = SystemTime::now().duration_since(UNIX_EPOCH + std::time::Duration::from_secs(time)).map(|d| age(d.as_secs())).unwrap_or_else(|_| "0 s".into());
    format!("The game crashed {ago} ago")
}
