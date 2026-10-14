//! Voice calls: one LiveKit room per voice channel, exposed to the UI as an iced
//! `Subscription` keyed by the join token. The microphone and the speakers go through
//! WebRTC's own audio device module (`PlatformAudio`), with its echo cancellation and noise
//! suppression; the audio itself never passes through this code.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, Stream, StreamExt};
use iced::Subscription;
use iced::futures::channel::mpsc;
use livekit::options::{AudioEncoding, TrackPublishOptions};
use livekit::prelude::*;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::{AudioProcessingOptions, PlatformAudio, PlayoutDeviceId, RecordingDeviceId};
use plume_lite_protocol::Snowflake;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc as channel;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub url: String,
    pub token: String,
    pub channel_id: Snowflake,
}

/// Audio settings, saved with the preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// `None`: the system's default device.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Name of `output_device`, to play the join and leave chimes on it too.
    pub output_name: Option<String>,
    /// Voice activation: sounds quieter than this (dBFS) are not sent.
    pub sensitivity_db: f32,
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            output_name: None,
            sensitivity_db: -50.0,
            noise_suppression: true,
            echo_cancellation: true,
        }
    }
}

pub const SENSITIVITY_RANGE: std::ops::RangeInclusive<f32> = -80.0..=-10.0;

#[derive(Debug, Clone)]
pub enum Command {
    Mute(bool),
    Deafen(bool),
    Settings(AudioSettings),
    Leave,
}

/// Sends commands to the running call.
#[derive(Debug, Clone)]
pub struct Commands(channel::UnboundedSender<Command>);

impl Commands {
    pub fn send(&self, command: Command) {
        let _ = self.0.send(command);
    }
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// In the room; the UI sends its current switches and settings through `Commands`.
    Connected(Commands),
    /// Someone else arrived in the room, or left it.
    PeerJoined,
    PeerLeft,
    /// Users currently speaking (including oneself).
    Speaking(HashSet<Snowflake>),
    /// Microphone level in dBFS, a few times per second (the meter in the settings).
    Level(f32),
    /// The call is over: `None` when we left, otherwise why (removed, connection lost…).
    Ended(Option<String>),
}

/// A microphone or a speaker the user can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Devices {
    pub inputs: Vec<Device>,
    pub outputs: Vec<Device>,
}

/// The microphones and speakers of this computer.
pub async fn devices() -> Result<Devices, String> {
    tokio::task::spawn_blocking(|| {
        let audio = PlatformAudio::new().map_err(|err| format!("Périphériques audio inaccessibles : {err}"))?;
        let device = |id: &str, name: &str| Device { id: id.to_owned(), name: name.to_owned() };
        Ok(Devices {
            inputs: audio.recording_devices().map(|d| device(d.id.as_str(), &d.name)).collect(),
            outputs: audio.playout_devices().map(|d| device(d.id.as_str(), &d.name)).collect(),
        })
    })
    .await
    .map_err(|err| err.to_string())?
}

/// The call's events, with the voice channel they come from: when hopping channels, the end of
/// the previous call may still be reported after the next one has started.
pub fn subscription(target: Target) -> Subscription<(Snowflake, VoiceEvent)> {
    Subscription::run_with(target, run)
}

fn run(target: &Target) -> impl Stream<Item = (Snowflake, VoiceEvent)> + use<> {
    let target = target.clone();
    let channel_id = target.channel_id;
    iced::stream::channel(100, async move |mut output: mpsc::Sender<VoiceEvent>| {
        let reason = call(&target, &mut output).await.err();
        let _ = output.send(VoiceEvent::Ended(reason)).await;
        // Idle until the UI drops the subscription.
        std::future::pending::<()>().await
    })
    .map(move |event| (channel_id, event))
}

/// Voice activation keeps the microphone open this long after the last loud enough sound.
const HANGOVER: Duration = Duration::from_millis(400);
const LEVEL_EVERY: Duration = Duration::from_millis(100);

/// What decides whether the microphone sends.
struct Mic {
    muted: bool,
    deafened: bool,
    /// Voice activation: open while speaking.
    open_until: Instant,
    /// Voice activation only works if levels keep coming while the track is disabled;
    /// otherwise it is turned off (the microphone then always sends).
    gating: Gating,
    sensitivity_db: f32,
}

#[derive(PartialEq)]
enum Gating {
    /// Not known yet: the first time the gate closes, we watch whether levels keep coming.
    Unknown,
    Probing {
        since: Instant,
        frames: u32,
    },
    Works,
    Unsupported,
}

impl Mic {
    fn sends(&self, now: Instant) -> bool {
        let voice = self.gating == Gating::Unsupported || now < self.open_until;
        !self.muted && !self.deafened && voice
    }
}

async fn call(target: &Target, output: &mut mpsc::Sender<VoiceEvent>) -> Result<(), String> {
    let audio = PlatformAudio::new().map_err(|err| format!("Micro ou haut-parleurs inaccessibles : {err}"))?;
    let (room, mut events) = Room::connect(&target.url, &target.token, RoomOptions::default())
        .await
        .map_err(|err| format!("Connexion au vocal impossible : {err}"))?;
    let (sender, mut commands) = channel::unbounded_channel();
    if output.send(VoiceEvent::Connected(Commands(sender))).await.is_err() {
        let _ = room.close().await;
        return Ok(());
    }

    let track = LocalAudioTrack::create_audio_track("micro", audio.rtc_source());
    let mut published = publish(&room, &track).await;
    let mut levels = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
    let mut levels_alive = true;
    let mut mic = Mic {
        muted: false,
        deafened: false,
        open_until: Instant::now() + HANGOVER,
        gating: Gating::Unknown,
        sensitivity_db: AudioSettings::default().sensitivity_db,
    };
    let mut sending = true;
    let mut last_level = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Leave) => {
                    let _ = room.close().await;
                    return Ok(());
                }
                Some(Command::Mute(muted)) => mic.muted = muted,
                Some(Command::Deafen(deafened)) => {
                    mic.deafened = deafened;
                    for participant in room.remote_participants().values() {
                        for publication in participant.track_publications().values() {
                            publication.set_subscribed(!deafened);
                        }
                    }
                }
                Some(Command::Settings(settings)) => {
                    mic.sensitivity_db = settings.sensitivity_db;
                    apply_settings(&audio, &settings);
                }
            },
            event = events.recv() => match event {
                None => return Err("Connexion au vocal perdue.".to_owned()),
                Some(RoomEvent::ParticipantConnected(_)) => {
                    let _ = output.try_send(VoiceEvent::PeerJoined);
                }
                Some(RoomEvent::ParticipantDisconnected(_)) => {
                    let _ = output.try_send(VoiceEvent::PeerLeft);
                }
                Some(RoomEvent::ActiveSpeakersChanged { speakers }) => {
                    let speaking = speakers.iter().filter_map(|p| p.identity().0.parse().ok()).collect();
                    let _ = output.try_send(VoiceEvent::Speaking(speaking));
                }
                Some(RoomEvent::TrackPublished { publication, .. }) if mic.deafened => {
                    publication.set_subscribed(false);
                }
                // Muted or unmuted by a moderator: LiveKit takes the microphone away or
                // allows it again.
                Some(RoomEvent::ParticipantPermissionChanged { participant: Participant::Local(_), permission }) => {
                    let can_publish = permission.is_none_or(|p| p.can_publish);
                    if can_publish && !published {
                        published = publish(&room, &track).await;
                    } else if !can_publish {
                        published = false;
                    }
                }
                Some(RoomEvent::Disconnected { reason }) => return Err(ended(reason).to_owned()),
                Some(_) => {}
            },
            frame = levels.next(), if levels_alive => match frame {
                Some(frame) => {
                    let now = Instant::now();
                    let level = dbfs(&frame.data);
                    if level >= mic.sensitivity_db {
                        mic.open_until = now + HANGOVER;
                    }
                    if let Gating::Probing { frames, .. } = &mut mic.gating {
                        *frames += 1;
                        if *frames >= 5 {
                            mic.gating = Gating::Works;
                        }
                    }
                    if now.duration_since(last_level) >= LEVEL_EVERY {
                        last_level = now;
                        let _ = output.try_send(VoiceEvent::Level(level));
                    }
                }
                None => {
                    levels_alive = false;
                    mic.gating = Gating::Unsupported;
                }
            },
            _ = tick.tick() => {}
        }

        // Probing: levels stopped while the track was disabled, so they cannot reopen it.
        let now = Instant::now();
        if let Gating::Probing { since, frames } = mic.gating
            && frames < 5
            && now.duration_since(since) > Duration::from_millis(300)
        {
            mic.gating = Gating::Unsupported;
        }
        let should_send = mic.sends(now);
        if should_send != sending {
            if !should_send && mic.gating == Gating::Unknown && !mic.muted && !mic.deafened {
                mic.gating = Gating::Probing { since: now, frames: 0 };
            }
            sending = should_send;
            track.rtc_track().set_enabled(sending);
        }
    }
}

async fn publish(room: &Room, track: &LocalAudioTrack) -> bool {
    let options = TrackPublishOptions {
        source: TrackSource::Microphone,
        // High quality voice (Opus), and nothing at all during silences (DTX).
        audio_encoding: Some(AudioEncoding { max_bitrate: 128_000 }),
        dtx: true,
        red: true,
        ..TrackPublishOptions::default()
    };
    room.local_participant().publish_track(LocalTrack::Audio(track.clone()), options).await.is_ok()
}

fn apply_settings(audio: &PlatformAudio, settings: &AudioSettings) {
    if let Some(id) = &settings.input_device {
        let _ = audio.switch_recording_device(&RecordingDeviceId::from_unchecked_guid(id));
    }
    if let Some(id) = &settings.output_device {
        let _ = audio.switch_playout_device(&PlayoutDeviceId::from_unchecked_guid(id));
    }
    let _ = audio.configure_audio_processing(AudioProcessingOptions {
        echo_cancellation: settings.echo_cancellation,
        noise_suppression: settings.noise_suppression,
        auto_gain_control: true,
        prefer_hardware_processing: false,
    });
}

/// Why the room closed (LiveKit's protocol reason, not the participant one of the prelude).
fn ended(reason: livekit::DisconnectReason) -> &'static str {
    use livekit::DisconnectReason;
    match reason {
        DisconnectReason::ParticipantRemoved => "Vous avez été retiré du vocal.",
        DisconnectReason::DuplicateIdentity => "Vous avez rejoint le vocal depuis un autre appareil.",
        DisconnectReason::RoomDeleted | DisconnectReason::RoomClosed => "Le salon vocal a été fermé.",
        DisconnectReason::ServerShutdown => "Le serveur vocal redémarre.",
        _ => "Déconnecté du vocal.",
    }
}

/// Loudness of a block of samples, in dBFS (0 = loudest possible).
pub fn dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let rms = (sum / samples.len() as f64).sqrt() / 32768.0;
    if rms <= 0.0 { -100.0 } else { (20.0 * rms.log10()).max(-100.0) as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness() {
        assert_eq!(dbfs(&[]), -100.0);
        assert_eq!(dbfs(&[0; 480]), -100.0);
        assert!((dbfs(&[i16::MAX; 480]) - 0.0).abs() < 0.01);
        assert!((dbfs(&[3277; 480]) + 20.0).abs() < 0.1, "a tenth of full scale is -20 dB");
    }

    #[test]
    fn the_microphone_sends_only_when_allowed_and_speaking() {
        let now = Instant::now();
        let mut mic = Mic {
            muted: false,
            deafened: false,
            open_until: now + HANGOVER,
            gating: Gating::Works,
            sensitivity_db: -50.0,
        };
        assert!(mic.sends(now));
        assert!(!mic.sends(now + HANGOVER * 2), "silence closes the gate");
        mic.gating = Gating::Unsupported;
        assert!(mic.sends(now + HANGOVER * 2), "without levels, always open");
        mic.deafened = true;
        assert!(!mic.sends(now), "deafened implies muted");
    }
}
