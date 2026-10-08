//! Previews of text tables (.gxt2) and audio containers (.awc).

use std::time::Duration;

use egui_kittest::Harness;
use paleto_core::ArchivePath;
use paleto_modkit::App;
use paleto_rpf::ArchiveEditor;

fn settle(harness: &mut Harness<'_, App>) {
    for _ in 0..300 {
        harness.step();
        if harness.state().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(3);
}

/// A game folder whose update.rpf holds `files` in data/.
fn game_with(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("update")).unwrap();
    std::fs::write(root.join("GTA5.exe"), b"").unwrap();
    let mut ed = ArchiveEditor::new("update.rpf");
    for (name, data) in files {
        ed.add_file(&format!("data/{name}"), data.clone()).unwrap();
    }
    ed.save(&root.join("update/update.rpf")).unwrap();
    (dir, root)
}

fn open(harness: &mut Harness<'_, App>, root: &std::path::Path, file: &str) {
    let ctx = harness.ctx.clone();
    harness.state_mut().select_game_folder(root, &ctx);
    settle(harness);
    harness.state_mut().open(ArchivePath::new(root.join("update/update.rpf")).join("data"));
    harness.state_mut().preview(file, &ctx);
    settle(harness);
}

#[test]
fn text_table_edit_and_save() {
    let table = paleto_formats::gxt2::write(&[(0x1234_5678, "Hello".to_string())]);
    let (_dir, root) = game_with(&[("global.gxt2", table)]);
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    open(&mut harness, &root, "global.gxt2");
    assert_eq!(harness.state().preview_text(), Some("0x12345678 = Hello\n"));

    // A broken line is refused before saving; a valid edit lands in the mods copy.
    let ctx = harness.ctx.clone();
    harness.state_mut().edit_preview("no separator here");
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    assert!(harness.state().status_error().is_some_and(|e| e.contains("line 1")));
    assert!(!root.join("mods/update/update.rpf").exists());

    harness.state_mut().edit_preview("0x12345678 = Hi\nMY_LABEL = New text\n");
    harness.state_mut().save_preview(&ctx);
    settle(&mut harness);
    let saved = ArchivePath::new(root.join("mods/update/update.rpf")).join("data").join("global.gxt2").read(None).unwrap();
    let entries = paleto_formats::gxt2::parse(&saved).unwrap();
    assert!(entries.contains(&(0x1234_5678, "Hi".to_string())));
    assert!(entries.contains(&(paleto_formats::hash::joaat_lower("my_label"), "New text".to_string())));
}

/// An audio container with one PCM sound.
fn awc(samples: &[i16]) -> Vec<u8> {
    let mut data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut format = vec![0u8; 20];
    format[0..4].copy_from_slice(&(samples.len() as u32).to_le_bytes());
    format[8..10].copy_from_slice(&32000u16.to_le_bytes());
    let header_len = 16 + 4 + 2 * 8;
    let (format_at, data_at) = (header_len, header_len + format.len());
    let mut out = b"ADAT".to_vec();
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0xFF00u16.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(data_at as u32).to_le_bytes());
    out.extend_from_slice(&((2u32 << 29) | 0xABCD).to_le_bytes());
    let info = |kind: u64, size: usize, offset: usize| (kind << 56) | ((size as u64) << 28) | offset as u64;
    out.extend_from_slice(&info(0xFA, format.len(), format_at).to_le_bytes());
    out.extend_from_slice(&info(0x55, data.len(), data_at).to_le_bytes());
    out.append(&mut format);
    out.append(&mut data);
    out
}

#[test]
fn audio_export() {
    let (dir, root) = game_with(&[("beep.awc", awc(&[0; 32000]))]);
    let mut harness = Harness::builder().with_size([1200.0, 700.0]).build_eframe(|cc| App::new(cc));
    open(&mut harness, &root, "beep.awc");
    assert!(harness.state().preview_info().is_some_and(|i| i.ends_with("1 sound")), "{:?}", harness.state().preview_info());

    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let ctx = harness.ctx.clone();
    harness.state_mut().export_sounds(&out, &ctx);
    settle(&mut harness);
    let wav = std::fs::read(out.join("0000ABCD.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(wav.len(), 44 + 64000);

    // Replace it with the exported WAV, shortened: the mods copy changes.
    let short = dir.path().join("short.wav");
    let mut cut = wav[..44 + 2000].to_vec();
    cut[4..8].copy_from_slice(&(36u32 + 2000).to_le_bytes());
    cut[40..44].copy_from_slice(&2000u32.to_le_bytes());
    std::fs::write(&short, cut).unwrap();
    harness.state_mut().replace_sound(0xABCD, &short, &ctx);
    settle(&mut harness);
    let saved = ArchivePath::new(root.join("mods/update/update.rpf")).join("data").join("beep.awc").read(None).unwrap();
    let sounds = paleto_formats::awc::parse(&saved, None).unwrap();
    assert_eq!(sounds[0].samples.len(), 1000);
}
