//! A real call, without microphone or speakers: two participants join the same voice channel
//! through the Plume Lite server and LiveKit; one sends a tone, the other must hear it, and the
//! server must report both in the channel (LiveKit's webhooks).
//!
//! Needs the development stack: `docker compose up -d livekit` and
//! `cargo run -p plume-lite-server` (with the `.env` of `.env.example`). Then:
//!
//!     cargo test -p plume-lite-client --test voice_call -- --ignored

use std::time::Duration;

use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use serde_json::{Value, json};

const SERVER: &str = "http://127.0.0.1:8080/api/v1";

async fn call(http: &reqwest::Client, method: reqwest::Method, path: &str, token: Option<&str>, body: Value) -> Value {
    let mut request = http.request(method, format!("{SERVER}{path}")).json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.expect("is the server running?");
    let status = response.status();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    assert!(status.is_success(), "{path}: {status} {value}");
    value
}

async fn account(http: &reqwest::Client, name: &str) -> String {
    let body = json!({ "username": name, "password": "motdepasse-essai" });
    call(http, reqwest::Method::POST, "/auth/register", None, body).await["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
#[ignore = "needs the development server and LiveKit running"]
async fn two_people_hear_each_other() {
    let http = reqwest::Client::new();
    let suffix = std::process::id();
    let (alice, bob) = (account(&http, &format!("alice{suffix}")).await, account(&http, &format!("bob{suffix}")).await);

    // Alice's guild, a voice channel, and Bob invited.
    let guild = call(&http, reqwest::Method::POST, "/guilds", Some(&alice), json!({ "name": "Appel" })).await;
    let guild_id = guild["id"].as_str().unwrap();
    let path = format!("/guilds/{guild_id}/channels");
    let channel =
        call(&http, reqwest::Method::POST, &path, Some(&alice), json!({ "name": "vocal", "kind": "voice" })).await;
    let channel_id = channel["id"].as_str().unwrap();
    let invite =
        call(&http, reqwest::Method::POST, &format!("/guilds/{guild_id}/invites"), Some(&alice), json!({})).await;
    call(
        &http,
        reqwest::Method::POST,
        &format!("/invites/{}", invite["code"].as_str().unwrap()),
        Some(&bob),
        json!({}),
    )
    .await;

    // Both join the room.
    let join = |token: String| {
        let http = http.clone();
        let path = format!("/channels/{channel_id}/voice");
        async move {
            let join = call(&http, reqwest::Method::POST, &path, Some(&token), json!({})).await;
            Room::connect(join["url"].as_str().unwrap(), join["token"].as_str().unwrap(), RoomOptions::default())
                .await
                .expect("cannot join the LiveKit room")
        }
    };
    let (alice_room, _alice_events) = join(alice.clone()).await;
    let (_bob_room, mut bob_events) = join(bob).await;

    // Alice sends a 440 Hz tone.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let track = LocalAudioTrack::create_audio_track("tone", RtcAudioSource::Native(source.clone()));
    let options = TrackPublishOptions { source: TrackSource::Microphone, ..TrackPublishOptions::default() };
    alice_room.local_participant().publish_track(LocalTrack::Audio(track), options).await.unwrap();
    let tone = tokio::spawn(async move {
        let mut phase = 0.0f32;
        loop {
            let samples: Vec<i16> = (0..480)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
                    (phase.sin() * 8000.0) as i16
                })
                .collect();
            let frame =
                AudioFrame { data: samples.into(), sample_rate: 48_000, num_channels: 1, samples_per_channel: 480 };
            if source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    });

    // Bob hears it.
    let heard = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), .. }) = bob_events.recv().await {
                let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
                while let Some(frame) = stream.next().await {
                    let peak = frame.data.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                    if peak > 2000 {
                        return peak;
                    }
                }
            }
        }
    })
    .await
    .expect("Bob never heard Alice");
    println!("Bob hears Alice (peak {heard})");
    tone.abort();

    // The server saw both join (LiveKit's webhooks), and tells the guild.
    let mut in_voice = 0;
    for _ in 0..50 {
        let guilds = call(&http, reqwest::Method::GET, "/users/@me/guilds", Some(&alice), Value::Null).await;
        let guild = guilds.as_array().unwrap().iter().find(|g| g["id"] == guild_id).unwrap().clone();
        in_voice = guild["voice_states"].as_array().unwrap().len();
        if in_voice == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(in_voice, 2, "the server should know both are in voice");
    alice_room.close().await.unwrap();
}
