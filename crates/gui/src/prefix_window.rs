//! The "Prefix tweaks" window: changes to the game's Wine prefix that mods
//! need. For now one, .NET Framework 4.8, which ScriptHookVDotNet mods run on.
//! The work takes minutes, so it runs here, on its own thread, with a live log.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use paleto_core::prefix::{self, Backup, DotnetState, Needed, Options, Prefix, Run};
use paleto_core::{Error, Game};

use crate::mods_check::age;
use crate::theme;

/// How long the "copied" icon stays, and the "refused" and "granted" ones.
const COPIED_SHOWN: Duration = Duration::from_secs(3);
const REFUSED_SHOWN: Duration = Duration::from_secs(4);
/// A granted permission stays on screen this long before the tweaks open.
const GRANTED_SHOWN: Duration = Duration::from_millis(1200);

/// The icons of the permission gate. They are drawn, not typed: no font of the
/// interface has a clipboard.
#[derive(Clone, Copy)]
enum Mark {
    Copied,
    Granted,
    Refused,
}

fn paint_mark(ui: &egui::Ui, rect: egui::Rect, mark: Mark, color: egui::Color32) {
    let painter = ui.painter();
    let c = rect.center();
    let s = rect.height() * 0.3;
    let check = |o: egui::Vec2, k: f32, width: f32| {
        painter.line(
            vec![c + o + egui::vec2(-0.7 * k * s, 0.0), c + o + egui::vec2(-0.2 * k * s, 0.6 * k * s), c + o + egui::vec2(0.8 * k * s, -0.6 * k * s)],
            egui::Stroke::new(width, color),
        );
    };
    match mark {
        Mark::Copied => {
            // A clipboard: its board, the clip on top, and a check on the board.
            let board = egui::Rect::from_center_size(c + egui::vec2(0.0, 0.12 * s), egui::vec2(1.7 * s, 2.0 * s));
            painter.rect_stroke(board, 2.0, egui::Stroke::new(1.6, color), egui::StrokeKind::Inside);
            let clip = egui::Rect::from_center_size(egui::pos2(board.center().x, board.top()), egui::vec2(0.9 * s, 0.5 * s));
            painter.rect_filled(clip, 1.5, color);
            check(egui::vec2(0.0, 0.12 * s), 0.6, 1.6);
        }
        Mark::Granted => check(egui::Vec2::ZERO, 1.0, 2.2),
        Mark::Refused => {
            let k = 0.8 * s;
            painter.line_segment([c + egui::vec2(-k, -k), c + egui::vec2(k, k)], egui::Stroke::new(2.2, color));
            painter.line_segment([c + egui::vec2(k, -k), c + egui::vec2(-k, k)], egui::Stroke::new(2.2, color));
        }
    }
}

/// What the prefix looks like now.
struct Found {
    prefix: Prefix,
    dotnet: DotnetState,
    /// The game, or Wine, is using the prefix.
    in_use: bool,
    backups: Vec<Backup>,
    /// What installing .NET downloads, and what is already downloaded.
    needed: Vec<Needed>,
    /// A backup is a reflink copy (btrfs, XFS): instant and free.
    shared: bool,
    /// What a backup takes where copies are full ones.
    backup_gb: f64,
}

/// A running operation: its log so far, how to stop it, and its end.
struct Job {
    title: String,
    log: Arc<Mutex<Vec<String>>>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<String, String>>,
    started: Instant,
}

enum Do {
    Install,
    BackupNow,
    Restore(String),
    Delete(String),
    Refresh,
    /// "Check again" in the permission gate.
    Check,
}

pub struct PrefixWindow {
    game: Game,
    found: Result<Found, String>,
    /// Why nothing here can run, while the Flatpak is not allowed to run Wine on the host.
    /// Until it is allowed, the window shows only how to allow it.
    gate: Option<&'static str>,
    /// When the command was copied, and when the permission was last checked (and found).
    copied_at: Option<Instant>,
    checked_at: Option<(Instant, bool)>,
    job: Option<Job>,
    confirm_install: bool,
    /// Back the prefix up before installing.
    backup_first: bool,
    /// The backup whose restore awaits confirmation.
    confirm_restore: Option<String>,
    /// How the last operation ended.
    outcome: Option<Result<String, String>>,
}

fn message(e: Error) -> String {
    match e {
        Error::Prefix(m) => m,
        e => e.to_string(),
    }
}

fn gather(game: &Game) -> Result<Found, String> {
    let prefix = prefix::find(game).map_err(message)?;
    let shared = paleto_core::snapshots::shares_copies(&prefix.path);
    Ok(Found {
        dotnet: prefix::dotnet_state(&prefix),
        in_use: prefix::is_running(&prefix),
        backups: prefix::backups(&prefix),
        needed: prefix::needed_downloads(),
        backup_gb: if shared {
            0.0
        } else {
            paleto_core::snapshots::size(&prefix.path) as f64 / 1e9
        },
        shared,
        prefix,
    })
}

impl PrefixWindow {
    pub fn gather(game: &Game) -> Self {
        Self {
            game: game.clone(),
            found: gather(game),
            gate: prefix::current_sandbox_problem(),
            copied_at: None,
            checked_at: None,
            job: None,
            confirm_install: false,
            backup_first: true,
            confirm_restore: None,
            outcome: None,
        }
    }

    /// The prefix could not be found, or what is wrong with it, for tests.
    pub fn summary(&self) -> String {
        match &self.found {
            Ok(f) => format!("{}; {} backups", f.dotnet.describe(), f.backups.len()),
            Err(e) => e.clone(),
        }
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        title: &str,
        done: &str,
        f: impl FnOnce(&mut Run) -> Result<(), Error> + Send + 'static,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (thread_log, thread_cancel, ctx, done) =
            (log.clone(), cancel.clone(), ctx.clone(), done.to_string());
        std::thread::spawn(move || {
            let repaint = ctx.clone();
            let mut say = |line: &str| {
                thread_log.lock().unwrap().push(line.to_string());
                repaint.request_repaint();
            };
            let mut run = Run {
                log: &mut say,
                cancel: &thread_cancel,
            };
            let result = f(&mut run).map(|()| done).map_err(message);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.outcome = None;
        self.confirm_install = false;
        self.confirm_restore = None;
        self.job = Some(Job {
            title: title.to_string(),
            log,
            cancel,
            rx,
            started: Instant::now(),
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        match job.rx.try_recv() {
            Ok(result) => {
                // A cancelled installation can leave the prefix half changed.
                let result = result.map_err(|m| {
                    if m == "cancelled" { "Cancelled. If the prefix was already changed, install again or restore a backup.".to_string() } else { m }
                });
                self.outcome = Some(result);
                self.job = None;
                self.found = gather(&self.game);
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(500)),
        }
    }

    /// Shows the window; true when it is closed.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        self.poll(ctx);
        if let Some((at, true)) = self.checked_at
            && at.elapsed() >= GRANTED_SHOWN
        {
            self.gate = None;
            self.checked_at = None;
            self.found = gather(&self.game);
            self.outcome = Some(Ok("Permission granted: Prefix tweaks are available".to_string()));
        }
        // The permission gate is short: it is only as tall as its text.
        let fill = self.gate.is_none();
        let (action, closed) =
            crate::widgets::dialog_sized(ctx, "prefix_tweaks", "Prefix tweaks", 580.0, fill, |ui| {
                self.contents(ui)
            });
        match action {
            Some(Do::Refresh) => {
                self.gate = prefix::current_sandbox_problem();
                self.found = gather(&self.game);
                self.outcome = None;
            }
            Some(Do::Check) => match prefix::current_sandbox_problem() {
                // The gate stays up for a moment with the check mark, then opens.
                None => self.checked_at = Some((Instant::now(), true)),
                Some(_) => self.checked_at = Some((Instant::now(), false)),
            },
            Some(Do::Delete(id)) => {
                if let Ok(f) = &self.found {
                    self.outcome = Some(
                        prefix::delete_backup(&f.prefix, &id)
                            .map(|()| "backup deleted".to_string())
                            .map_err(message),
                    );
                }
                self.found = gather(&self.game);
            }
            Some(Do::Install) => {
                if let Ok(f) = &self.found {
                    let (prefix, options) = (
                        f.prefix.clone(),
                        Options {
                            backup: self.backup_first,
                        },
                    );
                    self.start(
                        ctx,
                        ".NET Framework 4.8",
                        ".NET Framework 4.8 is installed: ScriptHookVDotNet mods can run",
                        move |run| prefix::install_dotnet(&prefix, &options, run),
                    );
                }
            }
            Some(Do::BackupNow) => {
                if let Ok(f) = &self.found {
                    let prefix = f.prefix.clone();
                    self.start(
                        ctx,
                        "Backing the prefix up",
                        "prefix backed up",
                        move |run| {
                            run.say("Copying the prefix");
                            prefix::backup(&prefix, "manual backup").map(|_| ())
                        },
                    );
                }
            }
            Some(Do::Restore(id)) => {
                if let Ok(f) = &self.found {
                    let prefix = f.prefix.clone();
                    self.start(
                        ctx,
                        "Restoring the prefix",
                        "prefix restored from the backup",
                        move |run| {
                            run.say(
                                "Copying the backup back (the current prefix is backed up first)",
                            );
                            prefix::restore(&prefix, &id)
                        },
                    );
                }
            }
            None => {}
        }
        // Closing is not allowed while the prefix is being changed.
        closed && self.job.is_none()
    }

    /// What the window shows instead of the tweaks while the permission is missing.
    fn needs_permission(&mut self, ui: &mut egui::Ui) -> Option<Do> {
        let dark = ui.visuals().dark_mode;
        let mut request = None;
        let command = format!(
            "flatpak override --user --talk-name=org.freedesktop.Flatpak {}",
            crate::APP_ID
        );
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Prefix tweaks need a permission first").strong());
            ui.label("Wine runs on the host, and this Flatpak is not allowed to run commands there. Allow it once, in a terminal:");
            ui.add_space(4.0);
            ui.label(RichText::new(&command).monospace());
            ui.add_space(4.0);
            // Each button has an icon slot beside it. The slots are always there, so the
            // icon appearing does not move anything.
            let height = ui.spacing().interact_size.y;
            let padding = ui.spacing().button_padding.x * 2.0;
            let gap = ui.spacing().item_spacing.x;
            let text_width = |ui: &egui::Ui, text: &str| {
                egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + padding
            };
            let row = height + gap + text_width(ui, "Copy command") + gap + text_width(ui, "Check again") + gap + height;
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - row) / 2.0).max(0.0));
                let (left, _) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::hover());
                if let Some(at) = self.copied_at.filter(|at| at.elapsed() < COPIED_SHOWN) {
                    paint_mark(ui, left, Mark::Copied, theme::ok(dark));
                    ui.ctx().request_repaint_after(COPIED_SHOWN - at.elapsed());
                }
                if ui.button("Copy command").clicked() {
                    ui.ctx().copy_text(command.clone());
                    self.copied_at = Some(Instant::now());
                }
                if ui.button("Check again").clicked() {
                    request = Some(Do::Check);
                }
                let (right, _) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::hover());
                if let Some((at, granted)) = self.checked_at {
                    let shown = if granted { GRANTED_SHOWN } else { REFUSED_SHOWN };
                    if at.elapsed() < shown {
                        let mark = if granted { Mark::Granted } else { Mark::Refused };
                        let color = if granted { theme::ok(dark) } else { theme::error(dark) };
                        paint_mark(ui, right, mark, color);
                        ui.ctx().request_repaint_after(shown - at.elapsed());
                    }
                }
            });
            ui.add_space(4.0);
            crate::widgets::hint(
                ui,
                "A permission applies when the app starts: after allowing it, close and reopen Paleto Modkit, then check again. Until it is allowed, nothing in this window can run. To take it back, run the same command with --no-talk-name in place of --talk-name.",
            );
        });
        request
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<Do> {
        if self.gate.is_some() {
            return self.needs_permission(ui);
        }
        let dark = ui.visuals().dark_mode;
        let found = match &self.found {
            Ok(found) => found,
            Err(why) => {
                ui.label(
                    RichText::new("The game's Wine prefix cannot be changed from here.").strong(),
                );
                ui.label(why);
                ui.add_space(4.0);
                crate::widgets::hint(
                    ui,
                    "Prefix tweaks need the game installed through Steam and started once with Proton.",
                );
                return None;
            }
        };
        let mut request = None;
        let busy = self.job.is_some();

        ui.label("Changes to the game's Wine prefix that some mods need. They never touch the game's files or the mods folder.");
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Prefix").weak());
            ui.label(found.prefix.path.display().to_string());
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Proton").weak());
            ui.label(&found.prefix.proton.name);
        });

        crate::widgets::section(ui, ".NET Framework 4.8");
        ui.label("ScriptHookVDotNet and the mods written for it (scripts/*.dll) run on .NET. Paleto installs it in the prefix; ScriptHookVDotNet and the mods themselves you add yourself.");
        ui.add_space(4.0);
        let state = &found.dotnet;
        ui.horizontal_wrapped(|ui| {
            match (state.installed(), state.windows_ok()) {
                (true, true) => ui.label(RichText::new("✔").color(theme::ok(dark))),
                (true, false) => ui.label(RichText::new("⚠").color(theme::warning(dark))),
                (false, _) => ui.label(RichText::new("•").weak()),
            };
            ui.label(state.describe());
        });
        if !state.windows_ok() {
            crate::widgets::hint(
                ui,
                "A failed install can leave the prefix set to an old Windows. Installing again sets it back.",
            );
        }
        ui.add_space(4.0);
        let label = if state.installed() && state.windows_ok() {
            "Reinstall…"
        } else if state.installed() {
            "Repair…"
        } else {
            "Install .NET Framework 4.8…"
        };
        if prefix::on_host() {
            crate::widgets::hint(ui, "In the Flatpak, Wine runs on the host, through flatpak-spawn.");
        }
        ui.horizontal(|ui| {
            let enabled = !busy && !found.in_use;
            let mut button = ui.add_enabled(enabled, crate::widgets::primary(ui, label));
            if found.in_use {
                button = button.on_disabled_hover_text(
                    "The game (or Wine) is using the prefix: close it first",
                );
            }
            if button.clicked() {
                self.confirm_install = true;
            }
            if found.in_use {
                ui.label(
                    RichText::new("The game is using the prefix: close it first.")
                        .color(theme::warning(dark)),
                );
            }
        });
        if self.confirm_install && !busy {
            ui.separator();
            let missing: Vec<&Needed> = found.needed.iter().filter(|n| !n.cached).collect();
            if missing.is_empty() {
                ui.label("The installers are already downloaded.");
            } else {
                let mb = missing
                    .iter()
                    .map(|n| n.size)
                    .sum::<u64>()
                    .div_ceil(1 << 20);
                ui.label(format!(
                    "This downloads {mb} MB from Microsoft and GitHub, once: {}.",
                    missing
                        .iter()
                        .map(|n| n.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            ui.label(RichText::new("It changes the game's Wine prefix and takes a few minutes. Close the game first.").color(theme::warning(dark)));
            ui.checkbox(
                &mut self.backup_first,
                "Back the prefix up first (recommended)",
            );
            crate::widgets::hint(
                ui,
                if found.shared {
                    "On this filesystem a backup is instant and takes no space until files change."
                        .to_string()
                } else {
                    format!(
                        "On this filesystem a backup is a full copy: it takes {:.1} GB.",
                        found.backup_gb
                    )
                },
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!found.in_use, crate::widgets::primary(ui, "Start"))
                    .clicked()
                {
                    request = Some(Do::Install);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_install = false;
                }
            });
        }

        if let Some(job) = &self.job {
            ui.separator();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(&job.title).strong());
                ui.label(
                    RichText::new(format!("{} so far", age(job.started.elapsed().as_secs())))
                        .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cancelling = job.cancel.load(Ordering::Relaxed);
                    if ui
                        .add_enabled(
                            !cancelling,
                            egui::Button::new(if cancelling {
                                "Cancelling…"
                            } else {
                                "Cancel"
                            }),
                        )
                        .clicked()
                    {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("prefix_log")
                .max_height(200.0)
                .stick_to_bottom(true)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for line in job.log.lock().unwrap().iter() {
                        ui.label(RichText::new(line).monospace().size(12.0));
                    }
                });
        } else if let Some(outcome) = &self.outcome {
            ui.separator();
            match outcome {
                Ok(text) => ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("✔").color(theme::ok(dark)));
                    ui.label(text);
                }),
                Err(text) => ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("✖").color(theme::error(dark)));
                    ui.label(text);
                }),
            };
        }

        crate::widgets::section(ui, "Backups of the prefix");
        ui.label("A backup keeps the whole prefix as it is now. If a change breaks the game, restore it to go back.");
        ui.add_space(4.0);
        if ui
            .add_enabled(!busy && !found.in_use, egui::Button::new("Back up now"))
            .clicked()
        {
            request = Some(Do::BackupNow);
        }
        if found.backups.is_empty() {
            ui.label(RichText::new("No backups yet.").weak());
        }
        for b in &found.backups {
            ui.horizontal(|ui| {
                let ago = b
                    .time()
                    .and_then(|t| t.elapsed().ok())
                    .map(|d| format!("{} ago", age(d.as_secs())))
                    .unwrap_or_default();
                ui.label(if b.label.is_empty() {
                    RichText::new("(no note)").weak()
                } else {
                    RichText::new(&b.label)
                });
                ui.label(RichText::new(ago).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Delete"))
                        .on_hover_text("Delete this backup; the prefix stays as it is")
                        .clicked()
                    {
                        request = Some(Do::Delete(b.id.clone()));
                    }
                    if ui
                        .add_enabled(!busy && !found.in_use, egui::Button::new("Restore…"))
                        .on_hover_text("Put the prefix back as it was then")
                        .clicked()
                    {
                        self.confirm_restore = Some(b.id.clone());
                    }
                });
            });
        }
        if let Some(id) = self.confirm_restore.clone() {
            ui.separator();
            ui.label(RichText::new("The prefix will be replaced by this backup. The current one is backed up first.").color(theme::warning(dark)));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy, crate::widgets::danger(ui, "Replace the prefix"))
                    .clicked()
                {
                    request = Some(Do::Restore(id));
                    self.confirm_restore = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_restore = None;
                }
            });
        }
        ui.add_space(6.0);
        if ui
            .add_enabled(!busy, egui::Button::new("Refresh"))
            .on_hover_text("Look at the prefix again")
            .clicked()
        {
            request = Some(Do::Refresh);
        }
        request
    }
}
