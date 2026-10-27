//! A clip dictionary built by hand: two clips (one animation, one group) sharing one animation.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use paleto_formats::clips;

const SYS: u64 = 0x5000_0000;

fn build() -> Vec<u8> {
    let mut system = vec![0u8; 0x400];
    let mut put = |at: usize, bytes: &[u8]| system[at..at + bytes.len()].copy_from_slice(bytes);
    let ptr = |at: u64| (SYS + at).to_le_bytes();
    // Root: animation map at 0x100, clip buckets at 0x140 (capacity 2, two clips).
    put(0x18, &ptr(0x100));
    put(0x28, &ptr(0x140));
    put(0x30, &[2, 0, 2, 0]);
    // Animation map: buckets at 0x160, one entry.
    put(0x100 + 0x18, &ptr(0x160));
    put(0x100 + 0x20, &[2, 0, 1, 0]);
    put(0x160, &ptr(0x1A0));
    put(0x1A0, &0x1234u32.to_le_bytes());
    put(0x1A8, &ptr(0x200));
    // Clip buckets, entries {key, value}.
    put(0x140, &ptr(0x1C0));
    put(0x148, &ptr(0x1E0));
    put(0x1C0, &0xAAAAu32.to_le_bytes());
    put(0x1C8, &ptr(0x260));
    put(0x1E0, &0xBBBBu32.to_le_bytes());
    put(0x1E8, &ptr(0x300));
    // Animation: 31 frames, 1 second, one sequence, three tracks.
    put(0x214, &31u16.to_le_bytes());
    put(0x216, &32u16.to_le_bytes());
    put(0x218, &1.0f32.to_le_bytes());
    put(0x248, &1u16.to_le_bytes());
    put(0x250, &ptr(0x2E0));
    put(0x258, &3u16.to_le_bytes());
    put(0x2E0, &[0, 0, 0, 0, 0, 0, 1, 1, 7, 0, 1, 1]);
    // Animation clip: 0 to 0.5 s at rate 2.
    put(0x260 + 0x10, &1u32.to_le_bytes());
    put(0x260 + 0x18, &ptr(0x3C0));
    put(0x260 + 0x50, &ptr(0x200));
    put(0x260 + 0x5C, &0.5f32.to_le_bytes());
    put(0x260 + 0x60, &2.0f32.to_le_bytes());
    put(0x3C0, b"pack:/a.clip\0");
    // Group clip with one member.
    put(0x300 + 0x10, &2u32.to_le_bytes());
    put(0x300 + 0x50, &ptr(0x380));
    put(0x300 + 0x58, &[1, 0, 1, 0]);
    put(0x380, &0.25f32.to_le_bytes());
    put(0x384, &1.0f32.to_le_bytes());
    put(0x388, &1.0f32.to_le_bytes());
    put(0x390, &ptr(0x200));

    let mut out = Vec::new();
    for w in [0x3743_5352u32, 46, (46 >> 4) << 28 | 1 << 26, (46 & 0xF) << 28] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(&system).unwrap();
    z.finish().unwrap()
}

#[test]
fn summary_of_clips_and_animations() {
    let text = clips::to_text(&build()).unwrap();
    let expected = "\
2 clips, 1 animation

clip 0x0000AAAA (pack:/a.clip): animation clip
  animation 0x00001234, 0.000 to 0.500 s, rate 2.000, duration 0.500 s

clip 0x0000BBBB: clip group of 1
  animation 0x00001234, 0.250 to 1.000 s, rate 1.000, duration 0.750 s

animation 0x00001234: 31 frames, 1.000 s, 1 sequences, 3 tracks on 2 bones
  bone tags: 0[pos,rot] 7[rot]
";
    assert_eq!(text, expected);
}

#[test]
fn damaged_files_are_errors() {
    let data = build();
    assert!(clips::to_text(&data[..10]).is_err());
    assert!(clips::to_text(b"not a resource at all").is_err());
}
