//! Programs outside the app: the file manager, for a folder, and the program the user chose for exported models (Blender,
//! an editor). In a Flatpak a folder opens through the desktop's portal, which needs no permission; a program runs on the
//! host, which needs the permission to talk to `org.freedesktop.Flatpak` (the one the prefix tweaks need too).

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::prefix;

/// Where programs run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Here: not in a Flatpak.
    Here,
    /// In a Flatpak, on the host, if it is `allowed` to start programs there.
    Flatpak { allowed: bool },
}

impl Host {
    /// Where programs run now (asks the host in a Flatpak: call it when about to run one, not every frame).
    pub fn current() -> Self {
        if prefix::on_host() { Host::Flatpak { allowed: prefix::current_sandbox_problem().is_none() } } else { Host::Here }
    }
}

/// What a Flatpak without the permission says instead of starting a program.
pub const NO_HOST: &str = "This Flatpak may not start programs on the host. Allow it with: flatpak override --user --talk-name=org.freedesktop.Flatpak io.github.the_beper.PaletoModkit (the AppImage and the tarball need nothing)";

/// The program Blender is started by, when no other is set.
const BLENDER_FLATPAK: &str = "org.blender.Blender";

/// Opens `dir` in the file manager, without waiting: `xdg-open`, which in a Flatpak asks the desktop through its portal.
pub fn open_folder(dir: &Path) -> Result<(), String> {
    spawn(Command::new("xdg-open").arg(dir)).map_err(|e| format!("xdg-open {}: {e}", dir.display()))
}

/// Starts the user's program for an exported model, without waiting. `command` is a command line (words in quotes stay
/// one word) in which `{file}` stands for `file` and `{dir}` for its folder, each one word however many spaces it holds.
pub fn run_program(host: Host, command: &str, file: &Path) -> Result<(), String> {
    let args = command_line(command, file)?;
    let Some((program, rest)) = args.split_first().filter(|(program, _)| !program.is_empty()) else { return Err("No program is set".into()) };
    let shown = program.to_string_lossy();
    let mut cmd = Command::new(program);
    cmd.args(rest);
    let mut cmd = match host {
        Host::Flatpak { allowed: false } => return Err(NO_HOST.into()),
        Host::Flatpak { allowed: true } if !on_host_path(program) => return Err(format!("{shown} is not installed on the host")),
        Host::Flatpak { allowed: true } => prefix::on_host_command(&cmd),
        Host::Here if find(program).is_none() => return Err(format!("{shown} is not installed")),
        Host::Here => cmd,
    };
    spawn(&mut cmd).map_err(|e| format!("{shown}: {e}"))
}

/// The command line that starts Blender, if it is installed: `blender` from the PATH, else its Flatpak. Blender gets no
/// file: the XML is imported from inside it, with the Sollumz add-on.
pub fn find_blender(host: Host) -> Option<String> {
    let flatpak = format!("flatpak run {BLENDER_FLATPAK}");
    match host {
        Host::Flatpak { allowed: false } => None,
        Host::Flatpak { allowed: true } => {
            let blender = OsString::from("blender");
            if on_host_path(&blender) {
                return Some("blender".into());
            }
            succeeds(&mut prefix::on_host_command(Command::new("flatpak").args(["info", BLENDER_FLATPAK]))).then_some(flatpak)
        }
        Host::Here => {
            if find(&OsString::from("blender")).is_some() {
                return Some("blender".into());
            }
            (find(&OsString::from("flatpak")).is_some() && succeeds(Command::new("flatpak").args(["info", BLENDER_FLATPAK]))).then_some(flatpak)
        }
    }
}

/// The words of `command`, with `{file}` and `{dir}` put in.
fn command_line(command: &str, file: &Path) -> Result<Vec<OsString>, String> {
    let dir = file.parent().unwrap_or(Path::new("."));
    Ok(words(command)?
        .into_iter()
        .map(|word| match word.as_str() {
            "{file}" => file.as_os_str().to_owned(),
            "{dir}" => dir.as_os_str().to_owned(),
            _ => OsString::from(word.replace("{file}", &file.to_string_lossy()).replace("{dir}", &dir.to_string_lossy())),
        })
        .collect())
}

/// `line` split at spaces, except inside double or single quotes (which are taken away).
fn words(line: &str) -> Result<Vec<String>, String> {
    let (mut out, mut word, mut quote, mut started) = (Vec::new(), String::new(), None, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => (quote, started) = (Some(c), true),
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (None, c) => (word, started) = (word + &c.to_string(), true),
        }
    }
    if quote.is_some() {
        return Err(format!("A quote is not closed in: {line}"));
    }
    if started {
        out.push(word);
    }
    Ok(out)
}

/// `program` as it would be run here: itself if it names a path, else the first in the PATH; an executable file.
fn find(program: &OsString) -> Option<PathBuf> {
    let executable = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable(path).then(|| path.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(program)).find(|p| executable(p))
}

/// Whether the host would find `program` (asked through `flatpak-spawn`).
fn on_host_path(program: &OsString) -> bool {
    succeeds(&mut prefix::on_host_command(Command::new("sh").args(["-c", "command -v -- \"$1\" >/dev/null", "sh"]).arg(program)))
}

fn succeeds(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Starts `cmd` on its own; a thread waits for it, so that it leaves no zombie behind.
fn spawn(cmd: &mut Command) -> std::io::Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_keeps_quoted_words_and_paths_with_spaces_whole() {
        let file = Path::new("/home/me/My Exports/car.yft.xml");
        let args = command_line(r#"'/opt/My Blender/blender' --python-expr "print('hi')" {file} --dir={dir}"#, file).unwrap();
        assert_eq!(args, ["/opt/My Blender/blender", "--python-expr", "print('hi')", "/home/me/My Exports/car.yft.xml", "--dir=/home/me/My Exports"]);
        assert_eq!(words("  kate   ").unwrap(), ["kate"]);
        assert_eq!(words(r#"a "" b"#).unwrap(), ["a", "", "b"]);
        assert!(words("kate \"oops").unwrap_err().contains("not closed"));
    }

    #[test]
    fn the_program_set_gets_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args.txt");
        let editor = dir.path().join("my editor");
        crate::write_script(&editor, &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display()));
        let file = dir.path().join("exports with spaces/car.yft.xml");
        run_program(Host::Here, &format!("'{}' --open {{file}}", editor.display()), &file).unwrap();
        for _ in 0..500 {
            if std::fs::read_to_string(&out).is_ok_and(|s| s.ends_with('\n')) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), format!("--open\n{}\n", file.display()));
    }

    #[test]
    fn a_missing_program_or_a_flatpak_without_the_permission_is_a_message() {
        let file = Path::new("/tmp/x.xml");
        assert_eq!(run_program(Host::Here, "paleto-no-such-editor {file}", file).unwrap_err(), "paleto-no-such-editor is not installed");
        assert_eq!(run_program(Host::Here, "/nowhere/editor", file).unwrap_err(), "/nowhere/editor is not installed");
        assert_eq!(run_program(Host::Here, "   ", file).unwrap_err(), "No program is set");
        assert_eq!(run_program(Host::Here, "'' {file}", file).unwrap_err(), "No program is set");
        assert_eq!(run_program(Host::Flatpak { allowed: false }, "blender", file).unwrap_err(), NO_HOST);
        assert_eq!(find_blender(Host::Flatpak { allowed: false }), None);
    }
}
