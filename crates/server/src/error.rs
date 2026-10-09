use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use plume_lite_protocol::http::{ApiError, ErrorCode};

pub type Result<T, E = AppError> = std::result::Result<T, E>;

/// Every error a handler can return. Converted into the `ApiError` JSON envelope.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("{0}")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("rate limited")]
    RateLimited(Duration),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    pub fn unauthenticated() -> Self {
        Self::Unauthorized("Session invalide ou expirée.")
    }
}

impl From<&'static str> for AppError {
    /// Validation failures (`plume_lite_protocol::validate`) are plain user-facing strings.
    fn from(message: &'static str) -> Self {
        Self::BadRequest(message.to_owned())
    }
}

impl From<JsonRejection> for AppError {
    fn from(rejection: JsonRejection) -> Self {
        Self::BadRequest(format!("Corps de requête invalide : {}", rejection.body_text()))
    }
}

impl From<PathRejection> for AppError {
    fn from(rejection: PathRejection) -> Self {
        Self::BadRequest(format!("Chemin invalide : {}", rejection.body_text()))
    }
}

impl From<QueryRejection> for AppError {
    fn from(rejection: QueryRejection) -> Self {
        Self::BadRequest(format!("Paramètres invalides : {}", rejection.body_text()))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            Self::Conflict(_) => (StatusCode::CONFLICT, ErrorCode::Conflict),
            Self::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::RateLimited),
            Self::Database(_) | Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal),
        };

        let message = match &self {
            // Never leak internals to clients; the details go to the logs.
            Self::Database(err) => {
                tracing::error!(error = %err, "database error");
                "Erreur interne du serveur.".to_owned()
            }
            Self::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                "Erreur interne du serveur.".to_owned()
            }
            Self::RateLimited(_) => "Trop de requêtes, réessayez dans un instant.".to_owned(),
            other => other.to_string(),
        };

        let retry_after = match self {
            Self::RateLimited(wait) => Some(wait),
            _ => None,
        };

        let body = ApiError {
            code,
            message,
            retry_after_ms: retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        };

        let mut response = (status, Json(body)).into_response();
        if let Some(wait) = retry_after
            && let Ok(value) = (wait.as_secs() + 1).to_string().parse()
        {
            response.headers_mut().insert(axum::http::header::RETRY_AFTER, value);
        }
        response
    }
}
