//! Likely causes of a game crash: the crash dump's reading, Script Hook V's
//! state, plugins older than the game and the plugins' own logs, put in one
//! list. Fast and local: no network, no archive scan.

use std::path::PathBuf;
use std::time::Duration;

use crate::Game;
use crate::crash_report::{CrashReport, LogFindings};
use crate::loader::{self, AsiPlugin};
use crate::scripthook::{self, Compat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Problem,
    Warning,
    Info,
}

/// What the user can do about a cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    UpdateScriptHook,
    /// Turn this part off (an `.asi` file name, see [`crate::crash_hunt::set_enabled`]).
    TurnOff(String),
    ModsInGame,
    /// Find the culprit by elimination (see [`crate::crash_hunt`]).
    Eliminate,
    OpenLog(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub severity: Severity,
    pub text: String,
    pub fix: Option<Fix>,
}

fn cause(severity: Severity, text: impl Into<String>, fix: Option<Fix>) -> Cause {
    Cause { severity, text: text.into(), fix }
}

/// The likely causes, most severe first and, within a severity, most relevant
/// first. `report` is the crash looked at (none when the game never crashed),
/// `logs` the plugins' logs (see [`crate::crash_report::plugin_logs`]).
pub fn causes(game: &Game, report: Option<&CrashReport>, logs: &[LogFindings]) -> Vec<Cause> {
    let mut out = Vec::new();
    let plugins = loader::asi_plugins(game);
    let installed = |name: &str| plugins.iter().find(|p| p.name.eq_ignore_ascii_case(name));
    let hook_updated = |out: &mut Vec<Cause>| {
        if let Compat::Incompatible { supports, game } = scripthook::compat(game) {
            let detail = if supports.is_empty() && game.is_empty() { String::new() } else { format!(" (it supports {supports}, the game is {game})") };
            out.push(cause(Severity::Problem, format!("Script Hook V does not suit this game version{detail}: every script mod depends on it."), Some(Fix::UpdateScriptHook)));
        }
    };
    hook_updated(&mut out);
    let mut points_at_mods = false;
    if let Some(r) = report {
        let exe = game.edition.exe_name().to_ascii_lowercase();
        let faulting = r.module.as_ref().map(|(n, _)| n.to_ascii_lowercase()).unwrap_or_default();
        if let Some(s) = r.stores.iter().find(|s| s.full()) {
            out.push(cause(Severity::Problem, format!("The game ran out of slots in its {} ({} of {}): the mods add more files of this kind than it allows.", s.name, s.used, s.size), None));
        }
        if let Some((mounted, limit)) = r.archives.filter(|(m, l)| m >= l) {
            out.push(cause(Severity::Problem, format!("The game reached its limit of {limit} archives ({mounted} mounted): raise it with a Packfile Limit Adjuster or remove mods that add archives."), Some(Fix::ModsInGame)));
        }
        let involved = r.plugins_involved();
        for name in &involved {
            let lower = name.to_ascii_lowercase();
            let inside = lower == faulting;
            let file: Option<&AsiPlugin> = installed(name);
            let old = file.is_some_and(|p| p.older_than_game);
            let severity = if inside || old { Severity::Problem } else { Severity::Warning };
            let place = if inside { "The crash happened inside" } else { "On the crashing thread's stack:" };
            let mut text = format!("{place} {name}.");
            if old {
                text += " It is older than the game's last update, so it may not suit this version.";
            }
            let fix = if lower.starts_with("scripthookv") {
                (lower == "scripthookv.dll").then_some(Fix::UpdateScriptHook)
            } else {
                file.map(|p| Fix::TurnOff(p.name.clone()))
            };
            out.push(cause(severity, text, fix));
        }
        let graphics = ["d3d12", "dxgi", "vkd3d", "dxvk", "amdxc", "nvwgf", "nvapi", "winevulkan", "vulkan", "amd_fidelityfx", "sl.", "nvngx"].iter().any(|g| faulting.contains(g));
        if involved.is_empty() && faulting == exe {
            out.push(cause(Severity::Warning, "The crash happened in the game's own code with no plugin on the stack: most often a mod's file the game cannot read (a model or texture made for another edition, a broken .meta).", Some(Fix::ModsInGame)));
            points_at_mods = true;
        } else if graphics {
            out.push(cause(Severity::Info, format!("The crash happened in {faulting}, part of the graphics stack: try other graphics settings (upscaler, frame generation) or another Proton version."), None));
        } else if involved.is_empty() && faulting.is_empty() {
            out.push(cause(Severity::Info, "The crash address is outside every module: often code a plugin patched at runtime.", None));
            points_at_mods = true;
        }
        if involved.len() > 1 {
            points_at_mods = true;
        }
    }
    let recent = |l: &LogFindings| match (report.and_then(|r| r.time), l.time) {
        (Some(crash), Some(log)) => log + Duration::from_secs(86_400) >= crash,
        _ => true,
    };
    for log in logs.iter().filter(|l| recent(l)).take(3) {
        let name = log.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = log.path.file_stem().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let plugin = plugins.iter().find(|p| p.name.to_ascii_lowercase().strip_suffix(".asi") == Some(stem.as_str()));
        let last = log.lines.last().map(|l| format!(": {l}")).unwrap_or_default();
        let who = plugin.map_or(name.clone(), |p| p.name.clone());
        out.push(cause(Severity::Warning, format!("{who} reports errors in {name}{last}"), Some(Fix::OpenLog(log.path.clone()))));
        if plugin.is_some() {
            points_at_mods = true;
        }
    }
    let single = out.iter().any(|c| c.severity == Severity::Problem && matches!(c.fix, Some(Fix::TurnOff(_)) | Some(Fix::UpdateScriptHook)));
    if report.is_some() && points_at_mods && !single {
        out.push(cause(Severity::Info, "No single mod stands out: turning mods off by halves finds the one that crashes the game.", Some(Fix::Eliminate)));
    }
    // Stable: relevance order kept within a severity.
    out.sort_by_key(|c| c.severity);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn report(module: &str, stack: &[&str]) -> CrashReport {
        CrashReport {
            dump: "a.dmp".into(),
            time: Some(SystemTime::now()),
            code: 0xC000_0005,
            address: 0x1_4000_1234,
            module: Some((module.into(), 0x1234)),
            access: None,
            stack: stack.iter().map(|s| s.to_string()).collect(),
            modules: Vec::new(),
            context: Vec::new(),
            same_place: 0,
            stores: Vec::new(),
            last_archive: None,
            archives: None,
        }
    }

    fn fake_game(dir: &std::path::Path, asi: &[&str]) -> Game {
        let root = dir.join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"").unwrap();
        for a in asi {
            std::fs::write(root.join(a), b"").unwrap();
        }
        Game::open(&root).unwrap()
    }

    #[test]
    fn nothing_without_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let game = fake_game(dir.path(), &[]);
        assert!(causes(&game, None, &[]).is_empty());
    }

    #[test]
    fn plugin_on_the_stack_can_be_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let game = fake_game(dir.path(), &["Trainer.asi"]);
        let c = causes(&game, Some(&report("GTA5_Enhanced.exe", &["trainer.asi"])), &[]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].severity, Severity::Warning);
        assert_eq!(c[0].fix, Some(Fix::TurnOff("Trainer.asi".into())));
    }

    #[test]
    fn old_faulting_plugin_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let game = fake_game(dir.path(), &["Trainer.asi"]);
        let old = game.root.join("Trainer.asi");
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(SystemTime::now() - Duration::from_secs(10_000)).unwrap();
        std::fs::File::options().write(true).open(game.root.join("GTA5_Enhanced.exe")).unwrap().set_modified(SystemTime::now() - Duration::from_secs(5_000)).unwrap();
        let c = causes(&game, Some(&report("Trainer.asi", &[])), &[]);
        assert_eq!(c[0].severity, Severity::Problem);
        assert!(c[0].text.contains("older than the game's last update"));
        assert_eq!(c[0].fix, Some(Fix::TurnOff("Trainer.asi".into())));
    }

    #[test]
    fn own_code_suggests_the_mods_and_elimination() {
        let dir = tempfile::tempdir().unwrap();
        let game = fake_game(dir.path(), &[]);
        let c = causes(&game, Some(&report("GTA5_Enhanced.exe", &[])), &[]);
        assert_eq!(c.iter().map(|c| c.fix.clone()).collect::<Vec<_>>(), [Some(Fix::ModsInGame), Some(Fix::Eliminate)]);
    }

    #[test]
    fn script_hook_that_fails_comes_first_and_logs_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let game = fake_game(dir.path(), &["Trainer.asi"]);
        std::fs::write(game.root.join("ScriptHookV.dll"), b"").unwrap();
        std::fs::write(game.root.join("ScriptHookV.log"), "[1] FATAL: unknown game version\n").unwrap();
        let log = LogFindings { path: game.root.join("Trainer.log"), time: None, lines: vec!["Error: bad".into()] };
        let c = causes(&game, Some(&report("GTA5_Enhanced.exe", &["Trainer.asi"])), std::slice::from_ref(&log));
        assert_eq!(c[0].fix, Some(Fix::UpdateScriptHook));
        assert_eq!(c[0].severity, Severity::Problem);
        assert!(c.iter().any(|c| c.fix == Some(Fix::OpenLog(log.path.clone())) && c.text.starts_with("Trainer.asi reports errors")));
        assert!(c.windows(2).all(|w| w[0].severity <= w[1].severity));
    }
}
