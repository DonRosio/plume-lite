//! Wire protocol shared by the Plume Lite server and its clients.
//!
//! Everything that crosses the network lives here, so the server and the native
//! client are guaranteed to agree on the format at compile time:
//!
//! - [`Snowflake`]: 64-bit, time-sortable identifiers (serialised as strings).
//! - [`model`]: resources returned by the API (users, guilds, channels, messages…).
//! - [`http`]: REST request/response bodies and the error envelope.
//! - [`gateway`]: realtime WebSocket frames and events.
//! - [`layout`]: the display order of a guild's channels and categories.
//! - [`permissions`]: roles, permissions and the ranking of members.
//! - [`markup`]: the Markdown subset of messages (formatting, mentions).
//! - [`validate`]: input rules enforced by the server and pre-checked by clients.

pub mod gateway;
pub mod http;
pub mod layout;
pub mod markup;
pub mod model;
pub mod permissions;
mod snowflake;
pub mod validate;

pub use snowflake::{EPOCH_MS, ParseSnowflakeError, Snowflake};

/// Prefix of every REST route. Bump on breaking changes, keep the old one alive meanwhile.
pub const API_PREFIX: &str = "/api/v1";

/// Path of the realtime gateway (WebSocket upgrade).
pub const GATEWAY_PATH: &str = "/gateway";
