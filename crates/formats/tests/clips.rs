//! A clip dictionary built by hand: two clips (one animation, one group) sharing one animation.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use paleto_formats::clips;

const SYS: u64 = 0x5000_0000;

fn build() -> Vec<u8> {
    pack(&system(), 1 << 26)
}

/// The system pages of [`build`], to damage before packing them.
fn system() -> Vec<u8> {
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
    system
}

/// A resource of `system`, whose size `pages` (the page flags) describe.
fn pack(system: &[u8], pages: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for w in [0x3743_5352u32, 46, (46 >> 4) << 28 | pages, (46 & 0xF) << 28] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut z = DeflateEncoder::new(out, Compression::default());
    z.write_all(system).unwrap();
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

fn put(system: &mut [u8], at: usize, bytes: &[u8]) {
    system[at..at + bytes.len()].copy_from_slice(bytes);
}

#[test]
fn map_entries_sharing_one_large_animation_are_bounded() {
    // 65535 buckets, each holding the same entry: one animation with 65535 tracks.
    let size = 0x100000; // one page of 0x200 << 11
    let mut s = vec![0u8; size];
    let ptr = |at: usize| (SYS + at as u64).to_le_bytes();
    put(&mut s, 0x18, &ptr(0x100));
    put(&mut s, 0x100 + 0x18, &ptr(0x1000));
    put(&mut s, 0x100 + 0x20, &u16::MAX.to_le_bytes());
    for k in 0..u16::MAX as usize {
        put(&mut s, 0x1000 + k * 8, &ptr(0x200));
    }
    put(&mut s, 0x200 + 8, &ptr(0x300));
    put(&mut s, 0x300 + 0x50, &ptr(0x90000));
    put(&mut s, 0x300 + 0x58, &u16::MAX.to_le_bytes());
    let start = std::time::Instant::now();
    assert!(clips::parse(&pack(&s, (1 << 27) | 11)).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "took {:?}", start.elapsed());

    // An entry that is its own next: the entry limit ends the loop.
    let mut s = system();
    put(&mut s, 0x1A0 + 0x10, &(SYS + 0x1A0).to_le_bytes());
    assert!(clips::parse(&pack(&s, 1 << 26)).is_err());
}

#[test]
fn wild_pointers_and_truncated_files_are_errors() {
    let wild = [u64::MAX, u64::MAX - 7, SYS + 0x0FFF_FFFF, SYS + 0x3FF, 0x6000_0000, 0x7000_0000, 1];
    // The maps, their buckets and entries, an animation and its tracks, a clip and its name and members.
    for at in [0x18, 0x28, 0x118, 0x160, 0x1A8, 0x140, 0x1C8, 0x1E8, 0x250, 0x278, 0x350] {
        for p in wild {
            let mut s = system();
            put(&mut s, at, &p.to_le_bytes());
            let parsed = clips::parse(&pack(&s, 1 << 26));
            // A name at the very end of a page reads as empty.
            if at == 0x278 && matches!(p, 0x5000_03FF | 0x6000_0000) {
                assert_eq!(parsed.unwrap().clips[0].name, "");
            } else {
                assert!(parsed.is_err(), "{at:#x} = {p:#x}");
            }
        }
    }
    // Counts beyond the file.
    for (at, count) in [(0x30, u16::MAX), (0x120, u16::MAX), (0x258, u16::MAX), (0x358, u16::MAX)] {
        let mut s = system();
        put(&mut s, at, &count.to_le_bytes());
        assert!(clips::parse(&pack(&s, 1 << 26)).is_err(), "{at:#x} = {count}");
    }
    // Every truncation of a valid file.
    let data = build();
    for n in 0..data.len() {
        let _ = clips::to_text(&data[..n]);
    }
}
