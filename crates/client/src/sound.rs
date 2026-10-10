//! Join and leave chimes of voice channels. They are synthesized (no sound file to ship) and
//! played by a worker thread on a short-lived output stream of their own: LiveKit's audio
//! module only plays the call. Between two chimes, nothing is open and the thread sleeps.

use std::f32::consts::TAU;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Someone (oneself included) arrived in our voice channel: two rising notes.
    Join,
    /// Someone left it, or we did: the same notes, falling.
    Leave,
}

/// Peak loudness, well below a voice in the call.
const VOLUME: f32 = 0.2;
const LENGTH_SECS: f32 = 0.3;
/// The second note starts this long after the first one.
const GAP_SECS: f32 = 0.09;
const ATTACK_SECS: f32 = 0.005;
const DECAY_SECS: f32 = 0.05;
const FADE_OUT_SECS: f32 = 0.02;

impl Sound {
    /// The chime at `rate` Hz, mono, within [-VOLUME, VOLUME].
    pub fn samples(self, rate: u32) -> Vec<f32> {
        // E5 and B5, a fifth apart.
        let (low, high) = (659.25, 987.77);
        let notes = match self {
            Sound::Join => [low, high],
            Sound::Leave => [high, low],
        };
        let rate = rate as f32;
        let mut out = vec![0.0_f32; (rate * LENGTH_SECS) as usize];
        for (i, freq) in notes.into_iter().enumerate() {
            let start = (rate * GAP_SECS * i as f32) as usize;
            for (n, sample) in out[start..].iter_mut().enumerate() {
                let t = n as f32 / rate;
                let envelope = (t / ATTACK_SECS).min(1.0) * (-t / DECAY_SECS).exp();
                let phase = TAU * freq * t;
                // A touch of the octave above makes it a chime rather than a beep.
                *sample += envelope * (phase.sin() + 0.25 * (2.0 * phase).sin());
            }
        }
        let peak = out.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        let fade = ((rate * FADE_OUT_SECS) as usize).max(1);
        let len = out.len();
        for (n, sample) in out.iter_mut().enumerate() {
            // Down to exactly zero at the end: no click when the stream stops.
            let tail = ((len - n) as f32 / fade as f32).min(1.0);
            *sample *= VOLUME / peak * tail;
        }
        out
    }
}

struct Request {
    sound: Sound,
    /// Name of the output device; the system's default one when `None` or not found.
    device: Option<String>,
}

static WORKER: OnceLock<Option<Sender<Request>>> = OnceLock::new();

/// Plays `sound` without waiting, on the output device named `device` if there is one.
pub fn play(sound: Sound, device: Option<&str>) {
    let worker = WORKER.get_or_init(|| {
        let (sender, requests) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("chimes".into()).spawn(move || worker(&requests));
        spawned.ok().map(|_| sender)
    });
    if let Some(sender) = worker {
        let _ = sender.send(Request { sound, device: device.map(str::to_owned) });
    }
}

fn worker(requests: &Receiver<Request>) {
    let host = cpal::default_host();
    // Finding a device by name lists them all (slow with ALSA): remember the last search.
    let mut named: Option<(String, Option<cpal::Device>)> = None;
    while let Ok(mut request) = requests.recv() {
        // Several people arriving at once: one chime is enough.
        while let Ok(newer) = requests.try_recv() {
            request = newer;
        }
        let device = match request.device {
            Some(name) => {
                if named.as_ref().is_none_or(|(searched, _)| *searched != name) {
                    let found = host
                        .output_devices()
                        .ok()
                        .and_then(|mut all| all.find(|d| d.description().is_ok_and(|desc| desc.name() == name)));
                    named = Some((name, found));
                }
                named.as_ref().and_then(|(_, device)| device.clone())
            }
            None => None,
        };
        // The default device is looked up each time: it changes when a headset is plugged in.
        let Some(device) = device.or_else(|| host.default_output_device()) else { continue };
        if let Err(err) = play_on(&device, request.sound) {
            eprintln!("cannot play a chime: {err}");
            named = None; // the device may be gone: search again next time
        }
    }
}

fn play_on(device: &cpal::Device, sound: Sound) -> Result<(), String> {
    let supported = device.default_output_config().map_err(|err| err.to_string())?;
    let config = supported.config();
    let samples = sound.samples(config.sample_rate);
    let length = Duration::from_secs_f32(samples.len() as f32 / config.sample_rate as f32);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => output::<f32>(device, config, samples),
        SampleFormat::I16 => output::<i16>(device, config, samples),
        SampleFormat::U16 => output::<u16>(device, config, samples),
        SampleFormat::I32 => output::<i32>(device, config, samples),
        other => return Err(format!("unsupported sample format {other:?}")),
    }?;
    stream.play().map_err(|err| err.to_string())?;
    // Let the end drain through the device's buffer, then close the stream.
    std::thread::sleep(length + Duration::from_millis(150));
    Ok(())
}

fn output<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels).max(1);
    let mut samples = samples.into_iter();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    frame.fill(T::from_sample(samples.next().unwrap_or(0.0)));
                }
            },
            |err| eprintln!("chime stream error: {err}"),
            Some(Duration::from_secs(2)),
        )
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chimes_are_short_soft_and_end_in_silence() {
        for sound in [Sound::Join, Sound::Leave] {
            let samples = sound.samples(48_000);
            assert_eq!(samples.len(), 14_400, "0.3 s");
            let peak = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
            assert!((peak - VOLUME).abs() < 1e-4, "{peak}");
            assert!(samples[0].abs() < 1e-3 && samples[samples.len() - 1].abs() < 1e-4, "no clicks");
        }
        assert_ne!(Sound::Join.samples(44_100), Sound::Leave.samples(44_100));
    }
}
