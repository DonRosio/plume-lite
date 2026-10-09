//! Extractors whose rejections use the API error envelope instead of axum's plain text.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequest, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::state::AppState;

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct Json<T>(pub T);

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct Path<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct Query<T>(pub T);

/// Best-effort client IP, used as a rate-limit key.
///
/// Behind a reverse proxy (`TRUST_PROXY=true`) the right-most `X-Forwarded-For`
/// entry is the one appended by our proxy, so it is the only one we trust.
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Infallible> {
        Ok(Self(client_ip(&parts.headers, &parts.extensions, state.config.trust_proxy)))
    }
}

pub fn client_ip(headers: &HeaderMap, extensions: &Extensions, trust_proxy: bool) -> IpAddr {
    let forwarded = trust_proxy
        .then(|| headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next())
        .flatten()
        .and_then(|ip| ip.trim().parse().ok());
    let peer = extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip());
    forwarded.or(peer).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}
