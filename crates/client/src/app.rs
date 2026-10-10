//! Application state machine (Elm architecture): `Msg` in, new state + `Task`s out.
//! Rendering lives in `view/`, data folding in `store.rs`, I/O in `api.rs` / `gateway.rs`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::widget::{operation, scrollable};
use iced::{Subscription, Task, Theme, event, keyboard, window};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{AuthResponse, ClientRelease, Credentials, InstanceInfo, Registration, VoiceJoin};
use plume_lite_protocol::model::{Channel, ChannelKind, FullGuild, Invite, Member, Message, SessionInfo, User};
use plume_lite_protocol::validate;

use crate::api::{self, Api, ApiError};
use crate::gateway::{self, GatewayEvent};
use crate::session::{self, Preferences, SavedSession};
use crate::store::Store;
use crate::voice::{self, AudioSettings, Devices, VoiceEvent};
use crate::{theme, view};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

pub const LOGIN_USERNAME: &str = "login-username";
pub const COMPOSER: &str = "composer";
pub const MESSAGES: &str = "messages";
pub const EDITOR: &str = "editor";
pub const MODAL_INPUT: &str = "modal-input";

const TYPING_THROTTLE: Duration = Duration::from_secs(8);
const TOAST_DURATION: Duration = Duration::from_secs(5);
/// Messages kept for channels that are not on screen (memory stays flat).
const KEEP_HIDDEN_MESSAGES: usize = 100;
/// Distance from the top of the history (px) that triggers loading older messages.
const LOAD_OLDER_THRESHOLD: f32 = 150.0;

pub struct App {
    pub screen: Screen,
}

pub enum Screen {
    /// Validating a saved token at startup.
    Restoring,
    Login(LoginForm),
    Chat(Box<Chat>),
}

#[derive(Debug, Default)]
pub struct LoginForm {
    pub server: String,
    pub username: String,
    pub password: String,
    /// Registration code handed out by the server's administrator.
    pub invite_code: String,
    pub register: bool,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct Chat {
    pub api: Api,
    pub target: gateway::Target,
    pub me: User,
    pub store: Store,
    pub connection: Connection,
    pub guild: Option<Snowflake>,
    pub channel: Option<Snowflake>,
    pub composer: String,
    pub editing: Option<Editing>,
    pub hovered: Option<Snowflake>,
    pub guild_menu: bool,
    pub modal: Option<Modal>,
    pub settings: Option<Settings>,
    /// A newer client offered by the server.
    pub update: Option<ClientRelease>,
    /// The voice channel we are in (or joining).
    pub voice: Option<VoiceSession>,
    /// Microphone and sound switches, kept between calls.
    pub voice_mute: bool,
    pub voice_deaf: bool,
    pub audio: AudioSettings,
    pub toast: Option<String>,
    last_channel: HashMap<Snowflake, Snowflake>,
    drafts: HashMap<Snowflake, String>,
    toast_id: u64,
    last_typing: Option<Instant>,
    nonce_prefix: u32,
    nonce_seq: u64,
}

#[derive(Debug, Clone)]
pub enum Connection {
    Connecting,
    Online,
    Reconnecting { reason: String, retry_in: Duration },
}

pub struct VoiceSession {
    pub guild_id: Snowflake,
    pub channel_id: Snowflake,
    /// `None` while asking the server for a token.
    pub target: Option<voice::Target>,
    /// `None` until connected to the room.
    pub commands: Option<voice::Commands>,
    pub speaking: HashSet<Snowflake>,
    /// Microphone level (dBFS), for the meter in the settings.
    pub level: f32,
}

/// The settings window: password and logged-in devices.
#[derive(Default)]
pub struct Settings {
    pub current: String,
    pub new: String,
    pub confirm: String,
    pub busy: bool,
    pub error: Option<String>,
    pub saved: bool,
    /// `None` while loading.
    pub sessions: Option<Result<Vec<SessionInfo>, String>>,
    pub devices: Option<Result<Devices, String>>,
}

pub struct Editing {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub text: String,
}

pub struct Modal {
    pub kind: ModalKind,
    pub input: String,
    pub busy: bool,
    pub error: Option<String>,
}

pub enum ModalKind {
    CreateGuild,
    JoinGuild,
    CreateChannel(Snowflake, ChannelKind),
    Invite {
        invite: Option<Invite>,
    },
    LeaveGuild(Snowflake),
    DeleteGuild(Snowflake),
    DeleteChannel(Snowflake),
    /// The guild owner acts on someone in voice.
    VoiceMember {
        guild_id: Snowflake,
        user_id: Snowflake,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum ModalRequest {
    CreateGuild,
    JoinGuild,
    CreateChannel(Snowflake, ChannelKind),
    Invite(Snowflake),
    LeaveGuild(Snowflake),
    DeleteGuild(Snowflake),
    DeleteChannel(Snowflake),
    VoiceMember { guild_id: Snowflake, user_id: Snowflake },
}

#[derive(Debug, Clone)]
pub enum ModalOutcome {
    Guild(FullGuild),
    Channel(Channel),
    GuildRemoved(Snowflake),
    ChannelRemoved(Snowflake),
}

#[derive(Debug, Clone)]
pub enum Msg {
    // Session
    Restored(Result<(SavedSession, User), ApiError>),
    LoginServer(String),
    LoginUsername(String),
    LoginPassword(String),
    LoginInviteCode(String),
    ToggleRegister,
    SubmitLogin,
    LoggedIn(Result<(String, AuthResponse), ApiError>),
    Logout,
    // Realtime
    Gateway(GatewayEvent),
    // Navigation
    SelectGuild(Snowflake),
    SelectChannel(Snowflake),
    ToggleGuildMenu,
    // Data
    History { channel_id: Snowflake, older: bool, result: Result<Vec<Message>, ApiError> },
    LoadOlder,
    Scrolled(scrollable::Viewport),
    Members { guild_id: Snowflake, result: Result<Vec<Member>, ApiError> },
    // Composer
    ComposerChanged(String),
    Send,
    Sent { channel_id: Snowflake, nonce: String, result: Result<Message, ApiError> },
    Retry(String),
    Discard(String),
    // Message actions
    Hover(Snowflake),
    Unhover(Snowflake),
    Copy(String),
    StartEdit(Snowflake),
    EditChanged(String),
    SubmitEdit,
    Edited(Result<Message, ApiError>),
    Delete(Snowflake),
    Deleted { channel_id: Snowflake, id: Snowflake, result: Result<(), ApiError> },
    // Modals
    OpenModal(ModalRequest),
    CloseModal,
    ModalInput(String),
    SubmitModal,
    ModalDone(Result<ModalOutcome, ApiError>),
    InviteReady(Result<Invite, ApiError>),
    // Settings
    OpenSettings,
    SettingsCurrent(String),
    SettingsNew(String),
    SettingsConfirm(String),
    SubmitPassword,
    PasswordChanged(Result<(), ApiError>),
    SessionsLoaded(Result<Vec<SessionInfo>, ApiError>),
    RevokeSession(Snowflake),
    SessionRevoked { id: Snowflake, result: Result<(), ApiError> },
    InstanceLoaded(Result<InstanceInfo, ApiError>),
    OpenUrl(String),
    DevicesLoaded(Result<Devices, String>),
    SetInputDevice(voice::Device),
    SetOutputDevice(voice::Device),
    SetSensitivity(f32),
    SetNoiseSuppression(bool),
    SetEchoCancellation(bool),
    // Voice
    JoinVoice(Snowflake),
    VoiceTarget { channel_id: Snowflake, result: Result<VoiceJoin, ApiError> },
    Voice(VoiceEvent),
    LeaveVoice,
    ToggleMute,
    ToggleDeaf,
    ServerMute { guild_id: Snowflake, user_id: Snowflake, muted: bool },
    DisconnectFromVoice { guild_id: Snowflake, user_id: Snowflake },
    Moderated(Result<(), ApiError>),
    // Misc
    ToggleTheme,
    Tick,
    Escape,
    FocusNext,
    DismissToast(u64),
    Ignore,
}

impl App {
    pub fn boot() -> (Self, Task<Msg>) {
        theme::set_dark(session::load_preferences().dark);
        match session::load() {
            Some(SavedSession { server_url, token: Some(token) }) => {
                let api = Api::new(&server_url).authenticated(&token);
                let saved = SavedSession { server_url, token: Some(token) };
                let task = Task::perform(async move { api.me().await.map(|user| (saved, user)) }, Msg::Restored);
                (Self { screen: Screen::Restoring }, task)
            }
            saved => {
                let server = saved.map_or_else(|| DEFAULT_SERVER.to_owned(), |s| s.server_url);
                (
                    Self { screen: Screen::Login(LoginForm { server, ..LoginForm::default() }) },
                    operation::focus(LOGIN_USERNAME),
                )
            }
        }
    }

    pub fn title(&self) -> String {
        if let Screen::Chat(chat) = &self.screen
            && let Some(channel) = chat.channel.and_then(|id| chat.store.channel(id))
            && let Some(guild) = chat.store.guild(channel.guild_id)
        {
            return format!("#{} — {} · Plume Lite", channel.name, guild.name);
        }
        "Plume Lite".to_owned()
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    pub fn view(&self) -> iced::Element<'_, Msg> {
        match &self.screen {
            Screen::Restoring => view::splash(),
            Screen::Login(form) => view::login::view(form),
            Screen::Chat(chat) => view::chat_screen(chat),
        }
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let keys = event::listen_with(on_key);
        let Screen::Chat(chat) = &self.screen else { return keys };
        let mut subscriptions = vec![keys, gateway::subscription(chat.target.clone()).map(Msg::Gateway)];
        if let Some(target) = chat.voice.as_ref().and_then(|v| v.target.clone()) {
            subscriptions.push(voice::subscription(target).map(Msg::Voice));
        }
        if chat.store.has_typists() {
            subscriptions.push(iced::time::every(Duration::from_secs(1)).map(|_| Msg::Tick));
        }
        Subscription::batch(subscriptions)
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Restored(Ok((saved, user))) => {
                let token = saved.token.unwrap_or_default();
                self.enter_chat(&saved.server_url, &token, user)
            }
            Msg::Restored(Err(err)) => {
                let server = session::load().map_or_else(|| DEFAULT_SERVER.to_owned(), |s| s.server_url);
                let error = if err.is_unauthorized() {
                    session::forget_token();
                    "Votre session a expiré, reconnectez-vous.".to_owned()
                } else {
                    err.message
                };
                self.screen = Screen::Login(LoginForm { server, error: Some(error), ..LoginForm::default() });
                operation::focus(LOGIN_USERNAME)
            }
            Msg::Logout => {
                let Screen::Chat(chat) = &self.screen else { return Task::none() };
                let api = chat.api.clone();
                let server = chat.api.origin().to_owned();
                session::forget_token();
                self.screen = Screen::Login(LoginForm { server, ..LoginForm::default() });
                Task::batch([
                    Task::perform(async move { api.logout().await }, |_| Msg::Ignore),
                    operation::focus(LOGIN_USERNAME),
                ])
            }
            Msg::ToggleTheme => {
                let dark = !theme::is_dark();
                theme::set_dark(dark);
                let preferences = Preferences { dark, ..session::load_preferences() };
                if let Err(err) = session::save_preferences(&preferences) {
                    eprintln!("could not persist the preferences: {err}");
                }
                Task::none()
            }
            Msg::FocusNext if matches!(self.screen, Screen::Login(_)) => operation::focus_next(),
            Msg::LoggedIn(Ok((server, auth))) => {
                let saved = SavedSession { server_url: server.clone(), token: Some(auth.token.clone()) };
                if let Err(err) = session::save(&saved) {
                    eprintln!("could not persist the session: {err}");
                }
                self.enter_chat(&server, &auth.token, auth.user)
            }
            msg => match &mut self.screen {
                Screen::Restoring => Task::none(),
                Screen::Login(form) => update_login(form, msg),
                Screen::Chat(chat) => {
                    if let Msg::Gateway(GatewayEvent::AuthFailed) = msg {
                        session::forget_token();
                        let server = chat.api.origin().to_owned();
                        let error = Some("Votre session a expiré, reconnectez-vous.".to_owned());
                        self.screen = Screen::Login(LoginForm { server, error, ..LoginForm::default() });
                        return Task::none();
                    }
                    chat.update(msg)
                }
            },
        }
    }

    fn enter_chat(&mut self, server: &str, token: &str, me: User) -> Task<Msg> {
        let api = Api::new(server).authenticated(token);
        let target = gateway::Target { url: api.gateway_url(), token: token.to_owned() };
        self.screen = Screen::Chat(Box::new(Chat {
            api,
            target,
            me,
            store: Store::default(),
            connection: Connection::Connecting,
            guild: None,
            channel: None,
            composer: String::new(),
            editing: None,
            hovered: None,
            guild_menu: false,
            modal: None,
            settings: None,
            update: None,
            voice: None,
            voice_mute: false,
            voice_deaf: false,
            audio: session::load_preferences().audio,
            toast: None,
            last_channel: HashMap::new(),
            drafts: HashMap::new(),
            toast_id: 0,
            last_typing: None,
            nonce_prefix: getrandom::u32().unwrap_or(0),
            nonce_seq: 0,
        }));
        Task::none()
    }
}

fn on_key(event: iced::Event, _status: event::Status, _window: window::Id) -> Option<Msg> {
    use keyboard::key::Named;
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), modifiers, .. }) => {
            match named {
                Named::Escape => Some(Msg::Escape),
                Named::Tab if !modifiers.shift() => Some(Msg::FocusNext),
                _ => None,
            }
        }
        _ => None,
    }
}

fn update_login(form: &mut LoginForm, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::LoginServer(value) => form.server = value,
        Msg::LoginUsername(value) => form.username = value,
        Msg::LoginPassword(value) => form.password = value,
        Msg::LoginInviteCode(value) => form.invite_code = value,
        Msg::ToggleRegister => {
            form.register = !form.register;
            form.error = None;
        }
        Msg::SubmitLogin if !form.busy => {
            let checks = if form.register {
                validate::username(&form.username).and_then(|()| validate::password(&form.password))
            } else if form.username.is_empty() || form.password.is_empty() {
                Err("Renseignez votre nom d'utilisateur et votre mot de passe.")
            } else {
                Ok(())
            };
            if let Err(message) = checks {
                form.error = Some(message.to_owned());
                return Task::none();
            }
            form.busy = true;
            form.error = None;
            let api = Api::new(&form.server);
            let server = api.origin().to_owned();
            let username = form.username.trim().to_owned();
            let password = form.password.clone();
            let invite_code = Some(form.invite_code.trim().to_owned()).filter(|code| !code.is_empty());
            let register = form.register;
            return Task::perform(
                async move {
                    let auth = if register {
                        api.register(Registration { username, password, invite_code }).await
                    } else {
                        api.login(Credentials { username, password }).await
                    };
                    auth.map(|auth| (server, auth))
                },
                Msg::LoggedIn,
            );
        }
        Msg::LoggedIn(Err(err)) => {
            form.busy = false;
            form.error = Some(err.message);
        }
        _ => {}
    }
    Task::none()
}

impl Chat {
    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Gateway(GatewayEvent::Ready(ready)) => {
                self.connection = Connection::Online;
                self.me = ready.user.clone();
                self.store.load_ready(*ready);
                // READY reset the cache: reopen the current view (or the first guild).
                let view = match self.guild.filter(|g| self.store.guild(*g).is_some()) {
                    Some(guild) => self.select_guild(guild),
                    None => self.select_first_guild(),
                };
                // After each (re)connection: the server may have been updated meanwhile.
                let api = self.api.clone();
                Task::batch([view, Task::perform(async move { api.instance().await }, Msg::InstanceLoaded)])
            }
            Msg::Gateway(GatewayEvent::Event(event)) => self.on_event(*event),
            Msg::Gateway(GatewayEvent::Disconnected { reason, retry_in }) => {
                self.connection = Connection::Reconnecting { reason, retry_in };
                Task::none()
            }
            Msg::Gateway(GatewayEvent::AuthFailed) => Task::none(), // handled by `App`

            Msg::SelectGuild(guild_id) => self.select_guild(guild_id),
            Msg::SelectChannel(channel_id) if self.store.is_voice_channel(channel_id) => {
                self.update(Msg::JoinVoice(channel_id))
            }
            Msg::SelectChannel(channel_id) => self.select_channel(channel_id),
            Msg::ToggleGuildMenu => {
                self.guild_menu = !self.guild_menu;
                Task::none()
            }

            Msg::History { channel_id, older, result } => match result {
                Ok(page) => {
                    self.store.add_history(channel_id, page, older);
                    Task::none()
                }
                Err(err) => {
                    self.store.set_loading(channel_id, false);
                    self.toast(err.message)
                }
            },
            Msg::LoadOlder => match self.channel.and_then(|c| self.store.can_load_older(c).map(|before| (c, before))) {
                Some((channel_id, before)) => self.fetch_history(channel_id, Some(before)),
                None => Task::none(),
            },
            Msg::Scrolled(viewport) => {
                let scrollable_content = viewport.content_bounds().height > viewport.bounds().height;
                if scrollable_content && viewport.absolute_offset_reversed().y < LOAD_OLDER_THRESHOLD {
                    return self.update(Msg::LoadOlder);
                }
                Task::none()
            }
            Msg::Members { guild_id, result } => match result {
                Ok(members) => {
                    self.store.set_members(guild_id, members);
                    Task::none()
                }
                Err(err) => {
                    self.store.members_failed(guild_id);
                    self.toast(err.message)
                }
            },

            Msg::ComposerChanged(text) => {
                self.composer = text;
                let should_signal = !self.composer.trim().is_empty()
                    && self.last_typing.is_none_or(|at| at.elapsed() >= TYPING_THROTTLE);
                match self.channel {
                    Some(channel_id) if should_signal => {
                        self.last_typing = Some(Instant::now());
                        let api = self.api.clone();
                        Task::perform(async move { api.typing(channel_id).await }, |_| Msg::Ignore)
                    }
                    _ => Task::none(),
                }
            }
            Msg::Send => self.send(),
            Msg::Sent { channel_id, nonce, result } => match result {
                Ok(message) => {
                    self.store.upsert_message(message);
                    Task::none()
                }
                Err(err) => {
                    self.store.fail_pending(channel_id, &nonce);
                    let message = if err.is_rate_limited() {
                        "Doucement ! Vous envoyez des messages trop vite.".to_owned()
                    } else {
                        err.message
                    };
                    self.toast(message)
                }
            },
            Msg::Retry(nonce) => {
                let Some(channel_id) = self.channel else { return Task::none() };
                match self.store.retry_pending(channel_id, &nonce) {
                    Some(content) => self.post_message(channel_id, nonce, content),
                    None => Task::none(),
                }
            }
            Msg::Discard(nonce) => {
                if let Some(channel_id) = self.channel {
                    self.store.remove_pending(channel_id, &nonce);
                }
                Task::none()
            }

            Msg::Hover(id) => {
                self.hovered = Some(id);
                Task::none()
            }
            Msg::Unhover(id) => {
                if self.hovered == Some(id) {
                    self.hovered = None;
                }
                Task::none()
            }
            Msg::Copy(text) => iced::clipboard::write(text),
            Msg::StartEdit(id) => {
                let Some(channel_id) = self.channel else { return Task::none() };
                match self.store.message(channel_id, id) {
                    Some(message) if message.author.id == self.me.id => {
                        self.editing = Some(Editing { id, channel_id, text: message.content.clone() });
                        Task::batch([operation::focus(EDITOR), operation::move_cursor_to_end(EDITOR)])
                    }
                    _ => Task::none(),
                }
            }
            Msg::EditChanged(text) => {
                if let Some(editing) = &mut self.editing {
                    editing.text = text;
                }
                Task::none()
            }
            Msg::SubmitEdit => self.submit_edit(),
            Msg::Edited(result) => match result {
                Ok(message) => {
                    self.editing = None;
                    self.store.upsert_message(message);
                    operation::focus(COMPOSER)
                }
                Err(err) => self.toast(err.message),
            },
            Msg::Delete(id) => {
                let Some(channel_id) = self.channel else { return Task::none() };
                let api = self.api.clone();
                Task::perform(async move { api.delete_message(channel_id, id).await }, move |result| Msg::Deleted {
                    channel_id,
                    id,
                    result,
                })
            }
            Msg::Deleted { channel_id, id, result } => match result {
                Ok(()) => {
                    self.store.remove_message(channel_id, id);
                    Task::none()
                }
                Err(err) => self.toast(err.message),
            },

            Msg::OpenModal(request) => self.open_modal(request),
            Msg::CloseModal => {
                self.modal = None;
                Task::none()
            }
            Msg::ModalInput(text) => {
                if let Some(modal) = &mut self.modal {
                    modal.input = text;
                    modal.error = None;
                }
                Task::none()
            }
            Msg::SubmitModal => self.submit_modal(),
            Msg::ModalDone(Ok(outcome)) => {
                self.modal = None;
                self.apply_outcome(outcome)
            }
            Msg::ModalDone(Err(err)) | Msg::InviteReady(Err(err)) => {
                if let Some(modal) = &mut self.modal {
                    modal.busy = false;
                    modal.error = Some(err.message);
                }
                Task::none()
            }
            Msg::InviteReady(Ok(created)) => {
                if let Some(Modal { kind: ModalKind::Invite { invite }, .. }) = &mut self.modal {
                    *invite = Some(created);
                }
                Task::none()
            }

            Msg::Tick => {
                self.store.expire_typing(Instant::now());
                Task::none()
            }
            Msg::Escape => {
                if self.settings.take().is_none() && self.modal.take().is_none() && self.editing.take().is_none() {
                    self.guild_menu = false;
                }
                Task::none()
            }

            Msg::OpenSettings => {
                self.settings = Some(Settings::default());
                Task::batch([self.load_sessions(), Task::perform(voice::devices(), Msg::DevicesLoaded)])
            }
            Msg::DevicesLoaded(result) => {
                if let Some(settings) = &mut self.settings {
                    settings.devices = Some(result);
                }
                Task::none()
            }
            Msg::SetInputDevice(device) => self.edit_audio(|audio| audio.input_device = Some(device.id)),
            Msg::SetOutputDevice(device) => self.edit_audio(|audio| audio.output_device = Some(device.id)),
            Msg::SetSensitivity(db) => self.edit_audio(|audio| audio.sensitivity_db = db),
            Msg::SetNoiseSuppression(on) => self.edit_audio(|audio| audio.noise_suppression = on),
            Msg::SetEchoCancellation(on) => self.edit_audio(|audio| audio.echo_cancellation = on),

            Msg::JoinVoice(channel_id) => {
                if self.voice.as_ref().is_some_and(|v| v.channel_id == channel_id) {
                    return Task::none();
                }
                let Some(guild_id) = self.store.channel(channel_id).map(|c| c.guild_id) else { return Task::none() };
                // Hopping channels: leave the current room first.
                if let Some(commands) = self.voice.as_ref().and_then(|v| v.commands.as_ref()) {
                    commands.send(voice::Command::Leave);
                }
                self.voice = Some(VoiceSession {
                    guild_id,
                    channel_id,
                    target: None,
                    commands: None,
                    speaking: HashSet::new(),
                    level: -100.0,
                });
                let api = self.api.clone();
                Task::perform(async move { api.join_voice(channel_id).await }, move |result| Msg::VoiceTarget {
                    channel_id,
                    result,
                })
            }
            Msg::VoiceTarget { channel_id, result } => {
                let Some(session) = self.voice.as_mut().filter(|v| v.channel_id == channel_id) else {
                    return Task::none();
                };
                match result {
                    Ok(join) => {
                        session.target = Some(voice::Target { url: join.url, token: join.token, channel_id });
                        Task::none()
                    }
                    Err(err) => {
                        self.voice = None;
                        self.toast(err.message)
                    }
                }
            }
            Msg::Voice(event) => self.on_voice(event),
            Msg::LeaveVoice => {
                match self.voice.as_ref().and_then(|v| v.commands.as_ref()) {
                    // The call ends cleanly, then reports `Ended`.
                    Some(commands) => commands.send(voice::Command::Leave),
                    None => self.voice = None,
                }
                Task::none()
            }
            Msg::ToggleMute => {
                self.voice_mute = !self.voice_mute;
                if !self.voice_mute {
                    // Speaking again means hearing again.
                    self.voice_deaf = false;
                }
                self.push_switches()
            }
            Msg::ToggleDeaf => {
                self.voice_deaf = !self.voice_deaf;
                self.push_switches()
            }
            Msg::ServerMute { guild_id, user_id, muted } => {
                self.modal = None;
                let api = self.api.clone();
                Task::perform(async move { api.server_mute(guild_id, user_id, muted).await }, Msg::Moderated)
            }
            Msg::DisconnectFromVoice { guild_id, user_id } => {
                self.modal = None;
                let api = self.api.clone();
                Task::perform(async move { api.disconnect_from_voice(guild_id, user_id).await }, Msg::Moderated)
            }
            Msg::Moderated(Ok(())) => Task::none(),
            Msg::Moderated(Err(err)) => self.toast(err.message),
            Msg::SettingsCurrent(value) => self.edit_settings(|s| s.current = value),
            Msg::SettingsNew(value) => self.edit_settings(|s| s.new = value),
            Msg::SettingsConfirm(value) => self.edit_settings(|s| s.confirm = value),
            Msg::SubmitPassword => {
                let Some(settings) = &mut self.settings else { return Task::none() };
                if settings.busy {
                    return Task::none();
                }
                let checks = if settings.current.is_empty() {
                    Err("Renseignez votre mot de passe actuel.")
                } else if settings.new != settings.confirm {
                    Err("Les deux nouveaux mots de passe ne sont pas identiques.")
                } else {
                    validate::password(&settings.new)
                };
                if let Err(message) = checks {
                    settings.error = Some(message.to_owned());
                    return Task::none();
                }
                settings.busy = true;
                let api = self.api.clone();
                let (current, new) = (settings.current.clone(), settings.new.clone());
                Task::perform(async move { api.change_password(current, new).await }, Msg::PasswordChanged)
            }
            Msg::PasswordChanged(result) => {
                let Some(settings) = &mut self.settings else { return Task::none() };
                settings.busy = false;
                match result {
                    Ok(()) => {
                        *settings = Settings { saved: true, ..Settings::default() };
                        // The other devices were logged out.
                        self.load_sessions()
                    }
                    Err(err) => {
                        settings.error = Some(err.message);
                        Task::none()
                    }
                }
            }
            Msg::SessionsLoaded(result) => {
                if let Some(settings) = &mut self.settings {
                    settings.sessions = Some(result.map_err(|err| err.message));
                }
                Task::none()
            }
            Msg::RevokeSession(id) => {
                let api = self.api.clone();
                Task::perform(async move { api.revoke_session(id).await }, move |result| Msg::SessionRevoked {
                    id,
                    result,
                })
            }
            Msg::SessionRevoked { id, result } => match result {
                Ok(()) => {
                    if let Some(Settings { sessions: Some(Ok(sessions)), .. }) = &mut self.settings {
                        sessions.retain(|session| session.id != id);
                    }
                    Task::none()
                }
                Err(err) => self.toast(err.message),
            },
            Msg::InstanceLoaded(Ok(info)) => {
                self.update = info.client.filter(|client| api::is_newer(&client.version, env!("CARGO_PKG_VERSION")));
                Task::none()
            }
            Msg::InstanceLoaded(Err(_)) => Task::none(), // an older server: nothing to offer
            Msg::OpenUrl(url) => match api::open_url(&url) {
                Ok(()) => Task::none(),
                Err(_) => self.toast(format!("Impossible d'ouvrir le navigateur. Adresse : {url}")),
            },
            Msg::DismissToast(id) => {
                if id == self.toast_id {
                    self.toast = None;
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn on_voice(&mut self, event: VoiceEvent) -> Task<Msg> {
        match event {
            VoiceEvent::Connected(commands) => {
                let Some(session) = &mut self.voice else { return Task::none() };
                commands.send(voice::Command::Settings(self.audio.clone()));
                session.commands = Some(commands);
                self.push_switches()
            }
            VoiceEvent::Speaking(speaking) => {
                if let Some(session) = &mut self.voice {
                    session.speaking = speaking;
                }
                Task::none()
            }
            VoiceEvent::Level(level) => {
                if let Some(session) = &mut self.voice {
                    session.level = level;
                }
                Task::none()
            }
            VoiceEvent::Ended(reason) => {
                self.voice = None;
                match reason {
                    Some(reason) => self.toast(reason),
                    None => Task::none(),
                }
            }
        }
    }

    /// Sends the microphone and sound switches to the call and to the server (which shows
    /// them to the others).
    fn push_switches(&self) -> Task<Msg> {
        let (mute, deaf) = (self.voice_mute || self.voice_deaf, self.voice_deaf);
        let Some(session) = &self.voice else { return Task::none() };
        if let Some(commands) = &session.commands {
            commands.send(voice::Command::Mute(mute));
            commands.send(voice::Command::Deafen(deaf));
        }
        let api = self.api.clone();
        Task::perform(async move { api.update_voice_state(mute, deaf).await }, |_| Msg::Ignore)
    }

    fn edit_audio(&mut self, edit: impl FnOnce(&mut AudioSettings)) -> Task<Msg> {
        edit(&mut self.audio);
        let preferences = Preferences { audio: self.audio.clone(), ..session::load_preferences() };
        if let Err(err) = session::save_preferences(&preferences) {
            eprintln!("could not persist the preferences: {err}");
        }
        if let Some(commands) = self.voice.as_ref().and_then(|v| v.commands.as_ref()) {
            commands.send(voice::Command::Settings(self.audio.clone()));
        }
        Task::none()
    }

    fn load_sessions(&self) -> Task<Msg> {
        let api = self.api.clone();
        Task::perform(async move { api.sessions().await }, Msg::SessionsLoaded)
    }

    fn edit_settings(&mut self, edit: impl FnOnce(&mut Settings)) -> Task<Msg> {
        if let Some(settings) = &mut self.settings {
            edit(settings);
            settings.error = None;
            settings.saved = false;
        }
        Task::none()
    }

    fn on_event(&mut self, event: Event) -> Task<Msg> {
        if let Event::MessageCreate(message) = &event
            && message.author.id != self.me.id
            && Some(message.channel_id) != self.channel
        {
            self.store.mark_unread(message.channel_id);
        }
        let removed_guild = matches!(&event, Event::GuildDelete { id } if Some(*id) == self.guild);
        // Our voice channel (or its whole guild) disappeared: the call is over.
        let lost_voice = self.voice.as_ref().is_some_and(|v| match &event {
            Event::GuildDelete { id } => *id == v.guild_id,
            Event::ChannelDelete { id, .. } => *id == v.channel_id,
            _ => false,
        });
        if lost_voice && let Some(commands) = self.voice.as_ref().and_then(|v| v.commands.as_ref()) {
            commands.send(voice::Command::Leave);
        }
        let removed_channel = match &event {
            Event::ChannelDelete { id, guild_id } if Some(*id) == self.channel => Some(*guild_id),
            _ => None,
        };
        self.store.apply(event, self.me.id);

        if removed_guild {
            self.guild = None;
            self.channel = None; // its channels are gone: nothing to save or trim
            return self.select_first_guild();
        }
        if let Some(guild_id) = removed_channel {
            self.channel = None;
            return self.select_guild(guild_id);
        }
        if self.guild.is_none() {
            return self.select_first_guild();
        }
        Task::none()
    }

    // --- Navigation ------------------------------------------------------------------------------

    fn select_first_guild(&mut self) -> Task<Msg> {
        let first = self.store.guilds().next().map(|g| g.id);
        match first {
            Some(guild_id) => self.select_guild(guild_id),
            None => {
                self.guild = None;
                self.switch_channel(None);
                Task::none()
            }
        }
    }

    fn select_guild(&mut self, guild_id: Snowflake) -> Task<Msg> {
        self.guild = Some(guild_id);
        self.guild_menu = false;
        let members = if self.store.request_members(guild_id) {
            let api = self.api.clone();
            Task::perform(async move { api.members(guild_id).await }, move |result| Msg::Members { guild_id, result })
        } else {
            Task::none()
        };
        let channel =
            self.last_channel.get(&guild_id).copied().filter(|c| self.store.channel(*c).is_some()).or_else(|| {
                self.store.channels_of(guild_id).into_iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id)
            });
        let open = match channel {
            Some(channel_id) => self.select_channel(channel_id),
            None => {
                self.switch_channel(None);
                Task::none()
            }
        };
        Task::batch([members, open])
    }

    fn select_channel(&mut self, channel_id: Snowflake) -> Task<Msg> {
        let Some(guild_id) = self.store.channel(channel_id).map(|c| c.guild_id) else { return Task::none() };
        self.switch_channel(Some(channel_id));
        self.last_channel.insert(guild_id, channel_id);
        self.store.mark_read(channel_id);
        let history =
            if self.store.needs_history(channel_id) { self.fetch_history(channel_id, None) } else { Task::none() };
        Task::batch([
            history,
            // Anchored at the bottom: relative offset 0 = newest messages.
            operation::snap_to(MESSAGES, scrollable::RelativeOffset::START),
            operation::focus(COMPOSER),
        ])
    }

    /// Saves/restores drafts and trims the history of the channel we leave.
    fn switch_channel(&mut self, next: Option<Snowflake>) {
        if self.channel == next {
            return;
        }
        if let Some(previous) = self.channel {
            let draft = std::mem::take(&mut self.composer);
            if draft.is_empty() {
                self.drafts.remove(&previous);
            } else {
                self.drafts.insert(previous, draft);
            }
            self.store.trim(previous, KEEP_HIDDEN_MESSAGES);
        }
        self.composer = next.and_then(|c| self.drafts.remove(&c)).unwrap_or_default();
        self.editing = None;
        self.hovered = None;
        self.last_typing = None;
        self.channel = next;
    }

    fn fetch_history(&mut self, channel_id: Snowflake, before: Option<Snowflake>) -> Task<Msg> {
        self.store.set_loading(channel_id, true);
        let api = self.api.clone();
        Task::perform(async move { api.messages(channel_id, before).await }, move |result| Msg::History {
            channel_id,
            older: before.is_some(),
            result,
        })
    }

    // --- Messages --------------------------------------------------------------------------------

    fn send(&mut self) -> Task<Msg> {
        let Some(channel_id) = self.channel else { return Task::none() };
        if self.composer.trim().is_empty() {
            return Task::none();
        }
        let content = match validate::message_content(&self.composer) {
            Ok(content) => content,
            Err(message) => return self.toast(message.to_owned()),
        };
        self.composer.clear();
        self.last_typing = None;
        self.nonce_seq += 1;
        let nonce = format!("{:08x}-{}", self.nonce_prefix, self.nonce_seq);
        self.store.add_pending(channel_id, nonce.clone(), content.clone());
        Task::batch([
            self.post_message(channel_id, nonce, content),
            operation::snap_to(MESSAGES, scrollable::RelativeOffset::START),
        ])
    }

    fn post_message(&self, channel_id: Snowflake, nonce: String, content: String) -> Task<Msg> {
        let api = self.api.clone();
        let request_nonce = nonce.clone();
        Task::perform(async move { api.send_message(channel_id, content, request_nonce).await }, move |result| {
            Msg::Sent { channel_id, nonce, result }
        })
    }

    fn submit_edit(&mut self) -> Task<Msg> {
        let Some(editing) = &self.editing else { return Task::none() };
        let unchanged =
            self.store.message(editing.channel_id, editing.id).is_some_and(|m| m.content == editing.text.trim());
        if unchanged {
            self.editing = None;
            return operation::focus(COMPOSER);
        }
        let content = match validate::message_content(&editing.text) {
            Ok(content) => content,
            Err(message) => return self.toast(message.to_owned()),
        };
        let (api, channel_id, id) = (self.api.clone(), editing.channel_id, editing.id);
        Task::perform(async move { api.edit_message(channel_id, id, content).await }, Msg::Edited)
    }

    // --- Modals ----------------------------------------------------------------------------------

    fn open_modal(&mut self, request: ModalRequest) -> Task<Msg> {
        self.guild_menu = false;
        let kind = match request {
            ModalRequest::CreateGuild => ModalKind::CreateGuild,
            ModalRequest::JoinGuild => ModalKind::JoinGuild,
            ModalRequest::CreateChannel(guild_id, kind) => ModalKind::CreateChannel(guild_id, kind),
            ModalRequest::Invite(_) => ModalKind::Invite { invite: None },
            ModalRequest::LeaveGuild(guild_id) => ModalKind::LeaveGuild(guild_id),
            ModalRequest::DeleteGuild(guild_id) => ModalKind::DeleteGuild(guild_id),
            ModalRequest::DeleteChannel(channel_id) => ModalKind::DeleteChannel(channel_id),
            ModalRequest::VoiceMember { guild_id, user_id } => ModalKind::VoiceMember { guild_id, user_id },
        };
        self.modal = Some(Modal { kind, input: String::new(), busy: false, error: None });
        match request {
            ModalRequest::Invite(guild_id) => {
                let api = self.api.clone();
                Task::perform(async move { api.create_invite(guild_id).await }, Msg::InviteReady)
            }
            ModalRequest::CreateGuild | ModalRequest::JoinGuild | ModalRequest::CreateChannel(..) => {
                operation::focus(MODAL_INPUT)
            }
            _ => Task::none(),
        }
    }

    fn submit_modal(&mut self) -> Task<Msg> {
        let Some(modal) = &mut self.modal else { return Task::none() };
        if modal.busy {
            return Task::none();
        }
        let api = self.api.clone();
        let task = match &modal.kind {
            ModalKind::CreateGuild => match validate::guild_name(&modal.input) {
                Ok(name) => {
                    Task::perform(async move { api.create_guild(name).await.map(ModalOutcome::Guild) }, Msg::ModalDone)
                }
                Err(message) => return invalid(modal, message),
            },
            ModalKind::JoinGuild => match api::parse_invite_code(&modal.input) {
                Some(code) => {
                    Task::perform(async move { api.accept_invite(code).await.map(ModalOutcome::Guild) }, Msg::ModalDone)
                }
                None => return invalid(modal, "Collez un code ou un lien d'invitation valide."),
            },
            ModalKind::CreateChannel(guild_id, kind) => match validate::channel_name(&modal.input) {
                Ok(name) => {
                    let (guild_id, kind) = (*guild_id, *kind);
                    Task::perform(
                        async move { api.create_channel(guild_id, name, kind).await.map(ModalOutcome::Channel) },
                        Msg::ModalDone,
                    )
                }
                Err(message) => return invalid(modal, message),
            },
            // Their buttons send their own messages; Enter just closes.
            ModalKind::Invite { .. } | ModalKind::VoiceMember { .. } => {
                self.modal = None;
                return Task::none();
            }
            ModalKind::LeaveGuild(guild_id) => {
                let guild_id = *guild_id;
                Task::perform(
                    async move { api.leave_guild(guild_id).await.map(|()| ModalOutcome::GuildRemoved(guild_id)) },
                    Msg::ModalDone,
                )
            }
            ModalKind::DeleteGuild(guild_id) => {
                let guild_id = *guild_id;
                Task::perform(
                    async move { api.delete_guild(guild_id).await.map(|()| ModalOutcome::GuildRemoved(guild_id)) },
                    Msg::ModalDone,
                )
            }
            ModalKind::DeleteChannel(channel_id) => {
                let channel_id = *channel_id;
                Task::perform(
                    async move { api.delete_channel(channel_id).await.map(|()| ModalOutcome::ChannelRemoved(channel_id)) },
                    Msg::ModalDone,
                )
            }
        };
        modal.busy = true;
        modal.error = None;
        task
    }

    /// Applies the REST result right away (the gateway echo, if any, is idempotent).
    fn apply_outcome(&mut self, outcome: ModalOutcome) -> Task<Msg> {
        match outcome {
            ModalOutcome::Guild(guild) => {
                let guild_id = guild.guild.id;
                self.store.apply(Event::GuildCreate(guild), self.me.id);
                self.select_guild(guild_id)
            }
            ModalOutcome::Channel(channel) => {
                let (channel_id, kind) = (channel.id, channel.kind);
                self.store.apply(Event::ChannelCreate(channel), self.me.id);
                if kind == ChannelKind::Text { self.select_channel(channel_id) } else { Task::none() }
            }
            ModalOutcome::GuildRemoved(guild_id) => self.on_event(Event::GuildDelete { id: guild_id }),
            ModalOutcome::ChannelRemoved(channel_id) => match self.store.channel(channel_id).map(|c| c.guild_id) {
                Some(guild_id) => self.on_event(Event::ChannelDelete { id: channel_id, guild_id }),
                None => Task::none(),
            },
        }
    }

    fn toast(&mut self, text: String) -> Task<Msg> {
        self.toast_id += 1;
        self.toast = Some(text);
        let id = self.toast_id;
        Task::perform(tokio::time::sleep(TOAST_DURATION), move |()| Msg::DismissToast(id))
    }

    // --- Read helpers for the views --------------------------------------------------------------

    pub fn is_owner(&self, guild_id: Snowflake) -> bool {
        self.store.guild(guild_id).is_some_and(|g| g.owner_id == self.me.id)
    }

    pub fn current_channel(&self) -> Option<&Channel> {
        self.channel.and_then(|id| self.store.channel(id))
    }
}

fn invalid(modal: &mut Modal, message: &str) -> Task<Msg> {
    modal.error = Some(message.to_owned());
    Task::none()
}
