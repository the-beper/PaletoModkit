//! Clip dictionaries (`.ycd`): animation clips and the animations they play,
//! summarised for previews (counts, times and the tracks present, not the
//! compressed sequence data). Layout following CodeWalker's `ClipDictionary.cs`
//! (MIT), checked against Enhanced files; Legacy files share the resource
//! version and are expected to use the same layout (unverified).

use std::fmt::Write;

use crate::Error;
use crate::names::Names;
use crate::rsc::{Resource, u16_at, u32_at, u64_at};

const ROOT: u64 = 0x5000_0000;
/// Bones listed per animation in the text.
const MAX_BONES: usize = 40;

/// What a clip plays.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipKind {
    /// One animation, between `start` and `end` seconds at `rate`.
    Animation { animation: u64, start: f32, end: f32, rate: f32 },
    /// Several animations played together.
    Group { members: Vec<Member> },
    /// A type this reader does not know.
    Other(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub animation: u64,
    pub start: f32,
    pub end: f32,
    pub rate: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Hash of the clip name (the dictionary key).
    pub hash: u32,
    /// Name stored in the clip (such as `pack:/name.clip`), empty if absent.
    pub name: String,
    pub kind: ClipKind,
}

/// One bone (or other target) animated by one track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub bone: u16,
    /// Track type: 0 is position, 1 rotation (others print as numbers).
    pub track: u8,
    /// Second type byte (CodeWalker: `Unk0`).
    pub format: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    /// Virtual address in the file, which clips refer to.
    pub id: u64,
    /// Hash of the animation name (the dictionary key).
    pub hash: u32,
    pub frames: u16,
    pub frame_limit: u16,
    pub duration: f32,
    pub sequences: u16,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Default, PartialEq)]
pub struct ClipDictionary {
    pub clips: Vec<Clip>,
    pub animations: Vec<Animation>,
}

pub fn to_text(data: &[u8]) -> Result<String, Error> {
    Ok(format(&parse(data)?))
}

pub fn parse(data: &[u8]) -> Result<ClipDictionary, Error> {
    let res = Resource::parse(data)?;
    let root = res.block(ROOT, 0x38)?;
    let mut dict = ClipDictionary::default();
    let animations = u64_at(root, 0x18);
    if animations != 0 {
        let map = res.block(animations, 0x28)?;
        let mut tracks = 0;
        for (hash, ptr) in hash_map(&res, u64_at(map, 0x18), u16_at(map, 0x20))? {
            let a = animation(&res, hash, ptr)?;
            // Damaged entries may all point at one animation with many tracks.
            tracks += a.tracks.len();
            if tracks > MAX_TRACKS {
                return Err(Error::Format("clip dictionary with too many tracks".into()));
            }
            dict.animations.push(a);
        }
    }
    for (hash, ptr) in hash_map(&res, u64_at(root, 0x28), u16_at(root, 0x30))? {
        dict.clips.push(clip(&res, hash, ptr)?);
    }
    dict.clips.sort_by_key(|c| c.hash);
    dict.animations.sort_by_key(|a| a.hash);
    Ok(dict)
}

/// Tracks of all animations at most.
const MAX_TRACKS: usize = 4_000_000;

/// Entries (key, value pointer) of a chained hash map: `capacity` bucket
/// pointers, each starting a list of {key u32, value ptr at 8, next ptr at 0x10}.
fn hash_map(res: &Resource, buckets: u64, capacity: u16) -> Result<Vec<(u32, u64)>, Error> {
    let mut out = Vec::new();
    if buckets == 0 || capacity == 0 {
        return Ok(out);
    }
    for bucket in res.block(buckets, capacity as usize * 8)?.as_chunks::<8>().0 {
        let mut next = u64::from_le_bytes(*bucket);
        while next != 0 {
            // A loop in a damaged file ends at the entry limit.
            if out.len() >= 1 << 16 {
                return Err(Error::Format("clip map has too many entries".into()));
            }
            let entry = res.block(next, 0x18)?;
            out.push((u32_at(entry, 0), u64_at(entry, 8)));
            next = u64_at(entry, 0x10);
        }
    }
    Ok(out)
}

fn animation(res: &Resource, hash: u32, ptr: u64) -> Result<Animation, Error> {
    let b = res.block(ptr, 0x60)?;
    let (sequences, tracks) = (u16_at(b, 0x48), u16_at(b, 0x58) as usize);
    let list = u64_at(b, 0x50);
    let tracks = if tracks == 0 || list == 0 { Vec::new() } else { res.block(list, tracks * 4)?.as_chunks::<4>().0.iter().map(|t| Track { bone: u16::from_le_bytes([t[0], t[1]]), track: t[2], format: t[3] }).collect() };
    Ok(Animation { id: ptr, hash, frames: u16_at(b, 0x14), frame_limit: u16_at(b, 0x16), duration: f32::from_bits(u32_at(b, 0x18)), sequences, tracks })
}

fn clip(res: &Resource, hash: u32, ptr: u64) -> Result<Clip, Error> {
    let b = res.block(ptr, 0x68)?;
    let name = match (u64_at(b, 0x18), u16_at(b, 0x20)) {
        (0, _) => String::new(),
        (p, _) => res.string(p)?,
    };
    let f = |at| f32::from_bits(u32_at(b, at));
    let kind = match u32_at(b, 0x10) {
        1 => ClipKind::Animation { animation: u64_at(b, 0x50), start: f(0x58), end: f(0x5c), rate: f(0x60) },
        2 => {
            let (list, count) = (u64_at(b, 0x50), u16_at(b, 0x58) as usize);
            let entries = if list == 0 || count == 0 { &[][..] } else { res.block(list, count * 0x18)? };
            let members = entries.as_chunks::<0x18>().0.iter().map(|m| Member { animation: u64_at(m, 0x10), start: f32::from_bits(u32_at(m, 0)), end: f32::from_bits(u32_at(m, 4)), rate: f32::from_bits(u32_at(m, 8)) }).collect();
            ClipKind::Group { members }
        }
        t => ClipKind::Other(t),
    };
    Ok(Clip { hash, name, kind })
}

fn track_name(t: u8) -> String {
    match t {
        0 => "pos".into(),
        1 => "rot".into(),
        t => format!("t{t}"),
    }
}

/// Tracks that share a bone, such as `61[pos,rot]`, and how many bones there are.
fn bones(tracks: &[Track]) -> (usize, String) {
    let mut by_bone: Vec<(u16, Vec<String>)> = Vec::new();
    for t in tracks {
        match by_bone.iter_mut().find(|(b, _)| *b == t.bone) {
            Some((_, v)) => v.push(track_name(t.track)),
            None => by_bone.push((t.bone, vec![track_name(t.track)])),
        }
    }
    let shown: Vec<String> = by_bone.iter().take(MAX_BONES).map(|(b, v)| format!("{b}[{}]", v.join(","))).collect();
    let more = if by_bone.len() > MAX_BONES { format!(" ... (+{})", by_bone.len() - MAX_BONES) } else { String::new() };
    (by_bone.len(), shown.join(" ") + &more)
}

fn format(dict: &ClipDictionary) -> String {
    let names = Names::new();
    let label = |hash: u32| match names.name(hash) {
        n if n.starts_with("hash_") => format!("{hash:#010X}"),
        n => n.into_owned(),
    };
    let animation = |id: u64| dict.animations.iter().find(|a| a.id == id).map_or_else(|| "?".to_string(), |a| label(a.hash));
    let mut out = String::new();
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let _ = writeln!(out, "{}, {}", plural(dict.clips.len(), "clip", "clips"), plural(dict.animations.len(), "animation", "animations"));
    for c in &dict.clips {
        let name = if c.name.is_empty() { String::new() } else { format!(" ({})", c.name) };
        match &c.kind {
            ClipKind::Animation { animation: a, start, end, rate } => {
                let _ = writeln!(out, "\nclip {}{name}: animation clip\n  animation {}, {start:.3} to {end:.3} s, rate {rate:.3}, duration {:.3} s", label(c.hash), animation(*a), end - start);
            }
            ClipKind::Group { members } => {
                let _ = writeln!(out, "\nclip {}{name}: clip group of {}", label(c.hash), members.len());
                for m in members {
                    let _ = writeln!(out, "  animation {}, {:.3} to {:.3} s, rate {:.3}, duration {:.3} s", animation(m.animation), m.start, m.end, m.rate, m.end - m.start);
                }
            }
            ClipKind::Other(t) => {
                let _ = writeln!(out, "\nclip {}{name}: unknown clip type {t}", label(c.hash));
            }
        }
    }
    for a in &dict.animations {
        let (bones, list) = bones(&a.tracks);
        let _ = writeln!(out, "\nanimation {}: {} frames, {:.3} s, {} sequences, {} tracks on {bones} bones", label(a.hash), a.frames, a.duration, a.sequences, a.tracks.len());
        if !list.is_empty() {
            let _ = writeln!(out, "  bone tags: {list}");
        }
    }
    out
}
