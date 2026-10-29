//! Checks made in the background when the app starts, and the small dialogs
//! that report their findings one at a time: a Script Hook V update, an
//! incompatible Script Hook V, a crash since the last run, a change that an
//! earlier run left interrupted.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};
use paleto_core::scripthook::{self, Compat, Release};
use paleto_core::txn::{Pending, PendingState};
use paleto_core::{Game, crash_report, loader};

use crate::mods_check::age;
use crate::scripthook_window::{compat_now, incompatible_text, is_update};
use crate::task::Task;
use crate::theme;

pub enum Notice {
    /// Changes an earlier run left half done: they are undone, or set aside, before anything else is changed.
    Interrupted(Vec<Pending>),
    /// What undoing or setting aside an interrupted change did: one line each, and whether something is left.
    Recovered { title: &'static str, lines: Vec<String>, failed: bool },
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
            Notice::Interrupted(_) | Notice::Recovered { .. } => 0,
            Notice::Update { .. } => 1,
            Notice::Incompatible(_) => 2,
            Notice::Crash { .. } => 3,
        }
    }
}

/// What the user chose in a notice.
pub enum Reply {
    Update(Release),
    OpenScriptHook,
    OpenCrash,
    /// Undo the interrupted changes.
    Undo,
    /// Ask before setting these journals (by id) aside.
    AskAbandon,
    /// Set these journals aside, after the confirmation.
    Abandon(Vec<String>),
    /// Back from the confirmation to the list.
    Back,
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
    /// The interrupted-change notice asks whether to set the stuck journals aside.
    confirming: bool,
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

    /// Changes an earlier run left interrupted were found in the game.
    pub fn offer_interrupted(&mut self, pending: Vec<Pending>) {
        if pending.is_empty() || matches!(self.shown, Some(Notice::Interrupted(_))) {
            return;
        }
        self.queue.retain(|n| !matches!(n, Notice::Interrupted(_)));
        self.enqueue(Notice::Interrupted(pending));
    }

    /// What undoing or setting aside did, to be read before anything else.
    pub fn offer_report(&mut self, title: &'static str, lines: Vec<String>, failed: bool) {
        self.enqueue(Notice::Recovered { title, lines, failed });
    }

    /// Drops the notices about interrupted changes (another game was chosen).
    pub fn forget_interrupted(&mut self) {
        self.queue.retain(|n| !matches!(n, Notice::Interrupted(_) | Notice::Recovered { .. }));
        if matches!(self.shown, Some(Notice::Interrupted(_) | Notice::Recovered { .. })) {
            self.shown = None;
        }
        self.confirming = false;
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

    /// Switches the interrupted-change notice between its list and the question before setting journals aside.
    pub fn confirm_abandon(&mut self, on: bool) {
        self.confirming = on;
    }

    /// Closes the notice on screen; a crash is marked as seen.
    pub fn dismiss(&mut self) {
        self.confirming = false;
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
            Notice::Interrupted(pending) if self.confirming => abandon_text(pending),
            Notice::Interrupted(pending) => interrupted_text(pending),
            Notice::Recovered { title, lines, .. } => format!("{title}\n{}", lines.join("\n")),
            Notice::Update { release, installed } => update_text(release, installed.as_deref()),
            Notice::Incompatible(text) => format!("Script Hook V is not compatible with this game version\n{text}"),
            Notice::Crash { time, verdict } => format!("{}\n{verdict}", crash_text(*time)),
        })
    }

    pub fn show(&self, ctx: &egui::Context) -> Option<Reply> {
        let notice = self.shown.as_ref()?;
        let title = match notice {
            Notice::Interrupted(_) if self.confirming => "Set the interrupted change aside?",
            Notice::Interrupted(_) => "A change was interrupted",
            Notice::Recovered { title, .. } => title,
            Notice::Update { .. } => "Script Hook V update",
            Notice::Incompatible(_) => "Script Hook V is not compatible with this game version",
            Notice::Crash { .. } => "The game crashed",
        };
        let (reply, closed) = crate::widgets::dialog_sized(ctx, "notice", title, 480.0, false, |ui| {
            let dark = ui.visuals().dark_mode;
            let mut reply = None;
            match notice {
                Notice::Interrupted(pending) if self.confirming => {
                    ui.label(abandon_text(pending));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(crate::widgets::primary(ui, "Set aside")).on_hover_text("Moves the journal to the folder `abandoned`; no file is deleted or put back").clicked() {
                            reply = Some(Reply::Abandon(stuck(pending).map(|p| p.journal.clone()).collect()));
                        }
                        if ui.button("Back").clicked() {
                            reply = Some(Reply::Back);
                        }
                    });
                }
                Notice::Interrupted(pending) => {
                    ui.label(interrupted_text(pending));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(crate::widgets::primary(ui, "Undo")).on_hover_text("Puts the files back as they were, or finishes the clean-up if every file was already in place").clicked() {
                            reply = Some(Reply::Undo);
                        }
                        if ui.button("Later").on_hover_text("Changes to this game are refused until it is undone").clicked() {
                            reply = Some(Reply::Dismiss);
                        }
                        if stuck(pending).next().is_some() && ui.button("Abandon…").on_hover_text("For a change that cannot be undone: it stops blocking, nothing is deleted").clicked() {
                            reply = Some(Reply::AskAbandon);
                        }
                    });
                }
                Notice::Recovered { lines, failed, .. } => {
                    let dark = ui.visuals().dark_mode;
                    let text = lines.join("\n");
                    ui.label(if *failed { RichText::new(text).color(theme::error(dark)) } else { RichText::new(text) });
                    ui.add_space(8.0);
                    if ui.add(crate::widgets::primary(ui, "OK")).clicked() {
                        reply = Some(Reply::Dismiss);
                    }
                }
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
        if closed { Some(if self.confirming { Reply::Back } else { Reply::Dismiss }) } else { reply }
    }
}

/// The journals that Undo cannot be expected to settle: a rollback that got stuck, or a journal nobody can read.
fn stuck(pending: &[Pending]) -> impl Iterator<Item = &Pending> {
    pending.iter().filter(|p| matches!(p.state, PendingState::Stuck | PendingState::Unreadable(_)))
}

fn interrupted_text(pending: &[Pending]) -> String {
    let mut text = String::from("An earlier change to this game was interrupted (a crash, a power cut or the program was killed). Changes to this game are refused until it is undone.");
    for p in pending {
        let label = if p.label.is_empty() { "(unnamed change)" } else { p.label.as_str() };
        let state = match &p.state {
            PendingState::Interrupted => "stopped half way: Undo puts the files back as they were".to_string(),
            PendingState::Committed => "every file was in place: Undo only removes the backups left over".to_string(),
            PendingState::Stuck => "an undo got stuck: Undo tries again".to_string(),
            PendingState::Unreadable(why) => format!("its journal cannot be read ({why}): it can only be set aside"),
        };
        text.push_str(&format!("\n\n{label} (journal {}): {state}", p.journal));
        for f in &p.files {
            text.push_str(&format!("\n    {}", f.display()));
        }
    }
    text
}

fn abandon_text(pending: &[Pending]) -> String {
    let mut text = String::from("The change below will stop blocking this game. Nothing is deleted or put back: its journal is moved to the folder `abandoned`, and the files and backups it names stay where they are.");
    for p in stuck(pending) {
        text.push_str(&format!("\n\n{} (journal {})", if p.label.is_empty() { "(unnamed change)" } else { p.label.as_str() }, p.journal));
        for f in &p.files {
            text.push_str(&format!("\n    {}", f.display()));
        }
    }
    text
}

fn update_text(release: &Release, installed: Option<&str>) -> String {
    format!("Script Hook V v{} is available (installed: {}).", release.version, installed.map_or("unknown version".to_string(), |v| format!("v{v}")))
}

fn crash_text(time: u64) -> String {
    let ago = SystemTime::now().duration_since(UNIX_EPOCH + std::time::Duration::from_secs(time)).map(|d| age(d.as_secs())).unwrap_or_else(|_| "0 s".into());
    format!("The game crashed {ago} ago")
}
