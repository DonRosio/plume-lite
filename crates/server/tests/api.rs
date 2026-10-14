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
use plume_lite_protocol::permissions::Permissions;
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

/// `(id, position, category_id)` of every channel of a guild, in the order of `GET /channels`.
async fn channel_order(app: &TestApp, gid: impl std::fmt::Display, account: &Account) -> Vec<(String, i64, Value)> {
    let (status, channels) = app.get(&format!("/guilds/{gid}/channels"), account).await;
    assert_eq!(status, StatusCode::OK);
    channels
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap().to_owned(), c["position"].as_i64().unwrap(), c["category_id"].clone()))
        .collect()
}

#[tokio::test]
async fn categories_and_channel_order() {
    let app = spawn_app(no_rate_limit()).await;
    let app = &app;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let gid = guild.guild.id;
    let general = guild.channels[0].id.to_string();
    let (mut bob_gw, _) = app.gateway(&bob.token).await;

    // Categories: owner only, invisible to non-members, names kept as typed.
    let categories = format!("/guilds/{gid}/categories");
    assert_eq!(app.post(&categories, &bob, json!({ "name": "Pirates" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(app.post(&categories, &eve, json!({ "name": "Pirates" })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.post(&categories, &alice, json!({ "name": "  " })).await.0, StatusCode::BAD_REQUEST);
    let (status, games) = app.post(&categories, &alice, json!({ "name": "  Jeux vidéo " })).await;
    assert_eq!(
        (status, games["name"].as_str(), games["position"].as_i64()),
        (StatusCode::CREATED, Some("Jeux vidéo"), Some(0))
    );
    let games = games["id"].as_str().unwrap().to_owned();
    bob_gw.expect("CATEGORY_CREATE", |e| matches!(e, Event::CategoryCreate(c) if c.id.to_string() == games)).await;
    let (_, talk) = app.post(&categories, &alice, json!({ "name": "Discussions" })).await;
    assert_eq!(talk["position"], 1);
    let talk = talk["id"].as_str().unwrap().to_owned();

    // Channels go last of their kind in their category; the others are renumbered.
    let channels = format!("/guilds/{gid}/channels");
    let (status, minecraft) =
        app.post(&channels, &alice, json!({ "name": "Minecraft", "category_id": games, "kind": "voice" })).await;
    assert_eq!((status, minecraft["category_id"].as_str()), (StatusCode::CREATED, Some(games.as_str())));
    let minecraft = minecraft["id"].as_str().unwrap().to_owned();
    let (_, chess) = app.post(&channels, &alice, json!({ "name": "échecs", "category_id": games })).await;
    let chess = chess["id"].as_str().unwrap().to_owned();
    let (_, news) = app.post(&channels, &alice, json!({ "name": "annonces" })).await;
    let news = news["id"].as_str().unwrap().to_owned();
    let unknown = json!({ "name": "x", "category_id": "12345" });
    assert_eq!(app.post(&channels, &alice, unknown).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        channel_order(app, gid, &bob).await,
        [
            ("général".to_owned(), 0, Value::Null),
            ("annonces".to_owned(), 1, Value::Null),
            ("échecs".to_owned(), 2, json!(games)),
            ("minecraft".to_owned(), 3, json!(games)),
        ]
    );
    // Creating "annonces" pushed the game channels down: bob is told.
    bob_gw
        .expect(
            "échecs renumbered",
            |e| matches!(e, Event::ChannelUpdate(c) if c.id.to_string() == chess && c.position == 2),
        )
        .await;

    // Renaming and moving: owner only, members get a 403, others a 404.
    let patch = |account: &Account, path: String, body: Value| {
        let token = account.token.clone();
        async move { app.call(Method::PATCH, &path, Some(&token), Some(body)).await }
    };
    assert_eq!(patch(&bob, format!("/channels/{news}"), json!({ "name": "x" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(patch(&eve, format!("/channels/{news}"), json!({ "name": "x" })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(patch(&alice, format!("/channels/{news}"), json!({ "name": "!!!" })).await.0, StatusCode::BAD_REQUEST);
    let (status, renamed) =
        patch(&alice, format!("/channels/{news}"), json!({ "name": "Grandes Annonces", "position": 0 })).await;
    assert_eq!(
        (status, renamed["name"].as_str(), renamed["position"].as_i64()),
        (StatusCode::OK, Some("grandes-annonces"), Some(0))
    );
    bob_gw
        .expect(
            "général moved down",
            |e| matches!(e, Event::ChannelUpdate(c) if c.id.to_string() == general && c.position == 1),
        )
        .await;

    // Into another category (last there), then out of every category.
    let (status, moved) = patch(&alice, format!("/channels/{general}"), json!({ "category_id": talk })).await;
    assert_eq!((status, moved["category_id"].as_str()), (StatusCode::OK, Some(talk.as_str())));
    let (_, moved) = patch(&alice, format!("/channels/{chess}"), json!({ "category_id": null, "position": 0 })).await;
    assert_eq!(moved["category_id"], Value::Null);
    assert_eq!(
        channel_order(app, gid, &bob).await,
        [
            ("échecs".to_owned(), 0, Value::Null),
            ("grandes-annonces".to_owned(), 1, Value::Null),
            ("minecraft".to_owned(), 2, json!(games)),
            ("général".to_owned(), 3, json!(talk)),
        ]
    );

    // A category of another guild is as unknown as a missing one.
    let other = app.guild_with(&alice, &[]).await;
    let (_, foreign) =
        app.post(&format!("/guilds/{}/categories", other.guild.id), &alice, json!({ "name": "Ailleurs" })).await;
    let foreign = foreign["id"].as_str().unwrap();
    let body = json!({ "category_id": foreign });
    assert_eq!(patch(&alice, format!("/channels/{news}"), body.clone()).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        app.post(&channels, &alice, json!({ "name": "x", "category_id": foreign })).await.0,
        StatusCode::NOT_FOUND
    );

    // Categories move with their channels, and can be renamed.
    assert_eq!(patch(&bob, format!("/categories/{talk}"), json!({ "name": "x" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(patch(&eve, format!("/categories/{talk}"), json!({ "name": "x" })).await.0, StatusCode::NOT_FOUND);
    let (status, category) =
        patch(&alice, format!("/categories/{talk}"), json!({ "name": "Blabla", "position": 0 })).await;
    assert_eq!(
        (status, category["name"].as_str(), category["position"].as_i64()),
        (StatusCode::OK, Some("Blabla"), Some(0))
    );
    bob_gw
        .expect(
            "jeux moved down",
            |e| matches!(e, Event::CategoryUpdate(c) if c.id.to_string() == games && c.position == 1),
        )
        .await;
    let order: Vec<String> = channel_order(app, gid, &bob).await.into_iter().map(|(name, ..)| name).collect();
    assert_eq!(order, ["échecs", "grandes-annonces", "général", "minecraft"]);
    let (_, full) = app.get(&format!("/guilds/{gid}"), &bob).await;
    let names: Vec<&str> = full["categories"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Blabla", "Jeux vidéo"]);

    // Deleting a category keeps its channels, above every category.
    assert_eq!(app.delete(&format!("/categories/{games}"), &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("/categories/{games}"), &alice).await, StatusCode::NO_CONTENT);
    bob_gw
        .expect("CATEGORY_DELETE", |e| matches!(e, Event::CategoryDelete { id, .. } if id.to_string() == games))
        .await;
    bob_gw
        .expect(
            "minecraft out of its category",
            |e| matches!(e, Event::ChannelUpdate(c) if c.id.to_string() == minecraft && c.category_id.is_none()),
        )
        .await;
    assert_eq!(
        channel_order(app, gid, &bob).await,
        [
            ("échecs".to_owned(), 0, Value::Null),
            ("grandes-annonces".to_owned(), 1, Value::Null),
            ("minecraft".to_owned(), 2, Value::Null),
            ("général".to_owned(), 3, json!(talk)),
        ]
    );
    assert_eq!(app.delete(&format!("/categories/{games}"), &alice).await, StatusCode::NOT_FOUND);
    let (_, ready) = app.gateway(&bob.token).await;
    assert_eq!(ready.guilds[0].categories.len(), 1);
}

#[tokio::test]
async fn guild_rename_and_ownership_transfer() {
    let app = spawn_app(no_rate_limit()).await;
    let app = &app;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let gid = app.guild_with(&alice, &[&bob]).await.guild.id;
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let path = format!("/guilds/{gid}");
    let patch = |account: &Account, body: Value| {
        let (token, path) = (account.token.clone(), path.clone());
        async move { app.call(Method::PATCH, &path, Some(&token), Some(body)).await }
    };

    assert_eq!(patch(&bob, json!({ "name": "Piraté" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(patch(&eve, json!({ "name": "Piraté" })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(patch(&alice, json!({ "name": "x" })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(patch(&alice, json!({})).await.0, StatusCode::BAD_REQUEST);
    let (status, guild) = patch(&alice, json!({ "name": "  Rust France " })).await;
    assert_eq!((status, guild["name"].as_str()), (StatusCode::OK, Some("Rust France")));
    bob_gw.expect("GUILD_UPDATE", |e| matches!(e, Event::GuildUpdate(g) if g.name == "Rust France")).await;

    // Handing the guild over: to a member, with the owner's password.
    let owner = format!("/guilds/{gid}/owner");
    let transfer = |account: &Account, to: &str, password: &str| {
        let (token, owner) = (account.token.clone(), owner.clone());
        let body = json!({ "user_id": to, "password": password });
        async move { app.call(Method::PUT, &owner, Some(&token), Some(body)).await.0 }
    };
    assert_eq!(transfer(&bob, &bob.id, "motdepasse-solide").await, StatusCode::FORBIDDEN);
    assert_eq!(transfer(&alice, &alice.id, "motdepasse-solide").await, StatusCode::BAD_REQUEST);
    assert_eq!(transfer(&alice, &eve.id, "motdepasse-solide").await, StatusCode::NOT_FOUND, "not a member");
    assert_eq!(transfer(&alice, &bob.id, "mauvais-mot-de-passe").await, StatusCode::FORBIDDEN);
    assert_eq!(transfer(&alice, &bob.id, "motdepasse-solide").await, StatusCode::OK);
    let event =
        bob_gw.expect("new owner", |e| matches!(e, Event::GuildUpdate(g) if g.owner_id.to_string() == bob.id)).await;
    assert!(matches!(event, Event::GuildUpdate(g) if g.name == "Rust France"));

    // Bob owns it now; alice is a member like the others.
    assert_eq!(patch(&alice, json!({ "name": "Encore moi" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(patch(&bob, json!({ "name": "Chez Bob" })).await.0, StatusCode::OK);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &bob).await, StatusCode::BAD_REQUEST);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &alice).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn bans() {
    let app = spawn_app(no_rate_limit()).await;
    let app = &app;
    let (alice, bob, carol, dave) = (
        app.register("alice").await,
        app.register("bob").await,
        app.register("carol").await,
        app.register("dave").await,
    );
    let gid = app.guild_with(&alice, &[&bob, &carol, &dave]).await.guild.id;
    let (_, carols_invite) = app.post(&format!("/guilds/{gid}/invites"), &carol, json!({})).await;
    let carols_code = carols_invite["code"].as_str().unwrap().to_owned();
    let (_, invite) = app.post(&format!("/guilds/{gid}/invites"), &alice, json!({})).await;
    let code = invite["code"].as_str().unwrap().to_owned();
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let (mut carol_gw, _) = app.gateway(&carol.token).await;

    let ban = |account: &Account, user: &str, reason: Value| {
        let (token, path) = (account.token.clone(), format!("/guilds/{gid}/bans/{user}"));
        async move { app.call(Method::PUT, &path, Some(&token), Some(json!({ "reason": reason }))).await.0 }
    };
    assert_eq!(ban(&bob, &carol.id, Value::Null).await, StatusCode::FORBIDDEN);
    assert_eq!(ban(&alice, &alice.id, Value::Null).await, StatusCode::BAD_REQUEST);
    assert_eq!(ban(&alice, "12345", Value::Null).await, StatusCode::NOT_FOUND);
    assert_eq!(ban(&alice, &carol.id, json!("x".repeat(513))).await, StatusCode::BAD_REQUEST);

    // Carol is out, at once, and the invites she handed out no longer work.
    assert_eq!(ban(&alice, &carol.id, json!("  spam ")).await, StatusCode::NO_CONTENT);
    carol_gw.expect("GUILD_DELETE", |e| matches!(e, Event::GuildDelete { id } if *id == gid)).await;
    bob_gw
        .expect(
            "carol removed",
            |e| matches!(e, Event::GuildMemberRemove { user, .. } if user.id.to_string() == carol.id),
        )
        .await;
    assert_eq!(app.get(&format!("/guilds/{gid}"), &carol).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.get(&format!("/invites/{carols_code}"), &bob).await.0, StatusCode::NOT_FOUND);
    let (status, error) = app.post(&format!("/invites/{code}"), &carol, json!({})).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));

    // Leaving first does not dodge a ban.
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &dave).await, StatusCode::NO_CONTENT);
    assert_eq!(ban(&alice, &dave.id, Value::Null).await, StatusCode::NO_CONTENT);
    assert_eq!(app.post(&format!("/invites/{code}"), &dave, json!({})).await.0, StatusCode::FORBIDDEN);

    // The list: owner only, most recent first.
    assert_eq!(app.get(&format!("/guilds/{gid}/bans"), &bob).await.0, StatusCode::FORBIDDEN);
    sqlx::query("UPDATE bans SET created_at = created_at - 60 WHERE user_id = $1")
        .bind(carol.id.parse::<i64>().unwrap())
        .execute(&app.db)
        .await
        .unwrap();
    let (status, list) = app.get(&format!("/guilds/{gid}/bans"), &alice).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["user"]["username"], "dave");
    assert!(list[0].get("reason").is_none());
    assert_eq!((list[1]["user"]["username"].as_str(), list[1]["reason"].as_str()), (Some("carol"), Some("spam")));

    // Unbanned: back with an invite.
    let unban = format!("/guilds/{gid}/bans/{}", carol.id);
    assert_eq!(app.delete(&unban, &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&unban, &alice).await, StatusCode::NO_CONTENT);
    assert_eq!(app.delete(&unban, &alice).await, StatusCode::NOT_FOUND);
    assert_eq!(app.post(&format!("/invites/{code}"), &carol, json!({})).await.0, StatusCode::OK);
}

/// The roles of a member, from the member list.
async fn roles_of(app: &TestApp, gid: impl std::fmt::Display, viewer: &Account, user: &Account) -> Vec<String> {
    let (_, members) = app.get(&format!("/guilds/{gid}/members"), viewer).await;
    let member = members.as_array().unwrap().iter().find(|m| m["user"]["id"] == user.id.as_str()).unwrap();
    member["roles"].as_array().unwrap().iter().map(|r| r.to_string().trim_matches('"').to_owned()).collect()
}

#[tokio::test]
async fn roles_and_permissions() {
    let app = spawn_app(no_rate_limit()).await;
    let app = &app;
    let (alice, bob, carol, dave) = (
        app.register("alice").await,
        app.register("bob").await,
        app.register("carol").await,
        app.register("dave").await,
    );
    let guild = app.guild_with(&alice, &[&bob, &carol, &dave]).await;
    let gid = guild.guild.id;
    let general = guild.channels[0].id;
    // @everyone has the guild's id and may create invites.
    assert_eq!(guild.roles.len(), 1);
    assert_eq!((guild.roles[0].id, guild.roles[0].permissions), (gid, Permissions::CREATE_INVITE));
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let perms = |list: &[Permissions]| list.iter().fold(Permissions::NONE, |all, p| all.union(*p)).to_string();
    let roles = format!("/guilds/{gid}/roles");
    let patch = |account: &Account, path: String, body: Value| {
        let token = account.token.clone();
        async move { app.call(Method::PATCH, &path, Some(&token), Some(body)).await.0 }
    };
    let give = |account: &Account, user: &Account, role: &str, on: bool| {
        let (token, path) = (account.token.clone(), format!("/guilds/{gid}/members/{}/roles/{role}", user.id));
        async move { app.call(if on { Method::PUT } else { Method::DELETE }, &path, Some(&token), None).await.0 }
    };

    // Only those who manage roles create them.
    assert_eq!(app.post(&roles, &bob, json!({ "name": "Pirate" })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(app.post(&roles, &alice, json!({ "name": "@everyone" })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(app.post(&roles, &alice, json!({ "name": "x", "color": 0x100_0000 })).await.0, StatusCode::BAD_REQUEST);
    let moderation = perms(&[Permissions::MANAGE_ROLES, Permissions::KICK_MEMBERS, Permissions::MUTE_MEMBERS]);
    let (status, moderator) = app
        .post(&roles, &alice, json!({ "name": " Modération ", "color": 0xE9_1E63, "permissions": moderation }))
        .await;
    assert_eq!(
        (status, moderator["name"].as_str(), moderator["position"].as_i64()),
        (StatusCode::CREATED, Some("Modération"), Some(1))
    );
    assert_eq!(moderator["permissions"], moderation.as_str());
    let moderator = moderator["id"].as_str().unwrap().to_owned();
    bob_gw
        .expect("GUILD_ROLE_CREATE", |e| matches!(e, Event::GuildRoleCreate(r) if r.id.to_string() == moderator))
        .await;

    // Bob becomes a moderator: everyone sees it.
    assert_eq!(give(&bob, &bob, &moderator, true).await, StatusCode::FORBIDDEN, "not by himself");
    assert_eq!(give(&alice, &bob, &moderator, true).await, StatusCode::NO_CONTENT);
    assert_eq!(give(&alice, &bob, &moderator, true).await, StatusCode::NO_CONTENT, "idempotent");
    bob_gw
        .expect("bob's roles", |e| matches!(e, Event::GuildMemberUpdate { user_id, roles, .. } if user_id.to_string() == bob.id && roles.len() == 1))
        .await;
    assert_eq!(roles_of(app, gid, &carol, &bob).await, std::slice::from_ref(&moderator));
    let (_, ready) = app.gateway(&bob.token).await;
    assert_eq!(
        ready.guilds[0].my_roles.iter().map(ToString::to_string).collect::<Vec<_>>(),
        std::slice::from_ref(&moderator)
    );

    // He hands out only permissions he has, and only roles ranked below his own.
    let ban = perms(&[Permissions::BAN_MEMBERS]);
    assert_eq!(
        app.post(&roles, &bob, json!({ "name": "Bannisseur", "permissions": ban })).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, regular) =
        app.post(&roles, &bob, json!({ "name": "Habitué", "permissions": perms(&[Permissions::KICK_MEMBERS]) })).await;
    assert_eq!((status, regular["position"].as_i64()), (StatusCode::CREATED, Some(1)), "lowest");
    let regular = regular["id"].as_str().unwrap().to_owned();
    bob_gw
        .expect(
            "moderation pushed up",
            |e| matches!(e, Event::GuildRoleUpdate(r) if r.id.to_string() == moderator && r.position == 2),
        )
        .await;
    assert_eq!(give(&bob, &carol, &regular, true).await, StatusCode::NO_CONTENT);
    assert_eq!(give(&bob, &bob, &regular, true).await, StatusCode::NO_CONTENT, "a lower role to himself");
    assert_eq!(give(&bob, &carol, &moderator, true).await, StatusCode::FORBIDDEN, "his own rank");
    assert_eq!(patch(&bob, format!("{roles}/{moderator}"), json!({ "name": "Chef" })).await, StatusCode::FORBIDDEN);
    assert_eq!(
        patch(&bob, format!("{roles}/{regular}"), json!({ "position": 2 })).await,
        StatusCode::FORBIDDEN,
        "up to his rank"
    );
    assert_eq!(patch(&bob, format!("{roles}/{regular}"), json!({ "permissions": ban })).await, StatusCode::FORBIDDEN);
    assert_eq!(
        patch(&bob, format!("{roles}/{regular}"), json!({ "name": "Fidèle", "color": null })).await,
        StatusCode::OK
    );
    assert_eq!(app.delete(&format!("{roles}/{moderator}"), &bob).await, StatusCode::FORBIDDEN);

    // Kicking: only those ranked lower, never the owner.
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/{}", alice.id), &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(give(&alice, &carol, &moderator, true).await, StatusCode::NO_CONTENT);
    assert_eq!(
        app.delete(&format!("/guilds/{gid}/members/{}", carol.id), &bob).await,
        StatusCode::FORBIDDEN,
        "same rank"
    );
    assert_eq!(give(&bob, &carol, &regular, false).await, StatusCode::FORBIDDEN, "carol is not below him anymore");
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/{}", dave.id), &carol).await, StatusCode::NO_CONTENT);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/{}", bob.id), &carol).await, StatusCode::FORBIDDEN);
    // Nor ban without the permission.
    let ban_bob = format!("/guilds/{gid}/bans/{}", bob.id);
    assert_eq!(app.call(Method::PUT, &ban_bob, Some(&carol.token), Some(json!({}))).await.0, StatusCode::FORBIDDEN);

    // @everyone: permissions only; nobody gives it what they lack.
    let everyone = format!("{roles}/{gid}");
    assert_eq!(patch(&alice, everyone.clone(), json!({ "name": "tous" })).await, StatusCode::BAD_REQUEST);
    assert_eq!(patch(&bob, everyone.clone(), json!({ "permissions": ban })).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&everyone, &alice).await, StatusCode::BAD_REQUEST);
    assert_eq!(give(&alice, &bob, &gid.to_string(), true).await, StatusCode::BAD_REQUEST);
    let mention = perms(&[Permissions::MENTION_EVERYONE]);
    assert_eq!(patch(&alice, everyone.clone(), json!({ "permissions": mention })).await, StatusCode::OK);

    // So everyone may now notify everyone, and nobody may invite any more.
    let messages = format!("/channels/{general}/messages");
    let (_, sent) = app.post(&messages, &bob, json!({ "content": "@everyone réunion !" })).await;
    assert_eq!(sent["mention_everyone"], true);
    assert_eq!(app.post(&format!("/guilds/{gid}/invites"), &bob, json!({})).await.0, StatusCode::FORBIDDEN);

    // Managing messages: deleting those of others, pinning.
    let (_, from_alice) = app.post(&messages, &alice, json!({ "content": "à épingler" })).await;
    let from_alice = from_alice["id"].as_str().unwrap().to_owned();
    let pin = format!("/channels/{general}/pins/{from_alice}");
    assert_eq!(app.call(Method::PUT, &pin, Some(&bob.token), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("{messages}/{from_alice}"), &bob).await, StatusCode::FORBIDDEN);
    let manage = perms(&[Permissions::MANAGE_ROLES, Permissions::KICK_MEMBERS, Permissions::MANAGE_MESSAGES]);
    assert_eq!(patch(&alice, format!("{roles}/{moderator}"), json!({ "permissions": manage })).await, StatusCode::OK);
    assert_eq!(app.call(Method::PUT, &pin, Some(&bob.token), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(app.delete(&format!("{messages}/{from_alice}"), &bob).await, StatusCode::NO_CONTENT);

    // Deleting a role takes it from its members; leaving the guild drops one's roles.
    assert_eq!(app.delete(&format!("{roles}/{moderator}"), &alice).await, StatusCode::NO_CONTENT);
    bob_gw
        .expect("GUILD_ROLE_DELETE", |e| matches!(e, Event::GuildRoleDelete { id, .. } if id.to_string() == moderator))
        .await;
    assert_eq!(roles_of(app, gid, &alice, &bob).await, std::slice::from_ref(&regular));
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/{}", carol.id), &bob).await, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("/guilds/{gid}/members/@me"), &bob).await, StatusCode::NO_CONTENT);
    let roles_left: i64 = sqlx::query_scalar("SELECT count(*) FROM member_roles WHERE user_id = $1")
        .bind(bob.id.parse::<i64>().unwrap())
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(roles_left, 0);
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
async fn unread_messages_are_remembered() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, carol, eve) = (
        app.register("alice").await,
        app.register("bob").await,
        app.register("carol").await,
        app.register("eve").await,
    );
    let guild = app.guild_with(&alice, &[&bob]).await;
    let channel = guild.channels[0].id;
    let path = format!("/channels/{channel}/messages");
    let post = |content: String, by: &Account, reply_to: Option<&str>| {
        let (path, token) = (path.clone(), by.token.clone());
        let body = json!({ "content": content, "reply_to": reply_to });
        let app = &app;
        async move {
            let (status, message) = app.call(Method::POST, &path, Some(&token), Some(body)).await;
            assert_eq!(status, StatusCode::CREATED);
            message["id"].as_str().unwrap().to_owned()
        }
    };
    let read_state = |account: &Account| {
        let token = account.token.clone();
        let app = &app;
        async move {
            let (_, ready) = app.gateway(&token).await;
            let last_message = ready.guilds[0].channels.iter().find(|c| c.id == channel).unwrap().last_message_id;
            let read = ready.read_states.iter().find(|r| r.channel_id == channel).copied();
            (last_message.map(|id| id.to_string()), read.map(|r| (r.last_read_id.to_string(), r.mentions)))
        }
    };

    // Messages, three of which are for bob: his name, @everyone from the owner, nothing else.
    post("bonjour".into(), &alice, None).await;
    post(format!("<@{}> regarde", bob.id), &alice, None).await;
    post("@everyone réunion".into(), &alice, None).await;
    let last = post(format!("<@{}> et <@{}>", alice.id, alice.id), &alice, None).await;
    let (last_message, read) = read_state(&bob).await;
    assert_eq!(last_message.as_deref(), Some(last.as_str()));
    assert_eq!(read, Some(("0".to_owned(), 2)), "never read: two mentions of bob");

    // Reading on one device: every device is told, and it never goes back.
    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    let ack = |message: &str, by: &Account| {
        let (path, token, body) =
            (format!("/channels/{channel}/ack"), by.token.clone(), json!({ "message_id": message }));
        let app = &app;
        async move { app.call(Method::POST, &path, Some(&token), Some(body)).await.0 }
    };
    assert_eq!(ack(&last, &bob).await, StatusCode::NO_CONTENT);
    let Event::ReadStateUpdate(update) =
        bob_gw.expect("bob's read state", |e| matches!(e, Event::ReadStateUpdate(_))).await
    else {
        unreachable!()
    };
    assert_eq!((update.last_read_id.to_string(), update.mentions), (last.clone(), 0));
    assert_eq!(ack(&guild.channels[0].id.to_string(), &bob).await, StatusCode::NOT_FOUND, "not a message");
    assert_eq!(ack(&last, &eve).await, StatusCode::NOT_FOUND, "members only");
    post("encore un".into(), &alice, None).await;
    assert_eq!(ack(&last, &bob).await, StatusCode::NO_CONTENT);
    assert_eq!(read_state(&bob).await.1, Some((last.clone(), 0)), "an older message does not unread anything");

    // Writing reads the channel; a reply to one of our messages counts as a mention.
    let mine = post("je réponds".into(), &bob, None).await;
    assert_eq!(read_state(&bob).await.1, Some((mine.clone(), 0)));
    post("d'accord".into(), &alice, Some(&mine)).await;
    assert_eq!(read_state(&bob).await.1, Some((mine, 1)));

    // A new member starts with the history read.
    let (_, invite) = app.post(&format!("/guilds/{}/invites", guild.guild.id), &alice, json!({})).await;
    app.post(&format!("/invites/{}", invite["code"].as_str().unwrap()), &carol, json!({})).await;
    let (last_message, read) = read_state(&carol).await;
    assert_eq!(read, Some((last_message.unwrap(), 0)));
}

#[tokio::test]
async fn notification_settings() {
    use plume_lite_protocol::model::{NotificationLevel, NotificationSetting};
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let (gid, channel) = (guild.guild.id, guild.channels[0].id);
    let set = |target: String, level: Option<&str>, by: &Account| {
        let (path, token) = (format!("/users/@me/notifications/{target}"), by.token.clone());
        let (app, level) = (&app, level.map(str::to_owned));
        async move {
            match level {
                Some(level) => app.call(Method::PUT, &path, Some(&token), Some(json!({ "level": level }))).await.0,
                None => app.call(Method::DELETE, &path, Some(&token), None).await.0,
            }
        }
    };
    let settings = |account: &Account| {
        let token = account.token.clone();
        let app = &app;
        async move {
            let mut settings = app.gateway(&token).await.1.notification_settings;
            settings.sort_by_key(|s| s.target_id);
            settings
        }
    };

    let (mut bob_gw, _) = app.gateway(&bob.token).await;
    assert_eq!(set(gid.to_string(), Some("mentions"), &bob).await, StatusCode::NO_CONTENT);
    bob_gw
        .expect("the setting, on every device", |e| {
            matches!(e, Event::NotificationSettingUpdate { target_id, level: Some(NotificationLevel::Mentions) } if *target_id == gid)
        })
        .await;
    assert_eq!(set(channel.to_string(), Some("all"), &bob).await, StatusCode::NO_CONTENT);
    assert_eq!(set(channel.to_string(), Some("nothing"), &bob).await, StatusCode::NO_CONTENT, "replaced");
    let mut expected = vec![
        NotificationSetting { target_id: gid, level: NotificationLevel::Mentions },
        NotificationSetting { target_id: channel, level: NotificationLevel::Nothing },
    ];
    expected.sort_by_key(|s| s.target_id);
    assert_eq!(settings(&bob).await, expected);
    assert!(settings(&alice).await.is_empty(), "per user");

    assert_eq!(set(channel.to_string(), None, &bob).await, StatusCode::NO_CONTENT);
    assert_eq!(settings(&bob).await, [NotificationSetting { target_id: gid, level: NotificationLevel::Mentions }]);
    assert_eq!(set(gid.to_string(), Some("all"), &eve).await, StatusCode::NOT_FOUND, "not a member");
    assert_eq!(set("1".into(), Some("all"), &bob).await, StatusCode::NOT_FOUND);
    assert_eq!(set(gid.to_string(), Some("loud"), &bob).await, StatusCode::BAD_REQUEST);

    // Settings go with their channel.
    let (_, other) = app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "autre" })).await;
    let other = other["id"].as_str().unwrap().to_owned();
    assert_eq!(set(other.clone(), Some("nothing"), &bob).await, StatusCode::NO_CONTENT);
    assert_eq!(app.delete(&format!("/channels/{other}"), &alice).await, StatusCode::NO_CONTENT);
    assert_eq!(settings(&bob).await.len(), 1);
}

#[tokio::test]
async fn search() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let gid = guild.guild.id;
    let general = format!("/channels/{}/messages", guild.channels[0].id);
    let (_, games) = app.post(&format!("/guilds/{gid}/channels"), &alice, json!({ "name": "jeux" })).await;
    let games_id = games["id"].as_str().unwrap().to_owned();
    let games = format!("/channels/{games_id}/messages");
    let post = |path: &str, content: &str, by: &Account| {
        let (path, token, body) = (path.to_owned(), by.token.clone(), json!({ "content": content }));
        let app = &app;
        async move { app.call(Method::POST, &path, Some(&token), Some(body)).await.1["id"].as_str().unwrap().to_owned() }
    };
    let search = |query: String, by: &Account| {
        let (path, token) = (format!("/guilds/{gid}/messages/search?{query}"), by.token.clone());
        let app = &app;
        async move {
            let (status, body) = app.call(Method::GET, &path, Some(&token), None).await;
            let contents: Vec<String> = body["messages"]
                .as_array()
                .map(|list| list.iter().map(|m| m["content"].as_str().unwrap().to_owned()).collect())
                .unwrap_or_default();
            (status, body["total"].as_u64().unwrap_or(0), contents)
        }
    };

    post(&general, "L'**été** arrive", &alice).await;
    let edited = post(&general, "Les vacances d'été", &alice).await;
    post(&games, "Une partie cet été ?", &bob).await;
    post(&general, "rien à voir", &bob).await;
    let elsewhere = app.guild_with(&eve, &[]).await;
    post(&format!("/channels/{}/messages", elsewhere.channels[0].id), "été chez eve", &eve).await;

    // Case and accents do not matter, newest first, every word must match.
    let (status, total, found) = search("q=ETE".into(), &bob).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (total, found),
        (3, vec!["Une partie cet été ?".to_owned(), "Les vacances d'été".into(), "L'**été** arrive".into()])
    );
    assert_eq!(search("q=vac".into(), &bob).await.2, ["Les vacances d'été"], "words starting with it");
    assert_eq!(search("q=%C3%A9t%C3%A9%20vacances".into(), &bob).await.2, ["Les vacances d'été"]);
    assert_eq!(search(format!("q=ete&channel_id={games_id}"), &bob).await.2, ["Une partie cet été ?"]);
    assert_eq!(search(format!("q=ete&author_id={}", bob.id), &alice).await.2, ["Une partie cet été ?"]);

    // The index follows edits and deletions, the deletion of a channel included.
    let edit = json!({ "content": "Les vacances au ski" });
    app.call(Method::PATCH, &format!("{general}/{edited}"), Some(&alice.token), Some(edit)).await;
    assert_eq!(search("q=ski".into(), &bob).await.2, ["Les vacances au ski"]);
    assert_eq!(search("q=ete".into(), &bob).await.1, 2);
    assert_eq!(app.delete(&format!("{general}/{edited}"), &alice).await, StatusCode::NO_CONTENT);
    assert!(search("q=ski".into(), &bob).await.2.is_empty());
    assert_eq!(app.delete(&format!("/channels/{games_id}"), &alice).await, StatusCode::NO_CONTENT);
    assert_eq!(search("q=ete".into(), &bob).await.2, ["L'**été** arrive"]);

    // Pages of 25.
    for i in 0..30 {
        post(&general, &format!("page {i}"), &alice).await;
    }
    let (_, body) = app.get(&format!("/guilds/{gid}/messages/search?q=page"), &bob).await;
    let page: plume_lite_protocol::http::SearchResults = serde_json::from_value(body).unwrap();
    assert_eq!((page.total, page.messages.len(), page.messages[0].content.as_str()), (30, 25, "page 29"));
    let next = search(format!("q=page&before={}", page.messages[24].id), &bob).await;
    assert_eq!((next.1, next.2.len(), next.2[4].as_str()), (30, 5, "page 0"));

    // Members only, and something to look for.
    assert_eq!(search("q=ete".into(), &eve).await.0, StatusCode::NOT_FOUND);
    assert_eq!(search("q=%21%21".into(), &bob).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(search(format!("q={}", "a".repeat(201)), &bob).await.0, StatusCode::BAD_REQUEST);
}

/// Percent-encodes every byte: emojis in a path segment.
fn encoded(emoji: &str) -> String {
    emoji.bytes().map(|b| format!("%{b:02X}")).collect()
}

#[tokio::test]
async fn reactions_and_pins() {
    let app = spawn_app(no_rate_limit()).await;
    let (alice, bob, eve) = (app.register("alice").await, app.register("bob").await, app.register("eve").await);
    let guild = app.guild_with(&alice, &[&bob]).await;
    let path = format!("/channels/{}/messages", guild.channels[0].id);
    let (_, message) = app.post(&path, &bob, json!({ "content": "salut" })).await;
    let id = message["id"].as_str().unwrap().to_owned();
    let reaction = |emoji: &str| format!("{path}/{id}/reactions/{}/@me", encoded(emoji));
    let react = |emoji: &str, by: &Account, method: Method| {
        let (path, token) = (reaction(emoji), by.token.clone());
        let app = &app;
        async move { app.call(method, &path, Some(&token), None).await.0 }
    };
    let history = || async {
        let (_, page) = app.get(&path, &alice).await;
        serde_json::from_value::<Vec<Message>>(page).unwrap().into_iter().find(|m| m.id.to_string() == id).unwrap()
    };

    // Reactions: a set per emoji, in the order the emojis first appeared, pushed to everyone.
    let (mut alice_gw, _) = app.gateway(&alice.token).await;
    assert_eq!(react("👍", &bob, Method::PUT).await, StatusCode::NO_CONTENT);
    let added = alice_gw.expect("bob's reaction", |e| matches!(e, Event::ReactionAdd { .. })).await;
    assert!(
        matches!(added, Event::ReactionAdd { ref emoji, user_id, .. } if emoji == "👍" && user_id.to_string() == bob.id)
    );
    assert_eq!(react("👍", &bob, Method::PUT).await, StatusCode::NO_CONTENT, "idempotent");
    assert_eq!(react("❤️", &alice, Method::PUT).await, StatusCode::NO_CONTENT);
    assert_eq!(react("👍", &alice, Method::PUT).await, StatusCode::NO_CONTENT);
    let reactions = history().await.reactions;
    assert_eq!(reactions.iter().map(|r| r.emoji.as_str()).collect::<Vec<_>>(), ["👍", "❤️"]);
    assert_eq!(
        reactions[0].users.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [bob.id.clone(), alice.id.clone()]
    );
    assert_eq!(react("abc", &bob, Method::PUT).await, StatusCode::BAD_REQUEST, "emojis only");
    assert_eq!(react("👍", &eve, Method::PUT).await, StatusCode::NOT_FOUND, "members only");
    let (status, _) =
        app.call(Method::PUT, &format!("{path}/1/reactions/{}/@me", encoded("👍")), Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // At most 20 different emojis on a message.
    for i in 0..18 {
        let emoji = char::from_u32(0x1F600 + i).unwrap().to_string();
        assert_eq!(react(&emoji, &bob, Method::PUT).await, StatusCode::NO_CONTENT);
    }
    assert_eq!(react("🚀", &bob, Method::PUT).await, StatusCode::BAD_REQUEST, "a 21st emoji");
    assert_eq!(react("👍", &eve, Method::DELETE).await, StatusCode::NOT_FOUND);
    assert_eq!(react("❤️", &alice, Method::DELETE).await, StatusCode::NO_CONTENT);
    alice_gw
        .expect("alice's reaction removed", |e| matches!(e, Event::ReactionRemove { emoji, .. } if emoji == "❤️"))
        .await;
    assert_eq!(react("❤️", &alice, Method::DELETE).await, StatusCode::NO_CONTENT, "idempotent");
    assert_eq!(history().await.reactions.len(), 19);

    // Pins: the guild owner only, shown as a MESSAGE_UPDATE, listed most recent first.
    let pin_path = format!("/channels/{}/pins", guild.channels[0].id);
    let pin = |message: &str, by: &Account, method: Method| {
        let (path, token) = (format!("{pin_path}/{message}"), by.token.clone());
        let app = &app;
        async move { app.call(method, &path, Some(&token), None).await.0 }
    };
    assert_eq!(pin(&id, &bob, Method::PUT).await, StatusCode::FORBIDDEN);
    assert_eq!(pin(&id, &eve, Method::PUT).await, StatusCode::NOT_FOUND);
    assert_eq!(pin(&id, &alice, Method::PUT).await, StatusCode::NO_CONTENT);
    let Event::MessageUpdate(updated) =
        alice_gw.expect("the pin", |e| matches!(e, Event::MessageUpdate(m) if m.pinned)).await
    else {
        unreachable!()
    };
    assert_eq!(updated.reactions.len(), 19, "the whole message");
    assert_eq!(pin(&id, &alice, Method::PUT).await, StatusCode::NO_CONTENT, "idempotent");
    assert_eq!(pin("1", &alice, Method::PUT).await, StatusCode::NOT_FOUND);
    let (_, pins) = app.get(&pin_path, &bob).await;
    assert_eq!((pins.as_array().unwrap().len(), pins[0]["id"].as_str()), (1, Some(id.as_str())));
    assert!(history().await.pinned);
    assert_eq!(pin(&id, &alice, Method::DELETE).await, StatusCode::NO_CONTENT);
    alice_gw.expect("the unpin", |e| matches!(e, Event::MessageUpdate(m) if !m.pinned)).await;
    assert!(app.get(&pin_path, &bob).await.1.as_array().unwrap().is_empty());

    // At most 50 pinned messages in a channel.
    for i in 0..50 {
        let (_, message) = app.post(&path, &alice, json!({ "content": format!("épinglé {i}") })).await;
        assert_eq!(pin(message["id"].as_str().unwrap(), &alice, Method::PUT).await, StatusCode::NO_CONTENT);
    }
    assert_eq!(pin(&id, &alice, Method::PUT).await, StatusCode::BAD_REQUEST);

    // A deleted message takes its reactions with it.
    assert_eq!(app.delete(&format!("{path}/{id}"), &bob).await, StatusCode::NO_CONTENT);
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM reactions").fetch_one(&app.db).await.unwrap();
    assert_eq!(left, 0);
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
    let windows = &info["client"]["downloads"][0];
    assert_eq!((windows["platform"].as_str(), windows["size"].as_u64()), (Some("windows"), Some(14)));
    // sha256sum of "MZ fake client"
    assert_eq!(windows["sha256"], "4b43d5894f9f76c0a78dfe5b29fabcb9fbf5f90022b6613c2beac0b807b103c3");
    assert!(windows.get("signature_path").is_none(), "not signed");

    // A signed Linux build: both are listed, each with its own digest.
    std::fs::write(downloads.join("plume-lite-linux-x64.tar.gz"), b"\x1f\x8b fake archive").unwrap();
    std::fs::write(downloads.join("plume-lite-linux-x64.tar.gz.minisig"), b"untrusted comment: test").unwrap();
    let (_, info) = app.call(Method::GET, "/instance", None, None).await;
    let release: plume_lite_protocol::http::ClientRelease = serde_json::from_value(info["client"].clone()).unwrap();
    assert_eq!(release.download_path, "/download/plume-lite.exe", "what older clients offer");
    let linux = release.downloads.iter().find(|d| d.platform == plume_lite_protocol::http::Platform::Linux).unwrap();
    assert_eq!(linux.path, "/download/plume-lite-linux-x64.tar.gz");
    assert_eq!(linux.signature_path.as_deref(), Some("/download/plume-lite-linux-x64.tar.gz.minisig"));
    assert_ne!(linux.sha256, release.downloads[0].sha256);
    let signature = app.http.get(app.url("/download/plume-lite-linux-x64.tar.gz.minisig")).send().await.unwrap();
    assert_eq!(signature.status(), StatusCode::OK);
    let file = app.http.get(app.url("/download/plume-lite.exe")).send().await.unwrap();
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.bytes().await.unwrap().as_ref(), b"MZ fake client");
    let escape = app.http.get(app.url("/download/..%2fplume-lite.db")).send().await.unwrap();
    assert_eq!(escape.status(), StatusCode::NOT_FOUND);

    let home = app.http.get(app.url("/")).send().await.unwrap();
    assert!(home.headers()["content-security-policy"].to_str().unwrap().starts_with("default-src 'none'"));
    let page = home.text().await.unwrap();
    assert!(page.contains(r#"href="/download/plume-lite.exe""#));
    assert!(page.contains(r#"href="/download/plume-lite-linux-x64.tar.gz""#));
    assert!(page.contains(&linux.sha256));
    assert!(page.contains(plume_lite_server::downloads::public_key_base64()));
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
