//! `paleto install` of a super package (.oivs): what it offers, and choosing from it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn paleto(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or settings of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").current_dir(dir).args(args);
    cmd.output().unwrap()
}

fn zip(path: &Path, entries: &[(&str, &str)]) -> PathBuf {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path.to_path_buf()
}

const SUPER: &str = r#"<superpackage version="1.0" target="Five"><metadata><name>Pack</name><version><major>1</major><minor>0</minor></version></metadata>
<modules>
<module id="base" required="true" name="Base"><install><content><add source="b.asi">scripts\b.asi</add></content></install></module>
<module id="extra" name="Extra"><install><content><add source="e.asi">scripts\e.asi</add></content></install></module>
</modules>
<groups><group id="alt" title="Alternative" allowNone="true">
<option id="one" name="One"><install><content><add source="o.asi">scripts\o.asi</add></content></install></option>
</group></groups></superpackage>"#;

#[test]
fn lists_and_chooses_what_a_super_package_offers() {
    let dir = tempfile::tempdir().unwrap();
    let package = zip(&dir.path().join("pack.oivs"), &[("super.xml", SUPER), ("content/b.asi", "b"), ("content/e.asi", "e"), ("content/o.asi", "o")]);
    let package = package.to_string_lossy();
    let out = paleto(dir.path(), &["install", "--list", "--select", "extra,alt=one", &package]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for line in ["Pack 1.0 by  (OIVS 1.0)", "[x] base", "(required)", "[x] extra", "group alt (Alternative):", "( ) none", "(*) one"] {
        assert!(text.contains(line), "{line}: {text}");
    }
    let unknown = paleto(dir.path(), &["install", "--select", "nope", &package]);
    assert!(!unknown.status.success() && String::from_utf8_lossy(&unknown.stderr).contains("no module nope"));
    // A value that starts with '-' unticks a module.
    let out = paleto(dir.path(), &["install", "--list", "--select", "-extra", &package]);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("[ ] extra"), "{}", String::from_utf8_lossy(&out.stderr));
    // Not for an add-on pack, which would be installed.
    std::fs::write(dir.path().join("dlc.rpf"), b"").unwrap();
    let out = paleto(dir.path(), &["install", "--list", &dir.path().join("dlc.rpf").to_string_lossy()]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("are for super packages (.oivs)"));
    let plain = zip(&dir.path().join("plain.oiv"), &[("assembly.xml", r#"<package version="2.2"><metadata><name>P</name></metadata><content/></package>"#)]);
    let out = paleto(dir.path(), &["install", "--list", &plain.to_string_lossy()]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("are for super packages (.oivs)"));
}

#[test]
fn a_super_package_with_nothing_chosen_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = SUPER.replace(r#"required="true""#, r#"required="false""#);
    let package = zip(&dir.path().join("pack.oivs"), &[("super.xml", &manifest), ("content/b.asi", "b"), ("content/e.asi", "e"), ("content/o.asi", "o")]);
    let out = paleto(dir.path(), &["install", &package.to_string_lossy()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing is chosen: choose with --select"), "{}", String::from_utf8_lossy(&out.stderr));
}
