//! End-to-end tests: the real router on a random port, a fresh SQLite database per
//! test (opened exactly like in production, in the system temp directory), and real
//! HTTP + WebSocket clients. Nothing to set up.
//!
//! Administration commands go through `admin::run`, exactly what the binary runs.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plume_lite_protocol::gateway::{ClientFrame, Event, Ready, ServerFrame, close_code};
use plume_lite_protocol::model::{FullGuild, Message, Status, VoiceState};
use plume_lite_server::config::LiveKitConfig;
use plume_lite_server::voice::token;
use plume_lite_server::{AppState, Config, MIGRATOR, admin, backup, db, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

struct TestApp {
    addr: SocketAddr,
    http: reqwest::Client,
    /// Direct access, to simulate the passing of time (expired sessions, invites).
    db: SqlitePool,
    config: Config,
    state: AppState,
    _dir: TempDir,
}

/// Folder of one test (database, backups, downloads), removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!("plume-lite-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Account {
    token: String,
    id: String,
}

fn account(auth: &Value) -> Account {
    Account { token: auth["token"].as_str().unwrap().to_owned(), id: auth["user"]["id"].as_str().unwrap().to_owned() }
}

async fn spawn_app(mut config: Config) -> TestApp {
    let dir = TempDir::new();
    config.backup_dir = Some(dir.path().join("backups"));
    config.downloads_dir = Some(dir.path().join("downloads"));
    let pool = db::connect(&dir.path().join("plume-lite.db")).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), config.clone());
    let app_state = state.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    TestApp { addr, http: reqwest::Client::new(), db: pool, config, state: app_state, _dir: dir }
}

fn no_rate_limit() -> Config {
    Config { rate_limit: false, open_registration: true, ..Config::default() }
}

fn closed_registration() -> Config {
    Config { rate_limit: false, ..Config::default() }
}

impl TestApp {
    async fn call(&self, method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self.http.request(method, format!("http://{}/api/v1{path}", self.addr));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn get(&self, path: &str, account: &Account) -> (StatusCode, Value) {
        self.call(Method::GET, path, Some(&account.token), None).await
    }

    async fn post(&self, path: &str, account: &Account, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(&account.token), Some(body)).await
    }

    async fn delete(&self, path: &str, account: &Account) -> StatusCode {
        self.call(Method::DELETE, path, Some(&account.token), None).await.0
    }

    async fn register(&self, username: &str) -> Account {
        let body = json!({ "username": username, "password": "motdepasse-solide" });
        let (status, value) = self.call(Method::POST, "/auth/register", None, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
        account(&value)
    }

    async fn login(&self, username: &str, password: &str) -> (StatusCode, Value) {
        let body = json!({ "username": username, "password": password });
        self.call(Method::POST, "/auth/login", None, Some(body)).await
    }

    async fn admin(&self, args: &[&str]) -> anyhow::Result<String> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        admin::run(&self.db, &self.config, &args).await
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Owner creates a guild and the others join it through an invite.
    async fn guild_with(&self, owner: &Account, others: &[&Account]) -> FullGuild {
        let (status, guild) = self.post("/guilds", owner, json!({ "name": "Rust FR" })).await;
        assert_eq!(status, StatusCode::CREATED);
        let guild: FullGuild = serde_json::from_value(guild).unwrap();
        let (_, invite) = self.post(&format!("/guilds/{}/invites", guild.guild.id), owner, json!({})).await;
        let code = invite["code"].as_str().unwrap();
        for account in others {
            let (status, _) = self.post(&format!("/invites/{code}"), account, json!({})).await;
            assert_eq!(status, StatusCode::OK);
        }
        guild
    }

    async fn gateway(&self, token: &str) -> (Gateway, Ready) {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/gateway", self.addr)).await.unwrap();
        assert!(matches!(next_frame(&mut ws).await, ServerFrame::Hello { .. }));
        let identify = ClientFrame::Identify { token: token.to_owned() };
        ws.send(WsMessage::text(serde_json::to_string(&identify).unwrap())).await.unwrap();
        let ServerFrame::Ready(ready) = next_frame(&mut ws).await else { panic!("expected READY") };
        (Gateway { ws }, *ready)
    }
}

struct Gateway {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Gateway {
    /// Waits for the first event matching `predicate`, skipping unrelated ones.
    async fn expect(&mut self, what: &str, predicate: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let frame = tokio::time::timeout_at(deadline, next_frame(&mut self.ws))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if let ServerFrame::Dispatch(event) = frame
                && predicate(&event)
            {
                return *event;
            }
        }
    }
}

async fn next_frame(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> ServerFrame {
    loop {
        match ws.next().await.expect("socket closed").expect("socket error") {
            WsMessage::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            WsMessage::Close(frame) => panic!("unexpected close: {frame:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn auth_lifecycle() {
    let app = spawn_app(no_rate_limit()).await;
    let alice = app.register("alice").await;

    let creds = |username: &str, password: &str| json!({ "username": username, "password": password });
    let (status, body) = app.call(Method::POST, "/auth/register", None, Some(creds("ALICE", "un-autre-mdp"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "usernames are case-insensitive");
    assert_eq!(body["code"], "conflict");

    let (status, body) = app.call(Method::POST, "/auth/register", None, Some(creds("a b", "motdepasse"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
    let (status, _) = app.call(Method::POST, "/auth/register", None, Some(creds("bob", "court"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = app.call(Method::POST, "/auth/login", None, Some(creds("alice", "mauvais-mdp"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
    let (status, _) = app.call(Method::POST, "/auth/login", None, Some(creds("inconnu", "motdepasse"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = app.call(Method::POST, "/auth/login", None, Some(creds("Alice", "motdepasse-solide"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["id"], alice.id.as_str());
    let second = Account { token: body["token"].as_str().unwrap().to_owned(), id: alice.id.clone() };

    let (status, me) = app.get("/users/@me", &alice).await;
    assert_eq!((status, me["username"].as_str()), (StatusCode::OK, Some("alice")));

    assert_eq!(app.call(Method::POST, "/auth/logout", Some(&alice.token), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/users/@me", &alice).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/users/@me", &second).await.0, StatusCode::OK, "other sessions survive");

    // An expired session is refused, then purged.
    sqlx::query("UPDATE sessions SET expires_at = unixepoch() - 1").execute(&app.db).await.unwrap();
    assert_eq!(app.get("/users/@me", &second).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(db::users::purge_expired_sessions(&app.db).await.unwrap(), 1);
    assert_eq!(app.call(Method::GET, "/users/@me", None, None).await.0, StatusCode::UNAUTHORIZED);

    let (status, body) = app.call(Method::GET, "/nope", None, None).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

#[tokio::test]
async fn guilds_invites_and_permissions() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, carol) = (app.register("alice").await, app.register("bob").await, app.register("carol").await);

    let (status, guild) = app.post("/guilds", &alice, json!({ "name": "  Rust FR  " })).await;
    assert_eq!(status, StatusCode::CREATED);
    let guild: FullGuild = serde_json::from_value(guild).unwrap();
    assert_eq!(guild.guild.name, "Rust FR");
    assert_eq!(guild.channels.len(), 1);
    assert_eq!(guild.channels[0].name, "général");
    let gid = guild.guild.id;

    // Non-members cannot even learn that the guild exists.
    assert_eq!(app.get(&format!("/guilds/{gid}"), &bob).await.0, StatusCode::NOT_FOUND);

    // Single-use invite.
    let (status, invite) = app.post(&format!("/guilds/{gid}/invites"), &alice, json!({ "max_uses": 1 })).await;
    assert_eq!(status, StatusCode::CREATED);
    let code = invite["code"].as_str().unwrap();
    assert_eq!(code.len(), 8);
    let (status, preview) = app.get(&format!("/invites/{code}"), &bob).await;
    assert_eq!((status, preview["member_count"].as_i64()), (StatusCode::OK, Some(1)));
    let (status, joined) = app.post(&format!("/invites/{code}"), &bob, json!({})).await;
    assert_eq!((status, joined["id"].as_str()), (StatusCode::OK, Some(gid.to_string().as_str())));
    assert_eq!(joined["member_count"], 2);
    assert_eq!(app.post(&format!("/invites/{code}"), &carol, json!({})).await.0, StatusCode::NOT_FOUND, "exhausted");

    // Invite with an expiry: usable until it expires.
    let (status, invite) = app.post(&format!("/guilds/{gid}/invites"), &alice, json!({ "max_age_secs": 3600 })).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(invite["expires_at"].is_string());
    let code = invite["code"].as_str().unwrap();
    assert_eq!(app.get(&format!("/invites/{code}"), &carol).await.0, StatusCode::OK);
    sqlx::query("UPDATE invites SET expires_at = unixepoch() - 1 WHERE code = $1")
        .bind(code)
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(app.get(&format!("/invites/{code}"), &carol).await.0, StatusCode::NOT_FOUND, "expired");
    assert_eq!(app.post(&format!("/invites/{code}"), &carol, json!({})).await.0, StatusCode::NOT_FOUND);

    // Only the owner manages channels; names are normalised.
    let (status, _) = app.post(&format!("/guilds/{gid}/channels"), &bob, json!({ "name": "pirate" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, channel) =
        app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "Annonces Importantes" })).await;
    assert_eq!((status, channel["name"].as_str()), (StatusCode::CREATED, Some("annonces-importantes")));
    assert_eq!(channel["position"], 1);
    let (_, channels) = app.get(&format!("/guilds/{gid}/channels"), &bob).await;
    assert_eq!(channels.as_array().unwrap().len(), 2);
    assert_eq!(
        app.delete(&format!("/channels/{}", channel["id"].as_str().unwrap()), &bob).await,
        StatusCode::FORBIDDEN
    );

    let (status, members) = app.get(&format!("/guilds/{gid}/members"), &bob).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(members.as_array().unwrap().len(), 2);
    assert_eq!(members[0]["status"], "offline");

    // Leaving / kicking rules.
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &alice).await, StatusCode::BAD_REQUEST);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/{}", alice.id), &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &bob).await, StatusCode::NO_CONTENT);
    assert_eq!(app.get(&format!("/guilds/{gid}"), &bob).await.0, StatusCode::NOT_FOUND);

    assert_eq!(app.delete(&format!("/guilds/{gid}"), &bob).await, StatusCode::NOT_FOUND);
    assert_eq!(app.delete(&format!("/guilds/{gid}"), &alice).await, StatusCode::NO_CONTENT);
    assert_eq!(app.get(&format!("/guilds/{gid}"), &alice).await.0, StatusCode::NOT_FOUND);
    let (_, mine) = app.get("/users/@me/guilds", &alice).await;
    assert_eq!(mine, json!([]));
}

#[tokio::test]
async fn messages_pagination_and_moderation() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob) = (app.register("alice").await, app.register("bob").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let channel = guild.channels[0].id;
    let path = format!("/channels/{channel}/messages");

    for i in 0..120 {
        let (status, _) = app.post(&path, &alice, json!({ "content": format!("message {i}") })).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let page = |value: Value| serde_json::from_value::<Vec<Message>>(value).unwrap();
    let latest = page(app.get(&path, &bob).await.1);
    assert_eq!(latest.len(), 50);
    assert_eq!(latest.first().unwrap().content, "message 70");
    assert_eq!(latest.last().unwrap().content, "message 119");
    assert!(latest.windows(2).all(|w| w[0].id < w[1].id), "oldest → newest");

    let older = page(app.get(&format!("{path}?before={}&limit=100", latest[0].id), &bob).await.1);
    assert_eq!(older.len(), 70);
    assert_eq!(older.first().unwrap().content, "message 0");
    let newer = page(app.get(&format!("{path}?after={}&limit=5", older[0].id), &bob).await.1);
    assert_eq!(
        newer.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        ["message 1", "message 2", "message 3", "message 4", "message 5"]
    );
    let (status, _) = app.get(&format!("{path}?before=1&after=2"), &bob).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.get(&format!("{path}?before=abc"), &bob).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Validation.
    assert_eq!(app.post(&path, &bob, json!({ "content": "   " })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(app.post(&path, &bob, json!({ "content": "x".repeat(4001) })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(app.post(&path, &bob, json!({ "content": "nul\u{0}" })).await.0, StatusCode::BAD_REQUEST);

    // Edit: author only. Delete: author or guild owner.
    let (_, bobs) = app.post(&path, &bob, json!({ "content": "salut 👋" })).await;
    let bobs_id = bobs["id"].as_str().unwrap();
    let edit = |content: &str| json!({ "content": content });
    let (status, _) =
        app.call(Method::PATCH, &format!("{path}/{bobs_id}"), Some(&alice.token), Some(edit("hack"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, edited) =
        app.call(Method::PATCH, &format!("{path}/{bobs_id}"), Some(&bob.token), Some(edit("salut à tous 👋"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["content"], "salut à tous 👋");
    assert_eq!(edited["author"]["username"], "bob");
    assert!(edited["edited_at"].is_string());

    let alices_id = latest[0].id;
    assert_eq!(app.delete(&format!("{path}/{alices_id}"), &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("{path}/{bobs_id}"), &alice).await, StatusCode::NO_CONTENT, "owner moderates");
    assert_eq!(app.delete(&format!("{path}/{bobs_id}"), &bob).await, StatusCode::NOT_FOUND);

    // Outsiders see nothing.
    let eve = app.register("eve").await;
    assert_eq!(app.get(&path, &eve).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.post(&path, &eve, json!({ "content": "spam" })).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replies_mentions_and_long_messages() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let path = format!("/channels/{}/messages", guild.channels[0].id);
    let mentioned = |id: &str| {
        let (db, id) = (app.db.clone(), id.parse::<i64>().unwrap());
        async move {
            let rows: Vec<(i64,)> =
                sqlx::query_as("SELECT user_id FROM message_mentions WHERE message_id = $1 ORDER BY user_id")
                    .bind(id)
                    .fetch_all(&db)
                    .await
                    .unwrap();
            rows.into_iter().map(|(id,)| id.to_string()).collect::<Vec<_>>()
        }
    };

    // Up to 4000 characters (not bytes).
    let (status, long) = app.post(&path, &bob, json!({ "content": "é".repeat(4000) })).await;
    assert_eq!(status, StatusCode::CREATED);

    // Only members are recorded as mentioned; @everyone only counts from the guild owner.
    let content = format!("salut <@{}> et <@{}> @everyone", alice.id, eve.id);
    let (_, from_bob) = app.post(&path, &bob, json!({ "content": content })).await;
    let from_bob_id = from_bob["id"].as_str().unwrap().to_owned();
    assert!(from_bob.get("mention_everyone").is_none());
    assert_eq!(mentioned(&from_bob_id).await, std::slice::from_ref(&alice.id));
    let (_, from_alice) = app.post(&path, &alice, json!({ "content": "réunion @everyone" })).await;
    assert_eq!(from_alice["mention_everyone"], true);

    // Replies carry the beginning of the answered message, in the same channel only.
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let (status, reply) =
        app.post(&path, &alice, json!({ "content": "oui", "reply_to": long["id"], "nonce": "n1" })).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(reply["reply_to"]["author"]["username"], "bob");
    assert_eq!(reply["reply_to"]["excerpt"].as_str().unwrap().chars().count(), 200);
    let Event::MessageCreate(pushed) = bob_gw.expect("the reply", |e| matches!(e, Event::MessageCreate(_))).await
    else {
        unreachable!()
    };
    assert_eq!(pushed.reply_to.unwrap().id.to_string(), long["id"].as_str().unwrap());
    let (_, other) = app.post(&format!("/guilds/{}/channels", guild.guild.id), &alice, json!({ "name": "x" })).await;
    let other_path = format!("/channels/{}/messages", other["id"].as_str().unwrap());
    let (status, _) = app.post(&other_path, &alice, json!({ "content": "non", "reply_to": long["id"] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Editing rewrites the mentions.
    let (_, bobs_reply) = app.post(&path, &bob, json!({ "content": "re", "reply_to": from_bob_id })).await;
    let edit = json!({ "content": format!("<@{}> seulement", bob.id) });
    let (status, edited) =
        app.call(Method::PATCH, &format!("{path}/{from_bob_id}"), Some(&bob.token), Some(edit)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(edited.get("mention_everyone").is_none());
    assert_eq!(mentioned(&from_bob_id).await, std::slice::from_ref(&bob.id));

    // Once the answered message is deleted, the reply says so.
    assert_eq!(app.delete(&format!("{path}/{from_bob_id}"), &bob).await, StatusCode::NO_CONTENT);
    let (_, history) = app.get(&path, &alice).await;
    let history = serde_json::from_value::<Vec<Message>>(history).unwrap();
    let orphan = history.iter().find(|m| m.id.to_string() == bobs_reply["id"].as_str().unwrap()).unwrap();
    let replied = orphan.reply_to.as_ref().unwrap();
    assert_eq!((replied.id.to_string(), replied.author.is_none()), (from_bob_id.clone(), true));
    assert!(mentioned(&from_bob_id).await.is_empty(), "deleted with the message");
}

#[tokio::test]
async fn gateway_delivers_realtime_events() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob) = (app.register("alice").await, app.register("bob").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let gid = guild.guild.id;
    let channel = guild.channels[0].id;

    let (mut bob_gw, ready) = app.gateway(&bob.token).await;
    assert_eq!(ready.user.username, "bob");
    assert_eq!(ready.guilds.len(), 1);
    assert_eq!(ready.guilds[0].guild.id, gid);
    assert_eq!(ready.guilds[0].member_count, 2);

    // Presence: alice comes online.
    let (alice_gw, _) = app.gateway(&alice.token).await;
    bob_gw
        .expect("alice online", |e| matches!(e, Event::PresenceUpdate { status: Status::Online, user_id } if user_id.to_string() == alice.id))
        .await;
    let (_, members) = app.get(&format!("/guilds/{gid}/members"), &bob).await;
    assert!(members.as_array().unwrap().iter().all(|m| m["status"] == "online"));

    // Messages carry the sender's nonce back.
    let (_, sent) = app
        .post(&format!("/channels/{channel}/messages"), &alice, json!({ "content": "temps réel !", "nonce": "n-1" }))
        .await;
    let Event::MessageCreate(message) = bob_gw.expect("MESSAGE_CREATE", |e| matches!(e, Event::MessageCreate(_))).await
    else {
        unreachable!()
    };
    assert_eq!(message.content, "temps réel !");
    assert_eq!(message.nonce.as_deref(), Some("n-1"));
    assert_eq!(message.id.to_string(), sent["id"].as_str().unwrap());

    app.call(Method::POST, &format!("/channels/{channel}/typing"), Some(&alice.token), None).await;
    bob_gw.expect("TYPING_START", |e| matches!(e, Event::TypingStart { .. })).await;

    let (_, created) = app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "nouveau" })).await;
    bob_gw.expect("CHANNEL_CREATE", |e| matches!(e, Event::ChannelCreate(c) if c.name == "nouveau")).await;
    app.delete(&format!("/channels/{}", created["id"].as_str().unwrap()), &alice).await;
    bob_gw.expect("CHANNEL_DELETE", |e| matches!(e, Event::ChannelDelete { .. })).await;

    // A newcomer joins: existing members are told; the newcomer gets the guild.
    let carol = app.register("carol").await;
    let (mut carol_gw, carol_ready) = app.gateway(&carol.token).await;
    assert!(carol_ready.guilds.is_empty());
    let (_, invite) = app.post(&format!("/guilds/{gid}/invites"), &bob, json!({ "max_age_secs": 3600 })).await;
    app.post(&format!("/invites/{}", invite["code"].as_str().unwrap()), &carol, json!({})).await;
    bob_gw
        .expect(
            "GUILD_MEMBER_ADD",
            |e| matches!(e, Event::GuildMemberAdd { member, .. } if member.user.username == "carol"),
        )
        .await;
    carol_gw.expect("GUILD_CREATE", |e| matches!(e, Event::GuildCreate(g) if g.guild.id == gid)).await;
    // …and immediately receives the guild's traffic.
    app.post(&format!("/channels/{channel}/messages"), &bob, json!({ "content": "bienvenue carol" })).await;
    carol_gw
        .expect("MESSAGE_CREATE for carol", |e| matches!(e, Event::MessageCreate(m) if m.content == "bienvenue carol"))
        .await;

    // Presence: alice goes offline when her last session closes.
    drop(alice_gw);
    bob_gw
        .expect("alice offline", |e| matches!(e, Event::PresenceUpdate { status: Status::Offline, user_id } if user_id.to_string() == alice.id))
        .await;

    // Deleting the guild reaches every member.
    app.delete(&format!("/guilds/{gid}"), &alice).await;
    bob_gw.expect("GUILD_DELETE", |e| matches!(e, Event::GuildDelete { id } if *id == gid)).await;
    carol_gw.expect("GUILD_DELETE", |e| matches!(e, Event::GuildDelete { id } if *id == gid)).await;
}

#[tokio::test]
async fn gateway_rejects_invalid_sessions() {
    let app = spawn_app(no_rate_limit()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/gateway", app.addr)).await.unwrap();
    assert!(matches!(next_frame(&mut ws).await, ServerFrame::Hello { .. }));
    let identify = serde_json::to_string(&ClientFrame::Identify { token: "forged".into() }).unwrap();
    ws.send(WsMessage::text(identify)).await.unwrap();
    match ws.next().await.unwrap().unwrap() {
        WsMessage::Close(Some(frame)) => assert_eq!(u16::from(frame.code), close_code::AUTHENTICATION_FAILED),
        other => panic!("expected close frame, got {other:?}"),
    }
}

#[tokio::test]
async fn message_flood_is_rate_limited() {
    let app = spawn_app(Config { open_registration: true, ..Config::default() }).await;
    let alice = app.register("alice").await;
    let guild = app.guild_with(&alice, &[]).await;
    let path = format!("/channels/{}/messages", guild.channels[0].id);

    for i in 0..5 {
        assert_eq!(app.post(&path, &alice, json!({ "content": format!("{i}") })).await.0, StatusCode::CREATED);
    }
    let (status, body) = app.post(&path, &alice, json!({ "content": "de trop" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    assert!(body["retry_after_ms"].as_u64().unwrap() > 0);
}

/// The code printed by `plume-lite-server invite`.
fn printed_code(output: &str) -> String {
    let line = output.lines().find_map(|line| line.strip_prefix("Code d'inscription : ")).expect(output);
    line.trim().to_owned()
}

#[tokio::test]
async fn registration_requires_a_code() {
    let app = spawn_app(closed_registration()).await;
    let register = |name: &str, code: Option<&str>| {
        let body = json!({ "username": name, "password": "motdepasse-solide", "invite_code": code });
        app.call(Method::POST, "/auth/register", None, Some(body))
    };

    let (status, body) = register("alice", None).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
    assert_eq!(register("alice", Some("inventé")).await.0, StatusCode::FORBIDDEN);
    let (_, info) = app.call(Method::GET, "/instance", None, None).await;
    assert_eq!(info["open_registration"], false);

    // A code for two people.
    let code = printed_code(&app.admin(&["invite", "--uses", "2", "--days", "3"]).await.unwrap());
    assert_eq!(code.len(), 10);
    let listed = app.admin(&["invites"]).await.unwrap();
    assert!(listed.lines().any(|line| line.starts_with(&code) && line.contains(" 0/2")), "{listed}");
    assert_eq!(register("alice", Some(&code)).await.0, StatusCode::CREATED);
    assert_eq!(register("ALICE", Some(&code)).await.0, StatusCode::CONFLICT, "a taken name does not use the code");
    assert_eq!(register("bob", Some(&format!(" {code} "))).await.0, StatusCode::CREATED);
    assert_eq!(register("carol", Some(&code)).await.0, StatusCode::FORBIDDEN, "used up");
    assert_eq!(app.admin(&["invites"]).await.unwrap(), "Aucun code d'inscription valable.");

    // Expired, and revoked, codes.
    let expired = printed_code(&app.admin(&["invite"]).await.unwrap());
    sqlx::query("UPDATE registration_codes SET expires_at = unixepoch() - 8 * 86400 WHERE code = $1")
        .bind(&expired)
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(register("carol", Some(&expired)).await.0, StatusCode::FORBIDDEN);
    let revoked = printed_code(&app.admin(&["invite"]).await.unwrap());
    assert_eq!(app.admin(&["revoke-invite", &revoked]).await.unwrap(), format!("Code {revoked} supprimé."));
    assert_eq!(register("carol", Some(&revoked)).await.0, StatusCode::FORBIDDEN);
    assert!(app.admin(&["revoke-invite", &revoked]).await.is_err());
    assert!(app.admin(&["invite", "--uses", "0"]).await.is_err());

    // Housekeeping removes the used-up and the long-expired codes.
    assert_eq!(db::registration::purge(&app.db).await.unwrap(), 2);
}

#[tokio::test]
async fn administration_commands() {
    let app = spawn_app(no_rate_limit()).await;
    let alice = app.register("alice").await;
    let (status, _) = app.login("alice", "motdepasse-solide").await;
    assert_eq!(status, StatusCode::OK);

    let users = app.admin(&["users"]).await.unwrap();
    let line = users.lines().find(|line| line.starts_with("alice")).unwrap();
    assert!(line.contains(" 2  actif"), "{line}");

    // New password: every session closes, the old password stops working.
    let output = app.admin(&["reset-password", "Alice"]).await.unwrap();
    let password = output.lines().next().unwrap().rsplit(' ').next().unwrap().to_owned();
    assert!(output.contains("2 session(s) fermée(s)"));
    assert_eq!(app.get("/users/@me", &alice).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login("alice", "motdepasse-solide").await.0, StatusCode::UNAUTHORIZED);
    let (status, auth) = app.login("alice", &password).await;
    assert_eq!(status, StatusCode::OK);
    let alice = account(&auth);

    // Disabled: sessions closed, logging in refused (with the right password only).
    assert!(app.admin(&["disable", "alice"]).await.unwrap().contains("désactivé (1 session(s)"));
    assert_eq!(app.get("/users/@me", &alice).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login("alice", "mauvais-mdp").await.0, StatusCode::UNAUTHORIZED);
    let (status, body) = app.login("alice", &password).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
    assert!(app.admin(&["users"]).await.unwrap().contains("désactivé"));
    assert!(app.admin(&["disable", "alice"]).await.unwrap().contains("déjà"));
    assert_eq!(app.admin(&["enable", "alice"]).await.unwrap(), "Compte alice réactivé.");
    assert_eq!(app.login("alice", &password).await.0, StatusCode::OK);

    assert!(app.admin(&["disable", "personne"]).await.is_err());
    assert!(app.admin(&["n-importe-quoi"]).await.is_err());
    assert!(app.admin(&["help"]).await.unwrap().contains("reset-password"));
}

#[tokio::test]
async fn backups() {
    let app = spawn_app(no_rate_limit()).await;
    app.register("alice").await;
    let dir = app.config.backup_dir.clone().unwrap();

    let output = app.admin(&["backup"]).await.unwrap();
    assert!(output.starts_with("Sauvegarde écrite"), "{output}");
    let first = backup::list(&dir).await.unwrap();
    assert_eq!(first.len(), 1);
    // A backup is a complete database.
    let copy = db::connect(&first[0]).await.unwrap();
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&copy).await.unwrap();
    assert_eq!(users, 1);
    copy.close().await;

    // The daily one is not due yet; older ones are pruned.
    assert!(backup::daily(&app.db, &dir, 7).await.unwrap().is_none());
    backup::create(&app.db, &dir).await.unwrap();
    backup::create(&app.db, &dir).await.unwrap();
    assert_eq!(backup::prune(&dir, 2).await.unwrap(), 1);
    assert_eq!(backup::list(&dir).await.unwrap().len(), 2);
}

#[tokio::test]
async fn password_and_devices() {
    let app = spawn_app(no_rate_limit()).await;
    let alice = app.register("alice").await;
    let bob = app.register("bob").await;

    // A second device, which names itself.
    let response = app
        .http
        .post(format!("http://{}/api/v1/auth/login", app.addr))
        .header("User-Agent", "plume-lite-desktop/0.3.0 (windows)")
        .json(&json!({ "username": "alice", "password": "motdepasse-solide" }))
        .send()
        .await
        .unwrap();
    let laptop = account(&response.json::<Value>().await.unwrap());

    let (status, sessions) = app.get("/users/@me/sessions", &alice).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = sessions.as_array().unwrap().clone();
    assert_eq!(sessions.len(), 2);
    let other = sessions.iter().find(|s| s["current"] == false).unwrap();
    assert_eq!(other["device"], "plume-lite-desktop/0.3.0 (windows)");
    let other_id = other["id"].as_str().unwrap();

    // Last use is refreshed (at most every few minutes).
    sqlx::query("UPDATE sessions SET last_seen_at = unixepoch() - 3600").execute(&app.db).await.unwrap();
    app.get("/users/@me", &alice).await;
    let (_, sessions) = app.get("/users/@me/sessions", &alice).await;
    assert_eq!(sessions[0]["current"], true, "most recently used first");

    // Only one's own sessions can be revoked; a revoked device's live connection closes.
    let (mut gateway, _) = app.gateway(&laptop.token).await;
    assert_eq!(app.delete(&format!("/users/@me/sessions/{other_id}"), &bob).await, StatusCode::NOT_FOUND);
    assert_eq!(app.delete(&format!("/users/@me/sessions/{other_id}"), &alice).await, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/users/@me", &laptop).await.0, StatusCode::UNAUTHORIZED);
    let heartbeat = serde_json::to_string(&ClientFrame::Heartbeat).unwrap();
    gateway.ws.send(WsMessage::text(heartbeat)).await.unwrap();
    loop {
        match gateway.ws.next().await.unwrap().unwrap() {
            WsMessage::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), close_code::AUTHENTICATION_FAILED);
                break;
            }
            WsMessage::Close(None) => panic!("close frame without a code"),
            _ => {}
        }
    }

    // Changing the password logs out every other device.
    let (status, _) = app.login("alice", "motdepasse-solide").await;
    assert_eq!(status, StatusCode::OK);
    let change = |current: &str, new: &str| {
        let body = json!({ "current_password": current, "new_password": new });
        app.call(Method::PUT, "/users/@me/password", Some(&alice.token), Some(body))
    };
    assert_eq!(change("mauvais-mdp", "nouveau-mdp-solide").await.0, StatusCode::FORBIDDEN);
    assert_eq!(change("motdepasse-solide", "court").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(change("motdepasse-solide", "nouveau-mdp-solide").await.0, StatusCode::NO_CONTENT);
    let (_, sessions) = app.get("/users/@me/sessions", &alice).await;
    assert_eq!(sessions.as_array().unwrap().len(), 1, "only this device stays logged in");
    assert_eq!(app.login("alice", "motdepasse-solide").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login("alice", "nouveau-mdp-solide").await.0, StatusCode::OK);
}

#[tokio::test]
async fn instance_page_and_downloads() {
    let app = spawn_app(no_rate_limit()).await;
    let (_, info) = app.call(Method::GET, "/instance", None, None).await;
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["open_registration"], true);
    assert!(info.get("client").is_none(), "no client to hand out yet");
    let page = app.http.get(app.url("/")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("pas encore disponible"));

    let downloads = app.config.downloads_dir.clone().unwrap();
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(downloads.join("plume-lite.exe"), b"MZ fake client").unwrap();

    let (_, info) = app.call(Method::GET, "/instance", None, None).await;
    assert_eq!(info["client"]["download_path"], "/download/plume-lite.exe");
    assert_eq!(info["client"]["version"], env!("CARGO_PKG_VERSION"));
    let file = app.http.get(app.url("/download/plume-lite.exe")).send().await.unwrap();
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.bytes().await.unwrap().as_ref(), b"MZ fake client");
    let escape = app.http.get(app.url("/download/..%2fplume-lite.db")).send().await.unwrap();
    assert_eq!(escape.status(), StatusCode::NOT_FOUND);

    let home = app.http.get(app.url("/")).send().await.unwrap();
    assert!(home.headers()["content-security-policy"].to_str().unwrap().starts_with("default-src 'none'"));
    let page = home.text().await.unwrap();
    assert!(page.contains(r#"href="/download/plume-lite.exe""#));
    assert!(page.contains(&format!("<code>http://{}</code>", app.addr)));
}

// --- Voice ---------------------------------------------------------------------------------------

const LIVEKIT_KEY: &str = "test-key";
const LIVEKIT_SECRET: &str = "test-secret-long-enough-for-hmac-sha256";

/// A stand-in for LiveKit's RoomService API: records the calls it receives and lists the
/// rooms it is told about.
/// `(room, identities)`, as LiveKit's ListRooms and ListParticipants would answer.
type Rooms = Vec<(String, Vec<String>)>;

#[derive(Clone, Default)]
struct MockLiveKit {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    rooms: Arc<Mutex<Rooms>>,
}

impl MockLiveKit {
    async fn spawn() -> (Self, String) {
        let mock = Self::default();
        let state = mock.clone();
        let app = axum::Router::new().route(
            "/twirp/livekit.RoomService/{method}",
            axum::routing::post(
                move |axum::extract::Path(method): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<Value>| {
                    let mock = state.clone();
                    async move {
                        // Every call carries an admin token signed with the shared secret.
                        let auth = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ");
                        let claims = token::verify(auth, LIVEKIT_SECRET).expect("badly signed admin token");
                        assert!(claims.video.unwrap().room_admin);
                        mock.calls.lock().unwrap().push((method.clone(), body.clone()));
                        let rooms = mock.rooms.lock().unwrap().clone();
                        axum::Json(match method.as_str() {
                            "ListRooms" => json!({ "rooms": rooms.iter().map(|(name, _)| json!({ "name": name })).collect::<Vec<_>>() }),
                            "ListParticipants" => {
                                let identities = rooms.iter().find(|(name, _)| *name == body["room"]).map(|(_, ids)| ids.clone()).unwrap_or_default();
                                json!({ "participants": identities.iter().map(|id| json!({ "identity": id })).collect::<Vec<_>>() })
                            }
                            _ => json!({}),
                        })
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (mock, format!("http://{addr}"))
    }

    /// Waits for a call (some are made in the background).
    async fn expect(&self, method: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..100 {
            let found = self.calls.lock().unwrap().iter().find(|(m, body)| m == method && predicate(body)).cloned();
            if let Some((_, body)) = found {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("LiveKit never received {method}; calls: {:?}", self.calls.lock().unwrap());
    }
}

fn voice_config(api_url: String) -> Config {
    Config {
        livekit: Some(LiveKitConfig {
            url: "wss://voice.example".into(),
            api_url,
            api_key: LIVEKIT_KEY.into(),
            api_secret: LIVEKIT_SECRET.into(),
        }),
        ..no_rate_limit()
    }
}

impl TestApp {
    /// What LiveKit sends when someone joins or leaves a room.
    async fn webhook(&self, event: &str, room: &str, identity: &str, secret: &str) -> StatusCode {
        let body =
            json!({ "event": event, "room": { "name": room }, "participant": { "identity": identity } }).to_string();
        let mut claims = token::Claims::new(LIVEKIT_KEY, 60);
        claims.sha256 = Some(token::body_hash(body.as_bytes()));
        let response = self
            .http
            .post(self.url("/api/v1/voice/webhook"))
            .header("Authorization", token::sign(&claims, secret))
            .header("Content-Type", "application/webhook+json")
            .body(body)
            .send()
            .await
            .unwrap();
        response.status()
    }
}

impl Gateway {
    async fn voice_update(&mut self, what: &str, predicate: impl Fn(&VoiceState) -> bool) -> VoiceState {
        let Event::VoiceStateUpdate(state) =
            self.expect(what, |e| matches!(e, Event::VoiceStateUpdate(s) if predicate(s))).await
        else {
            unreachable!()
        };
        state
    }
}

#[tokio::test]
async fn voice_channels() {
    let (livekit, api_url) = MockLiveKit::spawn().await;
    let app = spawn_app(voice_config(api_url)).await;
    let (alice, bob) = (app.register("alice").await, app.register("bob").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let (gid, text) = (guild.guild.id, guild.channels[0].id);

    // A voice channel; only text and voice exist.
    let (status, channel) =
        app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "Vocal", "kind": "voice" })).await;
    assert_eq!((status, channel["kind"].as_str()), (StatusCode::CREATED, Some("voice")));
    let voice = channel["id"].as_str().unwrap().to_owned();
    let (status, _) =
        app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "x", "kind": "hologram" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Joining: a token for this room only, signed with the shared secret.
    assert_eq!(app.post(&format!("/channels/{text}/voice"), &bob, json!({})).await.0, StatusCode::BAD_REQUEST);
    let eve = app.register("eve").await;
    assert_eq!(app.post(&format!("/channels/{voice}/voice"), &eve, json!({})).await.0, StatusCode::NOT_FOUND);
    let (status, join) = app.post(&format!("/channels/{voice}/voice"), &bob, json!({})).await;
    assert_eq!((status, join["url"].as_str()), (StatusCode::OK, Some("wss://voice.example")));
    let claims = token::verify(join["token"].as_str().unwrap(), LIVEKIT_SECRET).unwrap();
    let grant = claims.video.unwrap();
    assert_eq!((claims.sub.as_str(), claims.name.as_str(), claims.iss.as_str()), (bob.id.as_str(), "bob", LIVEKIT_KEY));
    assert_eq!((grant.room.as_str(), grant.room_join, grant.can_publish), (voice.as_str(), true, Some(true)));

    // LiveKit says bob is in: everyone sees it. Forged webhooks are refused.
    let (mut alice_gw, _) = app.gateway(&alice.token).await;
    assert_eq!(
        app.webhook("participant_joined", &voice, &bob.id, "wrong-secret-wrong-secret-wrong!").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.webhook("participant_joined", &voice, &bob.id, LIVEKIT_SECRET).await, StatusCode::OK);
    let state = alice_gw.voice_update("bob joined", |s| s.channel_id.is_some()).await;
    assert_eq!((state.user_id.to_string(), state.channel_id.unwrap().to_string()), (bob.id.clone(), voice.clone()));
    let (_, guilds) = app.get("/users/@me/guilds", &alice).await;
    assert_eq!(guilds[0]["voice_states"][0]["user_id"], bob.id.as_str());

    // His own switches.
    let body = json!({ "self_mute": true, "self_deaf": false });
    assert_eq!(
        app.call(Method::PATCH, "/users/@me/voice", Some(&bob.token), Some(body)).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(alice_gw.voice_update("bob muted himself", |s| s.self_mute).await.channel_id.is_some());

    // The owner mutes him: LiveKit stops his microphone, and new tokens cannot publish.
    let mute = |muted: bool| json!({ "muted": muted });
    let (status, _) = app
        .call(Method::PUT, &format!("/guilds/{gid}/voice/{}/mute", alice.id), Some(&bob.token), Some(mute(true)))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "owner only");
    let (status, _) = app
        .call(Method::PUT, &format!("/guilds/{gid}/voice/{}/mute", bob.id), Some(&alice.token), Some(mute(true)))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let update = livekit.expect("UpdateParticipant", |b| b["identity"] == bob.id.as_str()).await;
    assert_eq!(update["permission"]["canPublish"], false);
    assert!(alice_gw.voice_update("bob server-muted", |s| s.server_mute).await.self_mute);
    let (_, join) = app.post(&format!("/channels/{voice}/voice"), &bob, json!({})).await;
    assert_eq!(
        token::verify(join["token"].as_str().unwrap(), LIVEKIT_SECRET).unwrap().video.unwrap().can_publish,
        Some(false)
    );

    // The owner moves him to another voice channel: only his client is told to switch, and
    // LiveKit's webhooks then report where he is.
    let (_, other) =
        app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "Jeux", "kind": "voice" })).await;
    let other = other["id"].as_str().unwrap().to_owned();
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let app_ref = &app;
    let move_to = |user: &str, channel: &str, by: &Account| {
        let (path, body) = (format!("/guilds/{gid}/voice/{user}/move"), json!({ "channel_id": channel }));
        let token = by.token.clone();
        async move { app_ref.call(Method::POST, &path, Some(&token), Some(body)).await.0 }
    };
    assert_eq!(move_to(&alice.id, &other, &bob).await, StatusCode::FORBIDDEN, "owner only");
    assert_eq!(move_to(&bob.id, &text.to_string(), &alice).await, StatusCode::BAD_REQUEST, "voice channels only");
    let elsewhere = app.guild_with(&eve, &[]).await;
    let (_, foreign) = app
        .post(&format!("/guilds/{}/channels", elsewhere.guild.id), &eve, json!({ "name": "v", "kind": "voice" }))
        .await;
    assert_eq!(move_to(&bob.id, foreign["id"].as_str().unwrap(), &alice).await, StatusCode::NOT_FOUND);
    assert_eq!(move_to(&alice.id, &other, &alice).await, StatusCode::NOT_FOUND, "not in voice");
    assert_eq!(move_to(&bob.id, &other, &alice).await, StatusCode::NO_CONTENT);
    let moved = bob_gw.expect("bob told to move", |e| matches!(e, Event::VoiceMove { .. })).await;
    assert_eq!(moved, Event::VoiceMove { guild_id: gid, channel_id: other.parse().unwrap() });
    app.webhook("participant_joined", &other, &bob.id, LIVEKIT_SECRET).await;
    app.webhook("participant_left", &voice, &bob.id, LIVEKIT_SECRET).await;
    let state = alice_gw.voice_update("bob moved", |s| s.channel_id.is_some_and(|c| c.to_string() == other)).await;
    assert!(state.server_mute, "a server mute follows him");
    assert_eq!(app.state.voice.of_user(bob.id.parse().unwrap()).unwrap().channel_id.unwrap().to_string(), other);

    // The owner takes him out of voice.
    assert_eq!(
        app.call(Method::POST, &format!("/guilds/{gid}/voice/{}/disconnect", bob.id), Some(&alice.token), None).await.0,
        StatusCode::NO_CONTENT
    );
    livekit.expect("RemoveParticipant", |b| b["identity"] == bob.id.as_str() && b["room"] == other.as_str()).await;
    alice_gw.voice_update("bob left", |s| s.channel_id.is_none()).await;
    assert_eq!(
        app.call(Method::POST, &format!("/guilds/{gid}/voice/{}/disconnect", bob.id), Some(&alice.token), None).await.0,
        StatusCode::NOT_FOUND
    );

    // Someone who is not (or no longer) a member is thrown out of the room.
    assert_eq!(app.webhook("participant_joined", &voice, &eve.id, LIVEKIT_SECRET).await, StatusCode::OK);
    livekit.expect("RemoveParticipant", |b| b["identity"] == eve.id.as_str()).await;

    // Leaving the guild leaves its voice channels.
    app.webhook("participant_joined", &voice, &bob.id, LIVEKIT_SECRET).await;
    alice_gw.voice_update("bob back", |s| s.channel_id.is_some()).await;
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &bob).await, StatusCode::NO_CONTENT);
    alice_gw.voice_update("bob left with the guild", |s| s.channel_id.is_none()).await;

    // After a restart, who is in voice comes back from LiveKit.
    livekit.rooms.lock().unwrap().push((voice.clone(), vec![alice.id.clone(), "not-a-user".into()]));
    assert_eq!(plume_lite_server::routes::voice::resync(&app.state).await.unwrap(), 1);
    alice_gw.voice_update("alice found in her room", |s| s.user_id.to_string() == alice.id).await;
    app.webhook("participant_left", &voice, &alice.id, LIVEKIT_SECRET).await;
    alice_gw.voice_update("alice left", |s| s.channel_id.is_none()).await;

    // Deleting the channel ends its room.
    assert_eq!(app.delete(&format!("/channels/{voice}"), &alice).await, StatusCode::NO_CONTENT);
    livekit.expect("DeleteRoom", |b| b["room"] == voice.as_str()).await;

    // A disabled account is removed from every room (the command asks LiveKit directly).
    let output = app.admin(&["disable", "alice"]).await.unwrap();
    assert!(output.contains("Retiré du vocal."), "{output}");
}

#[tokio::test]
async fn voice_is_optional() {
    let app = spawn_app(no_rate_limit()).await;
    let alice = app.register("alice").await;
    let guild = app.guild_with(&alice, &[]).await;
    let (_, channel) = app
        .post(&format!("/guilds/{}/channels", guild.guild.id), &alice, json!({ "name": "vocal", "kind": "voice" }))
        .await;
    let (status, body) =
        app.post(&format!("/channels/{}/voice", channel["id"].as_str().unwrap()), &alice, json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("unavailable")));
    assert_eq!(app.webhook("participant_joined", "1", "1", LIVEKIT_SECRET).await, StatusCode::UNAUTHORIZED);
}
