//! When the Legacy -> Enhanced converter is missing, the error says where it
//! was searched for and how to put it back. `paleto convert` must stop for it.

use std::path::Path;
use std::process::Command;

fn paleto(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game, keys or converter of the user's: an empty home, and a converter
    // the environment names but that is found nowhere.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME");
    cmd.env("PALETO_CWCONV", "/nonexistent/paleto-cwconv").current_dir(dir).args(args);
    cmd
}

#[test]
fn converting_says_where_the_converter_was_searched_for() {
    let dir = tempfile::tempdir().unwrap();
    let out = paleto(dir.path(), &["convert", "car.yft"]).output().unwrap();
    assert!(!out.status.success(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    for piece in [
        "$PALETO_CWCONV (/nonexistent/paleto-cwconv): not found",
        "packaging/build-cwconv.sh",
        "docs/troubleshooting.md",
    ] {
        assert!(err.contains(piece), "{err}");
    }
}
