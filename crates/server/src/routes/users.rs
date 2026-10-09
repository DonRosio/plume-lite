use axum::extract::State;
use plume_lite_protocol::model::{FullGuild, User};

use crate::auth::AuthUser;
use crate::db;
use crate::error::Result;
use crate::extract::Json;
use crate::state::AppState;

pub async fn me(caller: AuthUser) -> Json<User> {
    Json(caller.user)
}

pub async fn guilds(State(state): State<AppState>, caller: AuthUser) -> Result<Json<Vec<FullGuild>>> {
    Ok(Json(db::guilds::full_for_user(&state.db, caller.user.id).await?))
}
