//! A test companion for voice calls, without a window: it logs in, joins a voice channel,
//! beeps every few seconds and prints how loud it hears the others.
//!
//!     cargo run -p plume-lite-client --example voice_bot -- <server> <name> <password> <channel id>
//!
//! For example, with the development stack: `http://127.0.0.1:8080 bob motdepasse-demo 123…`.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use serde_json::{Value, json};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [server, name, password, channel] = args.as_slice() else {
        eprintln!("usage: voice_bot <server> <name> <password> <channel id>");
        std::process::exit(2);
    };
    let api = format!("{}/api/v1", server.trim_end_matches('/'));
    let http = reqwest::Client::new();

    let login: Value = http
        .post(format!("{api}/auth/login"))
        .json(&json!({ "username": name, "password": password }))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .expect("login failed")
        .json()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap();
    let join: Value = http
        .post(format!("{api}/channels/{channel}/voice"))
        .bearer_auth(token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .expect("cannot join the voice channel")
        .json()
        .await
        .unwrap();
    let (room, mut events) =
        Room::connect(join["url"].as_str().unwrap(), join["token"].as_str().unwrap(), RoomOptions::default())
            .await
            .expect("cannot reach LiveKit");
    println!("{name} is in the voice channel. Ctrl+C to leave.");

    // A short beep every three seconds.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let track = LocalAudioTrack::create_audio_track("beep", RtcAudioSource::Native(source.clone()));
    let options = TrackPublishOptions { source: TrackSource::Microphone, ..TrackPublishOptions::default() };
    room.local_participant().publish_track(LocalTrack::Audio(track), options).await.unwrap();
    tokio::spawn(async move {
        let start = Instant::now();
        let mut phase = 0.0f32;
        loop {
            let beeping = start.elapsed().as_millis() % 3000 < 400;
            let samples: Vec<i16> = (0..480)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 660.0 / 48_000.0;
                    if beeping { (phase.sin() * 6000.0) as i16 } else { 0 }
                })
                .collect();
            let frame =
                AudioFrame { data: samples.into(), sample_rate: 48_000, num_channels: 1, samples_per_channel: 480 };
            if source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    });

    // How loud each person sounds, once per second.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = events.recv() => match event {
                Some(RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), participant, .. }) => {
                    let who = participant.name();
                    tokio::spawn(async move {
                        let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
                        let (mut peak, mut since) = (0u16, Instant::now());
                        while let Some(frame) = stream.next().await {
                            peak = peak.max(frame.data.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0));
                            if since.elapsed() >= Duration::from_secs(1) {
                                let bar = "#".repeat(usize::from(peak / 1000));
                                println!("{who:>12} {peak:>5} {bar}");
                                (peak, since) = (0, Instant::now());
                            }
                        }
                    });
                }
                Some(RoomEvent::Disconnected { reason }) => {
                    println!("disconnected: {reason:?}");
                    break;
                }
                None => break,
                Some(_) => {}
            },
        }
    }
    let _ = room.close().await;
}
