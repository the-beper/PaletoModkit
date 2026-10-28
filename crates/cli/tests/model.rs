//! `paleto model`: CodeWalker XML export of a model file on disk.

use std::process::Command;

/// A Legacy `.ydr` whose drawable has no shaders, skeleton or models: one empty
/// system page (the drawable's header is all zeros), stored uncompressed.
fn empty_drawable() -> Vec<u8> {
    let version = 165u32;
    let one_page = 1u32 << 27; // one page of 0x200 bytes
    let mut data = b"RSC7".to_vec();
    for word in [version, ((version >> 4) << 28) | one_page, ((version & 0xF) << 28) | one_page] {
        data.extend(word.to_le_bytes());
    }
    let payload = vec![0u8; 0x400];
    data.push(0x01); // a final, stored deflate block
    data.extend((payload.len() as u16).to_le_bytes());
    data.extend((!(payload.len() as u16)).to_le_bytes());
    data.extend(payload);
    data
}

fn paleto(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paleto"));
    // No game or settings of the user's: an empty home.
    cmd.env("HOME", dir).env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME").current_dir(dir);
    cmd
}

#[test]
fn model_writes_codewalker_xml() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("prop_probe.ydr");
    std::fs::write(&model, empty_drawable()).unwrap();
    let out = dir.path().join("out");
    let run = paleto(dir.path()).arg("model").arg(&model).arg("-o").arg(&out).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let xml = std::fs::read_to_string(out.join("prop_probe.ydr.xml")).unwrap();
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Drawable>\n"), "{xml}");
    assert!(xml.contains("<Name>prop_probe</Name>"), "{xml}");
    assert!(String::from_utf8_lossy(&run.stdout).contains("prop_probe.ydr.xml"));
}

#[test]
fn model_refuses_files_that_are_not_models() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    for (name, data) in [("broken.ydr", &b"not a resource at all"[..]), ("notes.txt", &empty_drawable()[..])] {
        let file = dir.path().join(name);
        std::fs::write(&file, data).unwrap();
        let run = paleto(dir.path()).arg("model").arg(&file).arg("-o").arg(&out).output().unwrap();
        assert!(!run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stdout));
        assert!(!String::from_utf8_lossy(&run.stderr).is_empty(), "{name}");
    }
    assert!(!out.join("broken.ydr.xml").exists() && !out.join("notes.txt.xml").exists());
}

#[test]
fn model_named_with_dots_writes_only_inside_output() {
    // The stem is "..": its textures folder must not be the parent of the output folder.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let model = src.join("...ydr");
    std::fs::write(&model, empty_drawable()).unwrap();
    let out = dir.path().join("out");
    let run = paleto(dir.path()).arg("model").arg(&model).arg("-o").arg(&out).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert!(out.join("...ydr.xml").is_file());
    // Nothing next to the output folder but the source (hidden entries, if any, are the home's).
    let mut names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect();
    names.sort();
    assert_eq!(names, ["out", "src"]);
}
