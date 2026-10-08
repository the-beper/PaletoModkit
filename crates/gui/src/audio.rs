//! Audio container (`.awc`) preview: its sounds, playback and WAV export.
//!
//! Playback talks to the PulseAudio server (or PipeWire's, which speaks the
//! same protocol) from a thread; stopping closes its connection.

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, RichText};
use paleto_formats::awc::Sound;
use pulseaudio::protocol;

/// What the user asked for.
pub enum AudioRequest {
    Export(usize),
    ExportAll,
    /// Replace this sound (by id) with a WAV or MP3 file.
    Replace(u32),
}

pub struct AudioView {
    pub sounds: Arc<Vec<Sound>>,
    selected: usize,
    playing: Option<Playback>,
    error: Option<String>,
}

struct Playback {
    index: usize,
    socket: UnixStream,
    done: Arc<AtomicBool>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

impl AudioView {
    pub fn new(sounds: Vec<Sound>) -> Self {
        AudioView { sounds: Arc::new(sounds), selected: 0, playing: None, error: None }
    }

    /// Selects the sound with this id, if there is one.
    pub fn select_id(&mut self, id: u32) {
        if let Some(i) = self.sounds.iter().position(|s| s.id == id) {
            self.selected = i;
        }
    }

    pub fn summary(&self) -> String {
        match self.sounds.len() {
            1 => "1 sound".into(),
            n => format!("{n} sounds"),
        }
    }

    /// `editable`: the container is inside an archive, so sounds can be replaced.
    pub fn ui(&mut self, ui: &mut egui::Ui, busy: bool, editable: bool) -> Option<AudioRequest> {
        let mut request = None;
        if self.playing.as_ref().is_some_and(|p| p.done.load(Ordering::Relaxed)) {
            self.playing = None;
        }
        if self.sounds.is_empty() {
            ui.label("This audio container has no playable sounds.");
            return None;
        }
        ui.horizontal_wrapped(|ui| {
            let playing = self.playing.as_ref().map(|p| p.index);
            if playing == Some(self.selected) {
                if ui.button("⏹ Stop").clicked() {
                    self.playing = None;
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            } else if ui.button("▶ Play").clicked() {
                self.playing = None;
                match play(self.sounds.clone(), self.selected) {
                    Ok(p) => {
                        self.playing = Some(p);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(format!("Playback: {e}")),
                }
            }
            ui.add_enabled_ui(!busy, |ui| {
                ui.menu_button("⬇ Export", |ui| {
                    if ui.button("Selected sound as WAV…").clicked() {
                        request = Some(AudioRequest::Export(self.selected));
                    }
                    if ui.button("All sounds as WAV…").clicked() {
                        request = Some(AudioRequest::ExportAll);
                    }
                });
                let hint = if editable { "Replace the selected sound with a WAV or MP3 file (in the mods folder copy). In streams, such as radio songs, a stereo file replaces the selected channel and the next. MP3 sounds (Enhanced) need a mono MP3, one per channel." } else { "Only sounds inside archives can be replaced" };
                let replace = ui.add_enabled(editable, egui::Button::new("🔄 Replace…"));
                if replace.on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
                    request = Some(AudioRequest::Replace(self.sounds[self.selected].id));
                }
            });
        });
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(crate::theme::error(ui.visuals().dark_mode)));
        }
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for (i, s) in self.sounds.iter().enumerate() {
                let details = format!("{} Hz · {:.2} s", s.sample_rate, s.duration_secs());
                let playing = self.playing.as_ref().is_some_and(|p| p.index == i);
                let label = if playing { format!("🔊 {}", file_stem(s)) } else { file_stem(s) };
                if crate::widgets::list_row(ui, i == self.selected, &label, &details).clicked() {
                    self.selected = i;
                }
            }
        });
        request
    }
}

/// File name (without extension) for a sound: its hash.
pub fn file_stem(sound: &Sound) -> String {
    format!("{:08X}", sound.id)
}

/// Starts playing a sound on a thread.
fn play(sounds: Arc<Vec<Sound>>, index: usize) -> Result<Playback, String> {
    let path = pulseaudio::socket_path_from_env().ok_or("no PulseAudio or PipeWire server found")?;
    let socket = UnixStream::connect(path).map_err(|e| e.to_string())?;
    let control = socket.try_clone().map_err(|e| e.to_string())?;
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let _ = stream(socket, &sounds[index]);
            flag.store(true, Ordering::Relaxed);
        })
        .map_err(|e| e.to_string())?;
    Ok(Playback { index, socket: control, done })
}

/// Sends the samples to the server as it asks for them, then drains.
fn stream(socket: UnixStream, sound: &Sound) -> Result<(), Box<dyn std::error::Error>> {
    let mut sock = BufReader::new(socket);
    let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
    let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
    protocol::write_command_message(sock.get_mut(), 0, &protocol::Command::Auth(auth), protocol::MAX_VERSION)?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
    let version = protocol::MAX_VERSION.min(reply.version);
    let mut props = protocol::Props::new();
    props.set(protocol::Prop::ApplicationName, CString::new(crate::NAME)?);
    protocol::write_command_message(sock.get_mut(), 1, &protocol::Command::SetClientName(props), version)?;
    protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;

    let params = protocol::PlaybackStreamParams {
        sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::S16Le, channels: 1, sample_rate: sound.sample_rate },
        channel_map: protocol::ChannelMap::mono(),
        cvolume: Some(protocol::ChannelVolume::norm(1)),
        sink_name: Some(protocol::DEFAULT_SINK.to_owned()),
        ..Default::default()
    };
    protocol::write_command_message(sock.get_mut(), 2, &protocol::Command::CreatePlaybackStream(params), version)?;
    let (_, info) = protocol::read_reply_message::<protocol::CreatePlaybackStreamReply>(&mut sock, version)?;

    let bytes: Vec<u8> = sound.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut sent = 0;
    let mut send = |sock: &mut BufReader<UnixStream>, len: usize| -> Result<bool, protocol::ProtocolError> {
        let end = (sent + len).min(bytes.len());
        if end > sent {
            protocol::write_memblock(sock.get_mut(), info.channel, &bytes[sent..end], 0)?;
            sent = end;
        }
        Ok(sent < bytes.len())
    };
    send(&mut sock, info.requested_bytes as usize)?;
    const DRAINED: u32 = 3;
    let mut draining = false;
    loop {
        let (seq, msg) = protocol::read_command_message(&mut sock, version)?;
        match msg {
            protocol::Command::Request(protocol::Request { length, .. }) if !draining => {
                if !send(&mut sock, length as usize)? {
                    let drain = protocol::Command::DrainPlaybackStream(info.channel);
                    protocol::write_command_message(sock.get_mut(), DRAINED, &drain, version)?;
                    draining = true;
                }
            }
            protocol::Command::Reply if seq == DRAINED => return Ok(()),
            protocol::Command::Error(e) => return Err(format!("server error {e:?}").into()),
            _ => {}
        }
    }
}
