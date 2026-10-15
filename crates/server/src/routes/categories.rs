//! Categories: the headings that group a guild's channels (manage channels).

use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{CreateCategory, UpdateCategory};
use plume_lite_protocol::model::{AuditAction, Category};
use plume_lite_protocol::permissions::Permissions;
use plume_lite_protocol::{Snowflake, layout, validate};

use super::{UNKNOWN_CATEGORY, dispatch_layout, require_permission};
use crate::auth::AuthUser;
use crate::db::channels::CategoryAccess;
use crate::db::{self, MAX_CATEGORIES_PER_GUILD};
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

/// `POST /guilds/{id}/categories`: a new category, below the others.
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<CreateCategory>,
) -> Result<(StatusCode, Json<Category>)> {
    require_permission(&state, guild_id, caller.user.id, Permissions::MANAGE_CHANNELS).await?;
    let name = validate::category_name(&body.name)?;
    if db::channels::count_categories(&state.db, guild_id).await? >= MAX_CATEGORIES_PER_GUILD {
        return Err(AppError::BadRequest("Ce serveur a atteint la limite de 50 catégories.".into()));
    }
    let id = state.ids.next();
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    db::channels::create_category(&mut tx, id, guild_id, &name).await?;
    let before = db::channels::layout(&mut tx, guild_id).await?;
    let mut after = before.clone();
    layout::renumber(&mut after.categories, &mut after.channels);
    let (channels, categories) = db::channels::save_layout(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let category = after.categories.into_iter().find(|c| c.id == id).ok_or(AppError::NotFound(UNKNOWN_CATEGORY))?;
    let record =
        db::audit::Record::new(guild_id, caller.user.id, AuditAction::CategoryCreate).target(id, &category.name);
    super::audit(&state, record).await;
    state.hub.dispatch_to_guild(guild_id, Event::CategoryCreate(category.clone()));
    dispatch_layout(&state, guild_id, channels, categories, Some(id)).await;
    Ok((StatusCode::CREATED, Json(category)))
}

/// `PATCH /categories/{id}`: renames a category or moves it among the others (its channels
/// follow it: each one whose rank changed is broadcast).
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(category_id): Path<Snowflake>,
    Json(body): Json<UpdateCategory>,
) -> Result<Json<Category>> {
    let access = require_category(&state, category_id, caller.user.id).await?;
    let name = body.name.as_deref().map(validate::category_name).transpose()?;
    let guild_id = access.category.guild_id;

    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let before = db::channels::layout(&mut tx, guild_id).await?;
    let mut after = before.clone();
    let Some(category) = after.categories.iter_mut().find(|c| c.id == category_id) else {
        return Err(AppError::NotFound(UNKNOWN_CATEGORY)); // deleted meanwhile
    };
    if let Some(name) = name {
        category.name = name;
    }
    if let Some(position) = body.position {
        let index = usize::try_from(position).unwrap_or(usize::MAX);
        layout::move_category(&mut after.categories, &mut after.channels, category_id, index);
    }
    let (channels, categories) = db::channels::save_layout(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let updated =
        after.categories.into_iter().find(|c| c.id == category_id).ok_or(AppError::NotFound(UNKNOWN_CATEGORY))?;
    let record = db::audit::Record::new(guild_id, caller.user.id, AuditAction::CategoryUpdate)
        .target(category_id, &updated.name)
        .change("nom", Some(access.category.name.clone()), Some(updated.name.clone()))
        .change("rang", None, body.position.map(|_| "déplacée".to_owned()));
    super::audit(&state, record).await;
    dispatch_layout(&state, guild_id, channels, categories, None).await;
    Ok(Json(updated))
}

/// `DELETE /categories/{id}`: its channels stay, above every category.
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(category_id): Path<Snowflake>,
) -> Result<StatusCode> {
    let access = require_category(&state, category_id, caller.user.id).await?;
    let guild_id = access.category.guild_id;

    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let before = db::channels::layout(&mut tx, guild_id).await?;
    db::channels::delete_category(&mut tx, category_id).await?;
    let mut after = before.clone();
    after.categories.retain(|c| c.id != category_id);
    for channel in after.channels.iter_mut().filter(|c| c.category_id == Some(category_id)) {
        channel.category_id = None;
    }
    layout::renumber(&mut after.categories, &mut after.channels);
    let (channels, categories) = db::channels::save_layout(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let record = db::audit::Record::new(guild_id, caller.user.id, AuditAction::CategoryDelete)
        .target(category_id, &access.category.name);
    super::audit(&state, record).await;
    state.hub.dispatch_to_guild(guild_id, Event::CategoryDelete { id: category_id, guild_id });
    dispatch_layout(&state, guild_id, channels, categories, None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Non-members get a 404 (as for channels), members who may not manage channels a 403.
async fn require_category(state: &AppState, category_id: Snowflake, user_id: Snowflake) -> Result<CategoryAccess> {
    let access = db::channels::category_access(&state.db, category_id, user_id)
        .await?
        .ok_or(AppError::NotFound(UNKNOWN_CATEGORY))?;
    require_permission(state, access.category.guild_id, user_id, Permissions::MANAGE_CHANNELS).await?;
    Ok(access)
}
