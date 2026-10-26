//! Audio containers (`.awc`), following CodeWalker's `AwcFile.cs` (MIT).
//!
//! A file holds streams of chunks. Simple streams have their own format and
//! data chunks; in multichannel files one stream holds interleaved blocks of
//! audio for the others (its channels). Audio is 16-bit PCM or IMA ADPCM, or
//! MP3 in the Enhanced edition.
//! Data may be encrypted with a variant of XXTEA, whose key comes from the
//! game (`magic.dat`).

use crate::Error;

const DATA: u8 = 0x55;
const FORMAT: u8 = 0xFA;
const STREAM_FORMAT: u8 = 0x48;
const PEAK: u8 = 0x36;
/// Sizes of an MP3 stream's frames (u16 each).
const SEEK_TABLE: u8 = 0xA3;
const MARKERS: u8 = 0xBD;
const GRANULAR_GRAINS: u8 = 0x5A;
const GRANULAR_LOOPS: u8 = 0xD9;
const MIDI: u8 = 0x68;
const PCM: u8 = 0;
const ADPCM: u8 = 4;
/// MPEG audio (Enhanced edition): MP3 frames, a seek table giving their sizes.
const MP3: u8 = 7;

/// One sound: mono 16-bit samples.
#[derive(Debug)]
pub struct Sound {
    /// Hash of the stream name (29 bits).
    pub id: u32,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Sound {
    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate.max(1) as f32
    }

    /// A WAV file (16-bit mono PCM).
    pub fn to_wav(&self) -> Vec<u8> {
        // A WAV file says its size in 32 bits.
        let data_len = u32::try_from(self.samples.len() * 2).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(44 + self.samples.len() * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&data_len.saturating_add(36).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.sample_rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

struct Chunk {
    kind: u8,
    offset: usize,
    size: usize,
}

struct Stream {
    id: u32,
    chunks: Vec<Chunk>,
}

/// Format of a channel in a multichannel stream.
struct ChannelFormat {
    id: u32,
    samples: u32,
    rate: u32,
    codec: u8,
}

/// A container's layout: its (decrypted) bytes, flags and streams.
struct Container {
    data: Vec<u8>,
    flags: u16,
    whole_encrypted: bool,
    streams: Vec<Stream>,
}

const NO_KEY: &str = "encrypted audio without the game's audio key";

fn read_container(data: &[u8], key: Option<&[u32; 4]>) -> Result<Container, Error> {
    let mut data = data.to_vec();
    let mut whole_encrypted = false;
    if !data.starts_with(b"ADAT") && !data.starts_with(b"TADA") {
        let key = key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?;
        if !data.len().is_multiple_of(4) || data.len() < 8 {
            return Err(Error::Format("not an audio container".into()));
        }
        decrypt(&mut data, key);
        if !data.starts_with(b"ADAT") {
            return Err(Error::Format("not an audio container".into()));
        }
        whole_encrypted = true;
    }
    if data.starts_with(b"TADA") {
        return Err(Error::Unsupported("big-endian (console) audio".into()));
    }
    let r = |at: usize, n: usize| -> Result<u64, Error> {
        let b = data.get(at..at + n).ok_or_else(|| Error::Format("audio container is truncated".into()))?;
        Ok(b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
    };
    let flags = r(6, 2)? as u16;
    let count = r(8, 4)? as usize;
    // Each stream takes at least 4 bytes of the header (6 with flag 1).
    let per_stream = if flags & 1 != 0 { 6 } else { 4 };
    if data.len() < 16 || count > (data.len() - 16) / per_stream {
        return Err(Error::Format("audio container is truncated".into()));
    }
    let mut at = 16 + if flags & 1 != 0 { count * 2 } else { 0 };
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let raw = r(at, 4)? as u32;
        ids.push((raw & 0x1FFF_FFFF, (raw >> 29) as usize));
        at += 4;
    }
    let mut streams = Vec::with_capacity(count);
    for (id, chunk_count) in ids {
        let mut chunks = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            let raw = r(at, 8)?;
            chunks.push(Chunk { kind: (raw >> 56) as u8, size: ((raw >> 28) & 0x0FFF_FFFF) as usize, offset: (raw & 0x0FFF_FFFF) as usize });
            at += 8;
        }
        streams.push(Stream { id, chunks });
    }
    Ok(Container { data, flags, whole_encrypted, streams })
}

/// The sounds of a `.awc` file. `key` decrypts encrypted files.
pub fn parse(data: &[u8], key: Option<&[u32; 4]>) -> Result<Vec<Sound>, Error> {
    let Container { data, flags, whole_encrypted, streams: parsed } = read_container(data, key)?;
    let chunk = |s: &Stream, kind: u8| -> Option<Result<Vec<u8>, Error>> {
        s.chunks.iter().find(|c| c.kind == kind).map(|c| {
            data.get(c.offset..c.offset + c.size).map(<[u8]>::to_vec).ok_or_else(|| Error::Format("audio chunk out of range".into()))
        })
    };
    let single_encrypted = flags & 2 != 0 && !whole_encrypted;
    let multi = flags & 4 != 0;
    let multi_encrypted = flags & 8 != 0 && !whole_encrypted;

    let mut sounds = Vec::new();
    if multi {
        let source = parsed.iter().find(|s| s.chunks.iter().any(|c| c.kind == STREAM_FORMAT)).ok_or_else(|| Error::Format("multichannel audio without a stream format".into()))?;
        let format = chunk(source, STREAM_FORMAT).unwrap()?;
        let le = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        if format.len() < 12 {
            return Err(Error::Format("audio stream format is truncated".into()));
        }
        let (block_count, block_size, channels) = (le(&format, 0) as usize, le(&format, 4) as usize, le(&format, 8) as usize);
        if block_size == 0 && block_count > 0 {
            return Err(Error::Format("audio blocks of no size".into()));
        }
        let channels: Vec<ChannelFormat> = (0..channels.min((format.len() - 12) / 16))
            .map(|i| {
                let c = &format[12 + i * 16..];
                ChannelFormat { id: le(c, 0), samples: le(c, 4), rate: u16::from_le_bytes([c[10], c[11]]) as u32, codec: c[12] }
            })
            .collect();
        let body = chunk(source, DATA).ok_or_else(|| Error::Format("multichannel audio without data".into()))??;
        let block_key = if multi_encrypted { Some(key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?) } else { None };
        let mut outputs: Vec<Vec<i16>> = vec![Vec::new(); channels.len()];
        if channels.iter().any(|c| c.codec == MP3) {
            // MP3 frames run on from block to block: decoded once gathered.
            let layout = StreamLayout::of(&body, block_count, block_size, block_key);
            for (i, track) in read_tracks(&body, &channels, block_count, &layout)?.into_iter().enumerate() {
                if let Track::Mp3(frames) = track {
                    outputs[i] = decode(&frames.concat(), MP3, channels[i].samples as usize)?;
                }
            }
        }
        for b in 0..block_count {
            let Some(block) = body.get(b * block_size..((b + 1) * block_size).min(body.len())) else { break };
            let mut block = block.to_vec();
            if let Some(key) = block_key {
                let len = block.len() / 4 * 4;
                decrypt(&mut block[..len], key);
            }
            for (i, part) in block_channels(&block, &channels)?.into_iter().enumerate() {
                if channels[i].codec != MP3 {
                    outputs[i].extend(decode(part.data, channels[i].codec, part.samples)?);
                }
            }
        }
        for (channel, mut samples) in channels.iter().zip(outputs) {
            samples.truncate(channel.samples as usize);
            sounds.push(Sound { id: channel.id, sample_rate: channel.rate, samples });
        }
    } else {
        for s in &parsed {
            if s.chunks.iter().any(|c| c.kind == MIDI) {
                continue;
            }
            let (Some(format), Some(body)) = (chunk(s, FORMAT), chunk(s, DATA)) else { continue };
            let (format, mut body) = (format?, body?);
            // Samples, loop point, rate, headroom, loop begin/end, play end/begin, codec.
            if format.len() < 20 {
                return Err(Error::Format("audio format is truncated".into()));
            }
            if single_encrypted {
                body.truncate(body.len() / 4 * 4);
                decrypt(&mut body, key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?);
            }
            let samples = u32::from_le_bytes(format[0..4].try_into().unwrap()) as usize;
            let rate = u16::from_le_bytes([format[8], format[9]]) as u32;
            sounds.push(Sound { id: s.id, sample_rate: rate, samples: decode(&body, format[19], samples)? });
        }
    }
    Ok(sounds)
}

/// Replaces a sound with a WAV or MP3 file. PCM and IMA ADPCM sounds keep
/// their codec, re-encoded from either file. MP3 sounds (Enhanced edition)
/// take the MP3 file's frames as they are: it must be mono MPEG-1 Layer III
/// (32, 44.1 or 48 kHz).
///
/// A sound on its own takes the file mixed down to mono, at the file's sample
/// rate. In a multichannel (streamed) container, such as a radio song, a file
/// with several channels replaces as many channels from the chosen one on
/// (a stereo file: it and the next), and keeps the stream's sample rate
/// unless it replaces all of them.
pub fn replace(file: &[u8], key: Option<&[u32; 4]>, id: u32, audio: &[u8]) -> Result<Vec<u8>, Error> {
    let container = read_container(file, key)?;
    let header = container.data[..16].to_vec();
    let (flags, whole_encrypted) = (container.flags, container.whole_encrypted);
    let (streams, contents) = if flags & 4 != 0 { replace_channels(container, key, id, audio)? } else { replace_sound(container, key, id, audio)? };
    let mut out = write_container(&header, flags, &streams, &contents);
    if whole_encrypted {
        let key = key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?;
        out.resize(out.len().next_multiple_of(4), 0);
        encrypt(&mut out, key);
    }
    Ok(out)
}

/// A container's streams and every chunk's bytes, to lay it out again.
type Laid = (Vec<Stream>, Vec<Vec<Vec<u8>>>);

/// Every chunk's bytes, to lay a container out again.
fn chunk_contents(data: &[u8], streams: &[Stream]) -> Result<Vec<Vec<Vec<u8>>>, Error> {
    let body = |c: &Chunk| data.get(c.offset..c.offset + c.size).map(<[u8]>::to_vec).ok_or_else(|| Error::Format("audio chunk out of range".into()));
    streams.iter().map(|s| s.chunks.iter().map(body).collect()).collect()
}

fn sample_rate(rate: u32) -> Result<u16, Error> {
    u16::try_from(rate).map_err(|_| Error::Unsupported(format!("a sample rate of {rate} Hz (at most 65535)")))
}

fn wants_mp3(id: u32) -> Error {
    Error::Edit(format!("sound {id:08X} is MP3: replace it with an MP3 file (mono, MPEG-1 Layer III)"))
}

/// Replaces a sound of a container of separate sounds.
fn replace_sound(container: Container, key: Option<&[u32; 4]>, id: u32, audio: &[u8]) -> Result<Laid, Error> {
    let Container { data, flags, whole_encrypted, mut streams } = container;
    let index = streams.iter().position(|s| s.id == id).ok_or_else(|| Error::Edit(format!("no sound {id:08X}")))?;
    let mut contents = chunk_contents(&data, &streams)?;

    let stream = &mut streams[index];
    let find = |kind: u8| stream.chunks.iter().position(|c| c.kind == kind);
    let (Some(f), Some(d)) = (find(FORMAT), find(DATA)) else {
        return Err(Error::Edit(format!("sound {id:08X} has no audio data")));
    };
    let mut format = contents[index][f].clone();
    if format.len() < 20 {
        return Err(Error::Format("audio format is truncated".into()));
    }
    let mut seek_table = None;
    let (rate, samples, mut encoded) = match format[19] {
        MP3 if audio.starts_with(b"RIFF") => return Err(wants_mp3(id)),
        MP3 => {
            let mp3 = read_mp3(audio)?;
            seek_table = Some(mp3.frames.iter().flat_map(|f| (f.len() as u16).to_le_bytes()).collect::<Vec<u8>>());
            (mp3.rate, mp3.samples, mp3.frames.concat())
        }
        codec @ (PCM | ADPCM) => {
            let audio = read_audio(audio)?;
            let samples = audio.mono();
            let encoded = if codec == PCM { samples.iter().flat_map(|s| s.to_le_bytes()).collect() } else { encode_adpcm(&samples) };
            (audio.rate, samples, encoded)
        }
        other => return Err(Error::Unsupported(format!("audio codec {other}"))),
    };
    let rate = sample_rate(rate)?;
    if flags & 2 != 0 && !whole_encrypted {
        let key = key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?;
        encoded.resize(encoded.len().next_multiple_of(4), 0);
        encrypt(&mut encoded, key);
    }
    format[0..4].copy_from_slice(&(samples.len() as u32).to_le_bytes());
    format[8..10].copy_from_slice(&rate.to_le_bytes());
    // Peaks: the first 4096 samples' in the format chunk, the others' in a peak chunk.
    let peak = |n: usize| samples.iter().skip(n * 4096).take(4096).map(|s| (s.unsigned_abs() as u32 * 2).min(65535) as u16).max().unwrap_or(0);
    if format.len() >= 24 {
        format[20..22].copy_from_slice(&peak(0).to_le_bytes());
    }
    let peaks: Vec<u8> = (1..=samples.len().saturating_sub(4096) / 4096).flat_map(|n| peak(n).to_le_bytes()).collect();
    contents[index][f] = format;
    contents[index][d] = encoded;
    match find(PEAK) {
        Some(p) if peaks.is_empty() => {
            stream.chunks.remove(p);
            contents[index].remove(p);
        }
        Some(p) => contents[index][p] = peaks,
        None if !peaks.is_empty() && contents[index][f].len() >= 24 => {
            stream.chunks.insert(f + 1, Chunk { kind: PEAK, offset: stream.chunks[f].offset, size: 0 });
            contents[index].insert(f + 1, peaks);
        }
        None => {}
    }
    // MP3 sounds list their frames' sizes.
    if let Some(table) = seek_table {
        match stream.chunks.iter().position(|c| c.kind == SEEK_TABLE) {
            Some(t) => contents[index][t] = table,
            None => {
                stream.chunks.insert(f + 1, Chunk { kind: SEEK_TABLE, offset: stream.chunks[f].offset, size: 0 });
                contents[index].insert(f + 1, table);
            }
        }
    }
    Ok((streams, contents))
}

/// The channels of a multichannel stream's format chunk.
fn channel_formats(format: &[u8]) -> Result<(usize, usize, Vec<ChannelFormat>), Error> {
    let le = |at: usize| format.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| Error::Format("audio stream format is truncated".into()));
    let (block_count, block_size, count) = (le(0)? as usize, le(4)? as usize, le(8)? as usize);
    if block_size == 0 && block_count > 0 {
        return Err(Error::Format("audio blocks of no size".into()));
    }
    let channels = (0..count)
        .map(|i| {
            let at = 12 + i * 16;
            let c = format.get(at..at + 16).ok_or_else(|| Error::Format("audio stream format is truncated".into()))?;
            Ok(ChannelFormat { id: le(at)?, samples: le(at + 4)?, rate: u16::from_le_bytes([c[10], c[11]]) as u32, codec: c[12] })
        })
        .collect::<Result<_, Error>>()?;
    Ok((block_count, block_size, channels))
}

/// Replaces channels of a multichannel (streamed) container. The other
/// channels keep their data as it is, laid out in blocks again.
fn replace_channels(container: Container, key: Option<&[u32; 4]>, id: u32, audio: &[u8]) -> Result<Laid, Error> {
    let Container { data, flags, whole_encrypted, streams } = container;
    let mut contents = chunk_contents(&data, &streams)?;
    let source = streams.iter().position(|s| s.chunks.iter().any(|c| c.kind == STREAM_FORMAT)).ok_or_else(|| Error::Format("multichannel audio without a stream format".into()))?;
    let find = |kind: u8| streams[source].chunks.iter().position(|c| c.kind == kind);
    let (Some(f), Some(d)) = (find(STREAM_FORMAT), find(DATA)) else {
        return Err(Error::Format("multichannel audio without data".into()));
    };
    let mut format = contents[source][f].clone();
    let (block_count, block_size, mut channels) = channel_formats(&format)?;
    let first = channels.iter().position(|c| c.id == id).ok_or_else(|| Error::Edit(format!("no sound {id:08X}")))?;
    let block_key = if flags & 8 != 0 && !whole_encrypted { Some(key.ok_or_else(|| Error::Unsupported(NO_KEY.into()))?) } else { None };
    let body = &contents[source][d];
    let layout = StreamLayout::of(body, block_count, block_size, block_key);
    let mut tracks = read_tracks(body, &channels, block_count, &layout)?;
    let stream_rate = channels[first].rate;

    // The replaced channels: (index, samples, rate).
    let mut replaced = Vec::new();
    match channels[first].codec {
        MP3 if audio.starts_with(b"RIFF") => return Err(wants_mp3(id)),
        MP3 => {
            let mp3 = read_mp3(audio).map_err(|e| match e {
                Error::Unsupported(what) if channels.len() > 1 => Error::Unsupported(format!(
                    "{what}; each channel of a stream takes its own mono MP3 (from a stereo song: ffmpeg -i song.mp3 -af \"pan=mono|c0=c0\" left.mp3, c0=c1 for the right)"
                )),
                other => other,
            })?;
            if mp3.rate != stream_rate && channels.len() > 1 {
                return Err(Error::Edit(format!("the MP3 is at {} Hz, the other channels of this stream at {stream_rate} Hz: export it at {stream_rate} Hz", mp3.rate)));
            }
            tracks[first] = Track::Mp3(mp3.frames);
            replaced.push((first, mp3.samples.len(), mp3.rate));
        }
        ADPCM => {
            let audio = read_audio(audio)?;
            let targets = first..first + audio.channels.len();
            if targets.end > channels.len() {
                return Err(Error::Edit(format!(
                    "the file has {} channels, the stream only {} from sound {id:08X} on: choose an earlier sound, or a file with fewer channels",
                    audio.channels.len(),
                    channels.len() - first
                )));
            }
            // A new rate only if every channel changes.
            let rate = if targets.len() == channels.len() { audio.rate } else { stream_rate };
            for (c, samples) in targets.zip(&audio.channels) {
                if channels[c].codec != ADPCM {
                    return Err(Error::Unsupported("multichannel audio mixing codecs".into()));
                }
                let samples = resample(samples, audio.rate, rate);
                let mut encoded = encode_adpcm(&samples);
                encoded.resize(encoded.len().next_multiple_of(2048), 0);
                tracks[c] = Track::Adpcm(encoded);
                replaced.push((c, samples.len(), rate));
            }
        }
        other => return Err(Error::Unsupported(format!("replacing sounds in multichannel audio of codec {other}"))),
    }
    for (c, samples, rate) in replaced {
        let at = 12 + c * 16;
        format[at + 4..at + 8].copy_from_slice(&(samples as u32).to_le_bytes());
        format[at + 10..at + 12].copy_from_slice(&sample_rate(rate)?.to_le_bytes());
        (channels[c].samples, channels[c].rate) = (samples as u32, rate);
    }

    let (body, blocks, seek) = write_tracks(&tracks, &channels, &layout)?;
    format[0..4].copy_from_slice(&(blocks as u32).to_le_bytes());
    contents[source][f] = format;
    contents[source][d] = body;
    if let Some(t) = find(SEEK_TABLE) {
        contents[source][t] = seek;
    }
    Ok((streams, contents))
}

/// Linear resampling, for a channel joining a stream at another rate.
fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let (at, frac) = (pos as usize, pos.fract());
            let (a, b) = (samples[at.min(samples.len() - 1)] as f64, samples[(at + 1).min(samples.len() - 1)] as f64);
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}

/// Lays a container out again: header, stream and chunk infos, then the
/// chunks in their original order (new ones right after their neighbour).
fn write_container(header: &[u8], flags: u16, streams: &[Stream], contents: &[Vec<Vec<u8>>]) -> Vec<u8> {
    let mut out = header.to_vec();
    let chunk_count: usize = streams.iter().map(|s| s.chunks.len()).sum();
    let info_len = 16 + if flags & 1 != 0 { streams.len() * 2 } else { 0 } + streams.len() * 4 + chunk_count * 8;
    out[12..16].copy_from_slice(&(info_len as u32).to_le_bytes());
    if flags & 1 != 0 {
        let mut first = 0u16;
        for s in streams {
            out.extend_from_slice(&first.to_le_bytes());
            first += s.chunks.len() as u16;
        }
    }
    for s in streams {
        out.extend_from_slice(&((s.id & 0x1FFF_FFFF) | ((s.chunks.len() as u32) << 29)).to_le_bytes());
    }
    let infos_at = out.len();
    out.resize(info_len, 0);
    // Chunks by original position; a chunk shares its position with the one it follows.
    let mut order: Vec<(usize, usize, usize)> = Vec::new();
    for (si, s) in streams.iter().enumerate() {
        for (ci, c) in s.chunks.iter().enumerate() {
            order.push((c.offset, si, ci));
        }
    }
    order.sort();
    let mut placed = vec![Vec::new(); streams.len()];
    for (si, s) in streams.iter().enumerate() {
        placed[si] = vec![(0usize, 0usize); s.chunks.len()];
    }
    for (_, si, ci) in order {
        let kind = streams[si].chunks[ci].kind;
        let align = match kind {
            DATA | MIDI => 16,
            MARKERS | GRANULAR_GRAINS | GRANULAR_LOOPS => 4,
            _ => 1,
        };
        out.resize(out.len().next_multiple_of(align), 0);
        placed[si][ci] = (out.len(), contents[si][ci].len());
        out.extend_from_slice(&contents[si][ci]);
    }
    let mut at = infos_at;
    for (si, s) in streams.iter().enumerate() {
        for (ci, c) in s.chunks.iter().enumerate() {
            let (offset, size) = placed[si][ci];
            let raw = ((c.kind as u64) << 56) | (((size as u64) & 0x0FFF_FFFF) << 28) | (offset as u64 & 0x0FFF_FFFF);
            out[at..at + 8].copy_from_slice(&raw.to_le_bytes());
            at += 8;
        }
    }
    out
}

/// Decoded audio of a WAV or MP3 file.
struct Audio {
    rate: u32,
    channels: Vec<Vec<i16>>,
}

impl Audio {
    fn mono(&self) -> Vec<i16> {
        match &self.channels[..] {
            [one] => one.clone(),
            all => (0..all[0].len()).map(|i| (all.iter().map(|c| c[i] as i32).sum::<i32>() / all.len() as i32) as i16).collect(),
        }
    }
}

/// A WAV or MP3 file, decoded (MP3 of any kind, mono or stereo).
fn read_audio(file: &[u8]) -> Result<Audio, Error> {
    if file.starts_with(b"RIFF") {
        return read_wav(file);
    }
    let decoded = nanomp3::decode_all::<i16>(file);
    if decoded.samples.is_empty() {
        return Err(Error::Format("not a WAV or MP3 file".into()));
    }
    let count = match decoded.channels {
        Some(nanomp3::Channels::Stereo) => 2,
        _ => 1,
    };
    let channels = (0..count).map(|c| decoded.samples.iter().skip(c).step_by(count).copied().collect()).collect();
    Ok(Audio { rate: decoded.sample_rate, channels })
}

#[derive(Debug)]
struct Mp3 {
    rate: u32,
    /// The audio frames (without tags or a VBR information frame).
    frames: Vec<Vec<u8>>,
    /// Decoded, for the sample count and the peaks.
    samples: Vec<i16>,
}

/// MPEG-1 Layer III bitrates (kbit/s) and sample rates.
const MP3_BITRATES: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const MP3_RATES: [u32; 3] = [44100, 48000, 32000];

/// The frames of an MP3 file, which must be mono MPEG-1 Layer III, as the
/// game's MP3 sounds are. ID3 tags and a Xing/Info (VBR) frame are dropped.
fn read_mp3(file: &[u8]) -> Result<Mp3, Error> {
    let required = "the MP3 must be mono, MPEG-1 Layer III (32, 44.1 or 48 kHz)";
    let mut at = 0;
    if file.starts_with(b"ID3") && file.len() >= 10 {
        let size = file[6..10].iter().fold(0usize, |v, &b| (v << 7) | (b & 0x7F) as usize);
        at = 10 + size + if file[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let (mut frames, mut rate) = (Vec::new(), None);
    while let Some(h) = file.get(at..at + 4) {
        if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
            break; // a trailing tag or junk
        }
        let (version, layer, bitrate, sample_rate, mode) = ((h[1] >> 3) & 3, (h[1] >> 1) & 3, (h[2] >> 4) as usize, ((h[2] >> 2) & 3) as usize, h[3] >> 6);
        if version != 3 || layer != 1 || mode != 3 || sample_rate == 3 || bitrate == 0 || bitrate == 15 {
            return Err(Error::Unsupported(required.into()));
        }
        let hz = MP3_RATES[sample_rate];
        if *rate.get_or_insert(hz) != hz {
            return Err(Error::Unsupported("an MP3 whose sample rate changes".into()));
        }
        let len = 144 * MP3_BITRATES[bitrate] as usize * 1000 / hz as usize + ((h[2] >> 1) & 1) as usize;
        let frame = file.get(at..at + len).ok_or_else(|| Error::Format("MP3 file is truncated".into()))?;
        // Mono MPEG-1: 17 bytes of side information, then a VBR tag if there is one.
        let info = frame.get(21..25).is_some_and(|t| t == b"Xing" || t == b"Info");
        if !(frames.is_empty() && info) {
            frames.push(frame.to_vec());
        }
        at += len;
    }
    let rate = rate.ok_or_else(|| Error::Format("no MP3 audio found".into()))?;
    let samples = nanomp3::decode_all::<i16>(&frames.concat()).samples;
    Ok(Mp3 { rate, frames, samples })
}

/// The 16-bit channels of a WAV file (PCM 8 to 32 bits, or 32-bit float).
fn read_wav(wav: &[u8]) -> Result<Audio, Error> {
    let bad = |what: &str| Error::Format(format!("WAV: {what}"));
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let (mut fmt, mut body) = (None, None);
    let mut at = 12;
    while at + 8 <= wav.len() {
        let len = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
        let chunk = &wav[at + 8..(at + 8 + len).min(wav.len())];
        match &wav[at..at + 4] {
            b"fmt " => fmt = Some(chunk),
            b"data" => body = Some(chunk),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let (fmt, body) = (fmt.ok_or_else(|| bad("no format"))?, body.ok_or_else(|| bad("no audio data"))?);
    if fmt.len() < 16 {
        return Err(bad("truncated format"));
    }
    let u16_at = |at: usize| u16::from_le_bytes([fmt[at], fmt[at + 1]]);
    let (mut tag, channels, rate, bits) = (u16_at(0), u16_at(2) as usize, u32::from_le_bytes(fmt[4..8].try_into().unwrap()), u16_at(14));
    if tag == 0xFFFE && fmt.len() >= 26 {
        tag = u16_at(24); // WAVE_FORMAT_EXTENSIBLE: the sub-format
    }
    let size = bits as usize / 8;
    // Below 1 kHz a rate is a damaged header, and would make resampling explode.
    if channels == 0 || size == 0 || rate < 1000 {
        return Err(bad("invalid format"));
    }
    let sample = |b: &[u8]| -> Option<f32> {
        Some(match (tag, bits) {
            (1, 8) => (b[0] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes(b.try_into().unwrap()),
            _ => return None,
        })
    };
    let mut out = vec![Vec::with_capacity(body.len() / size / channels); channels];
    for frame in body.chunks_exact(size * channels) {
        for (c, b) in frame.chunks_exact(size).enumerate() {
            let value = sample(b).ok_or_else(|| Error::Unsupported(format!("WAV format {tag} with {bits}-bit samples")))?;
            out[c].push((value.clamp(-1.0, 1.0) * 32767.0).round() as i16);
        }
    }
    Ok(Audio { rate, channels: out })
}

/// IMA ADPCM in 2048-byte frames, as the decoder reads them.
fn encode_adpcm(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() / 2 + samples.len() / 4088 * 4 + 8);
    let (mut predictor, mut index) = (0i32, 0i32);
    for frame in samples.chunks(4088) {
        out.extend_from_slice(&(index as i16).to_le_bytes());
        out.extend_from_slice(&(predictor as i16).to_le_bytes());
        for pair in frame.chunks(2) {
            let mut byte = 0u8;
            for (k, &s) in pair.iter().enumerate() {
                let step = STEP_TABLE[index as usize];
                let mut delta = s as i32 - predictor;
                let mut nibble = 0u8;
                if delta < 0 {
                    nibble = 8;
                    delta = -delta;
                }
                let mut diff = step >> 3;
                for (bit, part) in [(4u8, step), (2, step >> 1), (1, step >> 2)] {
                    if delta >= part {
                        nibble |= bit;
                        delta -= part;
                        diff += part;
                    }
                }
                predictor = (if nibble & 8 != 0 { predictor - diff } else { predictor + diff }).clamp(-32768, 32767);
                index = (index + INDEX_TABLE[nibble as usize]).clamp(0, 88);
                byte |= nibble << (4 * k);
            }
            out.push(byte);
        }
    }
    out
}

/// A channel's part of a block of a multichannel stream.
struct BlockChannel<'a> {
    data: &'a [u8],
    samples: usize,
    /// The first sample of the channel's part, if it has one.
    first_sample: Option<usize>,
}

/// One block of a multichannel stream: per-channel headers, sample offsets,
/// padding to 2048 bytes, then each channel's data.
fn block_channels<'a>(block: &'a [u8], channels: &[ChannelFormat]) -> Result<Vec<BlockChannel<'a>>, Error> {
    let truncated = || Error::Format("audio block is truncated".into());
    let le = |at: usize| block.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).ok_or_else(truncated);
    // Start frame (ADPCM; -1 for MP3), entries in the offset table, (unused),
    // samples, MP3 frames, MP3 data size.
    let mut headers = Vec::with_capacity(channels.len());
    for i in 0..channels.len() {
        headers.push((le(i * 24 + 4)?, le(i * 24 + 12)?, le(i * 24 + 16)?, le(i * 24 + 20)?));
    }
    let offsets_len: usize = headers.iter().map(|(entries, ..)| entries * 4).sum();
    let mut at = (channels.len() * 24 + offsets_len).next_multiple_of(0x800);
    let mut offsets_at = channels.len() * 24;
    let mut out = Vec::with_capacity(channels.len());
    for (channel, (entries, samples, _frames, size)) in channels.iter().zip(headers) {
        // ADPCM and PCM come in 2048-byte frames, one per offset entry.
        let len = if channel.codec == MP3 { size } else { entries * 2048 };
        let first_sample = if entries > 0 { Some(le(offsets_at)?) } else { None };
        offsets_at += entries * 4;
        // Each channel's data starts 16-byte aligned.
        at = at.next_multiple_of(16);
        out.push(BlockChannel { data: block.get(at..at + len).ok_or_else(truncated)?, samples, first_sample });
        at += len;
    }
    Ok(out)
}

/// A channel of a multichannel stream, as stored: ADPCM frames (2048 bytes
/// each, 4088 samples) or MP3 frames (1152 samples each).
enum Track {
    Adpcm(Vec<u8>),
    Mp3(Vec<Vec<u8>>),
}

/// Samples in an ADPCM frame.
const ADPCM_FRAME_SAMPLES: usize = 4088;

/// A multichannel stream's layout: block size, block encryption, and whether
/// the last block is padded to the full size (Legacy) or not (Enhanced).
struct StreamLayout<'a> {
    block_size: usize,
    key: Option<&'a [u32; 4]>,
    pad_last: bool,
    /// Newer files leave ADPCM channels' start frame unset (-1), as MP3's.
    unset_start: bool,
}

impl<'a> StreamLayout<'a> {
    /// The layout of a stream's data chunk, as the game wrote it.
    fn of(body: &[u8], block_count: usize, block_size: usize, key: Option<&'a [u32; 4]>) -> Self {
        let mut first = body.get(..16).map(<[u8]>::to_vec).unwrap_or_default();
        if let Some(key) = key
            && first.len() == 16
        {
            // XXTEA mixes the whole block: decrypt it all for its first word.
            let mut block = body[..block_size.min(body.len()) / 4 * 4].to_vec();
            decrypt(&mut block, key);
            first = block.get(..16).map(<[u8]>::to_vec).unwrap_or_default();
        }
        let unset_start = first.get(..4).is_some_and(|w| w == [0xFF; 4]);
        StreamLayout { block_size, key, pad_last: body.len() == block_count * block_size, unset_start }
    }
}

/// The channels' data of a multichannel stream's data chunk.
fn read_tracks(body: &[u8], channels: &[ChannelFormat], block_count: usize, layout: &StreamLayout) -> Result<Vec<Track>, Error> {
    let mut tracks: Vec<Track> = channels.iter().map(|c| if c.codec == MP3 { Track::Mp3(Vec::new()) } else { Track::Adpcm(Vec::new()) }).collect();
    for b in 0..block_count {
        let Some(block) = body.get(b * layout.block_size..((b + 1) * layout.block_size).min(body.len())) else { break };
        let mut block = block.to_vec();
        if let Some(key) = layout.key {
            let len = block.len() / 4 * 4;
            decrypt(&mut block[..len], key);
        }
        for (track, part) in tracks.iter_mut().zip(block_channels(&block, channels)?) {
            match track {
                Track::Adpcm(data) => data.extend_from_slice(part.data),
                Track::Mp3(frames) => {
                    // Some streams repeat a block's last frames at the start of
                    // the next (decoder pre-roll): its first sample tells.
                    let Some(first) = part.first_sample else { continue };
                    let mut index = first / 1152;
                    let mut at = 0;
                    while let Some(len) = part.data.get(at..at + 4).and_then(mp3_frame_len) {
                        let Some(frame) = part.data.get(at..at + len) else { break };
                        if index >= frames.len() {
                            frames.push(frame.to_vec());
                        }
                        (index, at) = (index + 1, at + len);
                    }
                }
            }
        }
    }
    Ok(tracks)
}

/// Lays channels out in blocks: each block holds the same stretch of time of
/// every channel. A channel's part of a block is a list of packets of at most
/// 2048 bytes, each listed by its first sample: one ADPCM frame, or as many
/// MP3 frames as fit. Returns the data chunk, the block count and the seek
/// table (each block's first sample).
fn write_tracks(tracks: &[Track], channels: &[ChannelFormat], layout: &StreamLayout) -> Result<(Vec<u8>, usize, Vec<u8>), Error> {
    let n = tracks.len();
    let mp3 = matches!(tracks.first(), Some(Track::Mp3(_)));
    if tracks.iter().any(|t| matches!(t, Track::Mp3(_)) != mp3) {
        return Err(Error::Unsupported("multichannel audio mixing MP3 and ADPCM".into()));
    }
    // Packets per channel and block, after the game's header allowance (as
    // CodeWalker found): 96 bytes per channel, 1/512 of the block and 1024 more.
    let reserved = (96 * n + layout.block_size / 512 + 1024).next_multiple_of(0x800);
    let space = layout.block_size.saturating_sub(reserved) / 2048;
    let packets_max = space / n.max(1);
    if packets_max == 0 {
        return Err(Error::Unsupported("audio blocks too small for their channels".into()));
    }
    // Frames of each channel (ADPCM: 2048-byte frames, padded).
    let frames: Vec<Vec<&[u8]>> = tracks
        .iter()
        .map(|t| match t {
            Track::Adpcm(d) => d.chunks(2048).collect(),
            Track::Mp3(f) => f.iter().map(Vec::as_slice).collect(),
        })
        .collect();
    let frame_samples = if mp3 { 1152 } else { ADPCM_FRAME_SAMPLES };
    // Packets from `start`: the frame count of each, up to `limit` packets.
    let packets = |c: usize, start: usize, limit: usize| -> Vec<usize> {
        let mut out = Vec::new();
        let (mut at, mut count, mut size) = (start, 0, 0);
        while at < frames[c].len() {
            let len = frames[c][at].len();
            if count > 0 && size + len > 2048 {
                if out.len() + 1 == limit {
                    break;
                }
                out.push(count);
                (count, size) = (0, 0);
            }
            count += 1;
            size += len;
            at += 1;
        }
        if count > 0 {
            out.push(count);
        }
        out
    };
    let total = frames.iter().map(Vec::len).max().unwrap_or(0);

    let (mut body, mut seek, mut blocks) = (Vec::new(), Vec::new(), 0);
    let mut first = 0;
    while first < total || blocks == 0 {
        // As many frames as every channel can take in its packets.
        let count = (0..n)
            .filter(|&c| first < frames[c].len())
            .map(|c| packets(c, first, packets_max).iter().sum::<usize>())
            .map(|fit| fit.max(1))
            .min()
            .unwrap_or(0)
            .min(total - first);
        let mut block = Vec::new();
        let (mut offsets, mut data) = (Vec::new(), Vec::new());
        // ADPCM channels' start frame: nominal but in the last block (the space
        // shared out, the first channels taking what is left over), there the
        // frames before; the data is packed either way.
        let nominal = first + count < total;
        let mut start_frame = 0;
        for (c, channel) in channels.iter().enumerate() {
            let end = (first + count).min(frames[c].len());
            let taken = end.saturating_sub(first);
            let groups = if taken == 0 { Vec::new() } else { packets(c, first, usize::MAX).into_iter().scan(0, |sum, g| (*sum < taken).then(|| { *sum += g; g })).collect() };
            // ADPCM streams count one sample more than the channel has.
            let length = channel.samples as usize + usize::from(!mp3);
            let samples = length.saturating_sub(first * frame_samples).min(taken * frame_samples);
            data.resize(data.len().next_multiple_of(16), 0);
            let part = frames[c][first.min(end)..end].concat();
            let head = if mp3 {
                [u32::MAX, groups.len() as u32, 0, samples as u32, taken as u32, part.len() as u32]
            } else {
                let start = if layout.unset_start { u32::MAX } else if nominal { (c * packets_max + c.min(space % n)) as u32 } else { start_frame as u32 };
                [start, taken as u32, 0, samples as u32, 0, 0]
            };
            let mut frame = first;
            for g in &groups {
                offsets.extend_from_slice(&((frame * frame_samples) as u32).to_le_bytes());
                frame += g;
            }
            block.extend(head.iter().flat_map(|w| w.to_le_bytes()));
            data.extend_from_slice(&part);
            if !mp3 {
                data.resize(data.len().next_multiple_of(2048), 0);
            }
            start_frame += taken;
        }
        block.extend_from_slice(&offsets);
        block.resize(block.len().next_multiple_of(0x800), 0);
        block.extend_from_slice(&data);
        if block.len() > layout.block_size {
            return Err(Error::Unsupported("audio too large for its stream's blocks".into()));
        }
        first += count;
        blocks += 1;
        // Blocks fill their size, except an unpadded last one (Enhanced): 16-byte aligned.
        let len = if first < total || layout.pad_last { layout.block_size } else { block.len().next_multiple_of(16) };
        block.resize(len, 0);
        if let Some(key) = layout.key {
            let len = block.len() / 4 * 4;
            encrypt(&mut block[..len], key);
        }
        seek.extend_from_slice(&(((first - count) * frame_samples) as u32).to_le_bytes());
        body.extend_from_slice(&block);
    }
    Ok((body, blocks, seek))
}

/// The length of an MPEG-1 Layer III frame, from its header.
fn mp3_frame_len(h: &[u8]) -> Option<usize> {
    let (version, layer, bitrate, rate) = ((h[1] >> 3) & 3, (h[1] >> 1) & 3, (h[2] >> 4) as usize, ((h[2] >> 2) & 3) as usize);
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 || version != 3 || layer != 1 || rate == 3 || bitrate == 0 || bitrate == 15 {
        return None;
    }
    Some(144 * MP3_BITRATES[bitrate] as usize * 1000 / MP3_RATES[rate] as usize + ((h[2] >> 1) & 1) as usize)
}

fn decode(data: &[u8], codec: u8, samples: usize) -> Result<Vec<i16>, Error> {
    match codec {
        PCM => Ok(data.as_chunks::<2>().0.iter().take(samples).map(|b| i16::from_le_bytes(*b)).collect()),
        ADPCM => Ok(decode_adpcm(data, samples)),
        MP3 => {
            let decoded = nanomp3::decode_all::<i16>(data);
            let mut out = match decoded.channels {
                // Sounds are mono: stereo frames are mixed down.
                Some(nanomp3::Channels::Stereo) => {
                    decoded.samples.as_chunks::<2>().0.iter().map(|[l, r]| ((*l as i32 + *r as i32) / 2) as i16).collect()
                }
                _ => decoded.samples,
            };
            out.truncate(samples);
            Ok(out)
        }
        other => Err(Error::Unsupported(format!("audio codec {other}"))),
    }
}

const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143,
    157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552,
    1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487,
    12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// IMA ADPCM in 2048-byte frames: a 4-byte header (step index, predictor),
/// then two samples per byte, low nibble first.
fn decode_adpcm(data: &[u8], samples: usize) -> Vec<i16> {
    // `samples` comes from the file: two samples per byte at most.
    let mut out = Vec::with_capacity(samples.min(data.len() * 2));
    for frame in data.chunks(2048) {
        if frame.len() < 4 || out.len() >= samples {
            break;
        }
        let mut index = (frame[0] as i32).clamp(0, 88);
        let mut predictor = i16::from_le_bytes([frame[2], frame[3]]) as i32;
        for &byte in &frame[4..] {
            for nibble in [byte & 0xF, byte >> 4] {
                let step = STEP_TABLE[index as usize];
                let mut diff = (((((nibble & 7) as i32) << 1) + 1) * step) >> 3;
                if nibble & 8 != 0 {
                    diff = -diff;
                }
                predictor = (predictor + diff).clamp(-32768, 32767);
                index = (index + INDEX_TABLE[nibble as usize]).clamp(0, 88);
                if out.len() < samples {
                    out.push(predictor as i16);
                }
            }
        }
    }
    out
}

/// Rockstar's XXTEA variant (encryption).
fn encrypt(data: &mut [u8], key: &[u32; 4]) {
    let mut v: Vec<u32> = data.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut z = v[n - 1];
    let mut sum = 0u32;
    for _ in 0..6 + 52 / n {
        sum = sum.wrapping_add(0x9E37_79B9);
        let e = (sum >> 2) & 3;
        for p in 0..n {
            let y = v[(p + 1) % n];
            let k = key[(p & 3) ^ e as usize];
            let mx = ((z >> 5) ^ (y << 2)).wrapping_add((y >> 3) ^ (z << 4)) ^ (sum ^ y).wrapping_add(k ^ z ^ 0x7B3A_207F);
            v[p] = v[p].wrapping_add(mx);
            z = v[p];
        }
    }
    for (chunk, word) in data.as_chunks_mut::<4>().0.iter_mut().zip(v) {
        *chunk = word.to_le_bytes();
    }
}

/// Rockstar's XXTEA variant (decryption).
fn decrypt(data: &mut [u8], key: &[u32; 4]) {
    let mut blocks: Vec<u32> = data.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
    let n = blocks.len();
    if n < 2 {
        return;
    }
    let mut b = blocks[0];
    let mut sum = 0x9E37_79B9u32.wrapping_mul(6 + 52 / n as u32);
    while sum != 0 {
        for i in (0..n).rev() {
            let a = blocks[if i > 0 { i } else { n } - 1];
            let k = key[(i & 3) ^ ((sum >> 2) & 3) as usize];
            let mx = ((a >> 5) ^ (b << 2)).wrapping_add((b >> 3) ^ (a << 4)) ^ (sum ^ b).wrapping_add(k ^ a ^ 0x7B3A_207F);
            blocks[i] = blocks[i].wrapping_sub(mx);
            b = blocks[i];
        }
        sum = sum.wrapping_sub(0x9E37_79B9);
    }
    for (chunk, word) in data.as_chunks_mut::<4>().0.iter_mut().zip(blocks) {
        *chunk = word.to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One stream, PCM: format and data chunks.
    fn pcm_file(samples: &[i16]) -> Vec<u8> {
        let mut data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut format = vec![0u8; 20];
        format[0..4].copy_from_slice(&(samples.len() as u32).to_le_bytes());
        format[8..10].copy_from_slice(&22050u16.to_le_bytes());
        format[19] = PCM;
        let header_len = 16 + 4 + 2 * 8;
        let (format_at, data_at) = (header_len, header_len + format.len());
        let mut out = b"ADAT".to_vec();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0xFF00u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(data_at as u32).to_le_bytes());
        out.extend_from_slice(&((2u32 << 29) | 0x1234).to_le_bytes());
        let info = |kind: u8, size: usize, offset: usize| ((kind as u64) << 56) | ((size as u64) << 28) | offset as u64;
        out.extend_from_slice(&info(FORMAT, format.len(), format_at).to_le_bytes());
        out.extend_from_slice(&info(DATA, data.len(), data_at).to_le_bytes());
        out.append(&mut format);
        out.append(&mut data);
        out
    }

    #[test]
    fn pcm_stream_and_wav() {
        let sounds = parse(&pcm_file(&[0, 1000, -1000, 32767]), None).unwrap();
        let [s] = &sounds[..] else { panic!() };
        assert_eq!((s.id, s.sample_rate, &s.samples[..]), (0x1234, 22050, &[0i16, 1000, -1000, 32767][..]));
        let wav = s.to_wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 8);
    }

    #[test]
    fn adpcm_silence_stays_silent() {
        let mut frame = vec![0u8; 2048];
        frame[0] = 0;
        assert!(decode_adpcm(&frame, 100).iter().all(|&s| s.abs() < 64));
    }

    /// A WAV file: 16-bit stereo.
    fn wav(rate: u32, frames: &[(i16, i16)]) -> Vec<u8> {
        wav_channels(rate, 2, &frames.iter().flat_map(|(l, r)| [*l, *r]).collect::<Vec<_>>())
    }

    /// A WAV file: 16-bit, `channels` interleaved.
    fn wav_channels(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, channels] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2 * channels as u32).to_le_bytes());
        for v in [2 * channels, 16] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn replace_pcm_and_adpcm() {
        let frames: Vec<(i16, i16)> = (0..10000).map(|i| ((i % 200 * 100) as i16, (i % 200 * 100) as i16)).collect();
        let replaced = replace(&pcm_file(&[1, 2, 3]), None, 0x1234, &wav(44100, &frames)).unwrap();
        let [s] = &parse(&replaced, None).unwrap()[..] else { panic!() };
        assert_eq!((s.sample_rate, s.samples.len()), (44100, 10000));
        assert_eq!(s.samples[150], 15000);

        // ADPCM: a tone decodes back close to the original.
        let frames: Vec<(i16, i16)> = (0..10000).map(|i| ((i as f32 * 0.12).sin() * 10000.0) as i16).map(|v| (v, v)).collect();
        let mut adpcm = pcm_file(&[0; 4]);
        let at = adpcm.len() - 8 - 20 + 19;
        adpcm[at] = ADPCM;
        let replaced = replace(&adpcm, None, 0x1234, &wav(22050, &frames)).unwrap();
        let [s] = &parse(&replaced, None).unwrap()[..] else { panic!() };
        assert_eq!((s.sample_rate, s.samples.len()), (22050, 10000));
        let error: i64 = s.samples.iter().zip(&frames).skip(100).map(|(a, (b, _))| (*a as i64 - *b as i64).abs()).max().unwrap();
        assert!(error < 1000, "{error}");

        assert!(replace(&pcm_file(&[1]), None, 0x9999, &wav(44100, &frames)).is_err());
        assert!(replace(&pcm_file(&[1]), None, 0x1234, b"not a wav").is_err());
    }

    /// Silent mono MPEG-1 Layer III frames: 48 kHz, 128 kbit/s, 384 bytes each.
    fn mp3(frames: usize, stereo: bool) -> Vec<u8> {
        let mut frame = vec![0u8; 384];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x94, if stereo { 0x00 } else { 0xC0 }]);
        let mut out = b"ID3\x03\0\0\0\0\0\x02xx".to_vec();
        for _ in 0..frames {
            out.extend_from_slice(&frame);
        }
        out.extend_from_slice(b"TAG");
        out
    }

    #[test]
    fn mp3_input() {
        let parsed = read_mp3(&mp3(10, false)).unwrap();
        assert_eq!((parsed.rate, parsed.frames.len()), (48000, 10));
        assert_eq!(parsed.samples.len(), 11520);
        assert!(read_mp3(&mp3(2, true)).unwrap_err().to_string().contains("mono"));

        // Into a PCM sound: decoded (stereo too) and stored as PCM.
        for stereo in [false, true] {
            let decoded = read_audio(&mp3(10, stereo)).unwrap();
            assert_eq!(decoded.channels.len(), 1 + stereo as usize);
            let replaced = replace(&pcm_file(&[1, 2]), None, 0x1234, &mp3(10, stereo)).unwrap();
            let [s] = &parse(&replaced, None).unwrap()[..] else { panic!() };
            assert_eq!((s.sample_rate, s.samples.len()), (48000, decoded.channels[0].len()));
        }

        // Into an MP3 sound: the frames as they are, and their sizes in the seek table.
        let mut file = pcm_file(&[0; 4]);
        let at = file.len() - 8 - 20 + 19;
        file[at] = MP3;
        let replaced = replace(&file, None, 0x1234, &mp3(10, false)).unwrap();
        let [s] = &parse(&replaced, None).unwrap()[..] else { panic!() };
        assert_eq!((s.sample_rate, s.samples.len()), (48000, 11520));
        let Container { data, streams, .. } = read_container(&replaced, None).unwrap();
        let seek = streams[0].chunks.iter().find(|c| c.kind == SEEK_TABLE).unwrap();
        assert_eq!(&data[seek.offset..seek.offset + seek.size], &[384u16; 10].map(u16::to_le_bytes).concat()[..]);
        let wav = wav(48000, &[(0, 0); 10]);
        assert!(replace(&file, None, 0x1234, &wav).unwrap_err().to_string().contains("is MP3"));
    }

    /// A multichannel container of ADPCM channels (ids 0xA0, 0xA1...), in
    /// small blocks, as the game lays streams out; the last block unpadded.
    fn stream_file(channels: &[Vec<i16>], rate: u16, block_size: usize) -> Vec<u8> {
        let formats: Vec<ChannelFormat> = channels.iter().enumerate().map(|(i, c)| ChannelFormat { id: 0xA0 + i as u32, samples: c.len() as u32, rate: rate as u32, codec: ADPCM }).collect();
        let tracks: Vec<Track> = channels.iter().map(|c| { let mut e = encode_adpcm(c); e.resize(e.len().next_multiple_of(2048), 0); Track::Adpcm(e) }).collect();
        let layout = StreamLayout { block_size, key: None, pad_last: false, unset_start: false };
        let (body, blocks, seek) = write_tracks(&tracks, &formats, &layout).unwrap();
        let mut format = Vec::new();
        for v in [blocks as u32, block_size as u32, channels.len() as u32] {
            format.extend_from_slice(&v.to_le_bytes());
        }
        for f in &formats {
            format.extend_from_slice(&f.id.to_le_bytes());
            format.extend_from_slice(&f.samples.to_le_bytes());
            format.extend_from_slice(&[0, 0]);
            format.extend_from_slice(&rate.to_le_bytes());
            format.extend_from_slice(&[ADPCM, 0, 0, 0]);
        }
        let mut streams = vec![Stream { id: 0, chunks: vec![Chunk { kind: STREAM_FORMAT, offset: 0, size: 0 }, Chunk { kind: DATA, offset: 1, size: 0 }, Chunk { kind: SEEK_TABLE, offset: 2, size: 0 }] }];
        let mut contents = vec![vec![format, body, seek]];
        for f in &formats {
            streams.push(Stream { id: f.id, chunks: Vec::new() });
            contents.push(Vec::new());
        }
        let mut header = b"ADAT".to_vec();
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&0xFF04u16.to_le_bytes());
        header.extend_from_slice(&(streams.len() as u32).to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        write_container(&header, 0xFF04, &streams, &contents)
    }

    fn tone(len: usize, period: usize) -> Vec<i16> {
        (0..len).map(|i| ((i as f32 / period as f32 * std::f32::consts::TAU).sin() * 8000.0) as i16).collect()
    }

    #[test]
    fn replace_in_streams() {
        // Three channels in blocks of 16 KiB: 6 frames per channel and block, so several blocks.
        let channels = [tone(60_000, 50), tone(60_000, 80), tone(60_000, 120)];
        let file = stream_file(&channels, 32000, 16384);
        let sounds = parse(&file, None).unwrap();
        assert_eq!(sounds.iter().map(|s| (s.id, s.samples.len())).collect::<Vec<_>>(), [(0xA0, 60_000), (0xA1, 60_000), (0xA2, 60_000)]);
        // Past ADPCM's first few samples, while its step adapts.
        let error = |a: &[i16], b: &[i16]| a.iter().zip(b).skip(32).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
        // Exactly what ADPCM gives for each channel: blocks split and join cleanly.
        for (sound, channel) in sounds.iter().zip(&channels) {
            assert_eq!(sound.samples, decode_adpcm(&encode_adpcm(channel), channel.len()));
        }

        // A mono file replaces one channel, longer than the others and at another
        // rate (resampled to the stream's); the others keep their data as it was.
        let replaced = replace(&file, None, 0xA1, &wav_channels(64000, 1, &tone(100_000, 160))).unwrap();
        let after = parse(&replaced, None).unwrap();
        assert_eq!(after.iter().map(|s| (s.sample_rate, s.samples.len())).collect::<Vec<_>>(), [(32000, 60_000), (32000, 50_000), (32000, 60_000)]);
        assert_eq!(after[0].samples, sounds[0].samples);
        assert_eq!(after[2].samples, sounds[2].samples);
        assert!(error(&after[1].samples, &tone(50_000, 80)) < 1500);

        // A stereo file replaces the chosen channel and the next.
        let stereo: Vec<(i16, i16)> = tone(10_000, 40).into_iter().zip(tone(10_000, 90)).collect();
        let replaced = replace(&file, None, 0xA1, &wav(32000, &stereo)).unwrap();
        let after = parse(&replaced, None).unwrap();
        assert_eq!(after.iter().map(|s| s.samples.len()).collect::<Vec<_>>(), [60_000, 10_000, 10_000]);
        assert_eq!(after[0].samples, sounds[0].samples);
        assert!(error(&after[2].samples, &tone(10_000, 90)) < 1000);
        // Not from the last channel, which has no next.
        assert!(replace(&file, None, 0xA2, &wav(32000, &stereo)).unwrap_err().to_string().contains("choose an earlier sound"));
    }

    #[test]
    fn xxtea_round_trip() {
        let key = [1, 2, 3, 4];
        let mut data: Vec<u8> = (0..64u8).collect();
        encrypt(&mut data, &key);
        assert_ne!(data, (0..64u8).collect::<Vec<_>>());
        decrypt(&mut data, &key);
        assert_eq!(data, (0..64u8).collect::<Vec<_>>());
    }

    #[test]
    fn encrypted_without_key() {
        assert!(parse(&[1, 2, 3, 4, 5, 6, 7, 8], None).unwrap_err().to_string().contains("audio key"));
    }

    /// A container with one stream of the given chunks, unencrypted.
    fn one_stream(flags: u16, chunks: &[(u8, &[u8])]) -> Vec<u8> {
        let header_len = 16 + 4 + chunks.len() * 8;
        let mut out = b"ADAT".to_vec();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(header_len as u32).to_le_bytes());
        out.extend_from_slice(&(((chunks.len() as u32) << 29) | 0x1234).to_le_bytes());
        let mut at = header_len;
        for (kind, data) in chunks {
            out.extend_from_slice(&(((*kind as u64) << 56) | ((data.len() as u64) << 28) | at as u64).to_le_bytes());
            at += data.len();
        }
        for (_, data) in chunks {
            out.extend_from_slice(data);
        }
        out
    }

    /// Damaged containers give an error, never a hang, a panic or a huge allocation.
    #[test]
    fn damaged_containers() {
        // Four billion streams said to be in a header of 16 bytes.
        let mut huge = b"ADAT".to_vec();
        huge.extend_from_slice(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
        assert!(parse(&huge, None).is_err());
        // A header cut short, given to replace.
        assert!(replace(&huge[..12], None, 0x1234, &[]).is_err());

        // A stream format asking for four billion blocks of no size.
        let mut format = [u32::MAX, 0, 0].iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let file = one_stream(4, &[(STREAM_FORMAT, &format), (DATA, &[0; 64])]);
        assert!(parse(&file, None).is_err());
        assert!(replace(&file, None, 0x1234, &[]).is_err());

        // Encrypted blocks shorter than 16 bytes, in an MP3 stream.
        format = [1u32, 8, 1].iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut channel = [0u8; 16];
        channel[12] = MP3;
        format.extend_from_slice(&channel);
        let file = one_stream(4 | 8, &[(STREAM_FORMAT, &format), (DATA, &[0; 64])]);
        assert!(parse(&file, Some(&[1, 2, 3, 4])).is_err());

        // An ADPCM sound saying it holds four billion samples, in one frame.
        let mut format = vec![0u8; 20];
        format[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        format[19] = ADPCM;
        let sounds = parse(&one_stream(0, &[(FORMAT, &format), (DATA, &[0; 2048])]), None).unwrap();
        assert_eq!(sounds[0].samples.len(), 4088);

        // A WAV at 0 Hz.
        assert!(read_wav(&wav_channels(0, 1, &[0; 8])).is_err());
    }
}
