//! `paleto hash`: generate and resolve, with the names store the app keeps.

use std::path::Path;
use std::process::{Command, Output};

fn paleto(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or names store of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").current_dir(dir).args(args);
    cmd.output().unwrap()
}

fn out(dir: &Path, args: &[&str]) -> String {
    let run = paleto(dir, args);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn generate_prints_a_hash_in_its_three_forms() {
    let dir = tempfile::tempdir().unwrap();
    // The lower mode is the default, the way the game hashes asset names.
    assert_eq!(out(dir.path(), &["hash", "generate", "ADDER"]), "0xB779A091  3078201489  -1216765807  ADDER\n");
    // Exact bytes and the AWC 29 bits.
    assert_eq!(out(dir.path(), &["hash", "generate", "--mode", "exact", "CMapData"]), "0xD3593FA6  3545841574  -749125722  CMapData\n");
    assert_eq!(out(dir.path(), &["hash", "generate", "--mode", "awc", "ADDER"]), "0x1779A091  393846929  393846929  ADDER\n");
    // Several names, one line each.
    assert_eq!(
        out(dir.path(), &["hash", "generate", "a", "bb"]),
        "0xCA2E9442  3392050242  -902917054  a\n0xFCB30ACB  4239592139  -55375157  bb\n"
    );
}

#[test]
fn resolve_reads_values_in_every_form() {
    let dir = tempfile::tempdir().unwrap();
    let out = |text: &str| out(dir.path(), &["hash", "resolve", text]);
    // The hash of CMapData, as metadata prints it: one known name.
    assert_eq!(out("3545841574"), "0xD3593FA6  3545841574  -749125722  CMapData\n");
    assert_eq!(out("0xD3593FA6"), "0xD3593FA6  3545841574  -749125722  CMapData\n");
    assert_eq!(out("D3593FA6"), "0xD3593FA6  3545841574  -749125722  CMapData\n");
    assert_eq!(out("hash_d3593fa6"), "0xD3593FA6  3545841574  -749125722  CMapData\n");
    // Wrapped decimal: -749125722 is the signed reading of 3545841574.
    assert_eq!(out("-749125722"), "0xD3593FA6  3545841574  -749125722  CMapData\n");
}

#[test]
fn resolve_says_unknown_and_still_names_the_hash() {
    let dir = tempfile::tempdir().unwrap();
    // A vehicle name is not a metadata name: nothing in the built-in dictionary holds its hash.
    assert_eq!(out(dir.path(), &["hash", "resolve", "0xB779A091"]), "0xB779A091  3078201489  -1216765807  unknown\n");
    // Unknown is an answer, not an error: the run succeeds.
    let run = paleto(dir.path(), &["hash", "resolve", "0xB779A091"]);
    assert_eq!(run.status.code(), Some(0), "{}", String::from_utf8_lossy(&run.stderr));
}

#[test]
fn a_value_of_no_form_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let run = paleto(dir.path(), &["hash", "resolve", "ADDER"]);
    assert!(!run.status.success(), "{:?}", run.status.code());
    assert!(String::from_utf8_lossy(&run.stderr).contains("is not a hash value"), "{}", String::from_utf8_lossy(&run.stderr));
}

#[test]
fn resolve_uses_the_names_the_app_saved() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".local/share/paleto/names.txt");
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::write(&store, "# written by hand\nMy_odly_named_vehicle\n").unwrap();
    // The hash of the stored name, resolved by the CLI like the app does.
    let hash = paleto_core::hashes::Mode::Lower.hash("my_odly_named_vehicle");
    let value = format!("{:08x}", hash);
    let text = out(dir.path(), &["hash", "resolve", &value]);
    assert_eq!(text, format!("{}  My_odly_named_vehicle\n", paleto_core::hashes::value_line(hash)));
}
