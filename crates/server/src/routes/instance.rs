//! Public facts about the server, and the page that hands out the desktop client.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use plume_lite_protocol::http::{ClientRelease, InstanceInfo};

use crate::extract::Json;
use crate::state::AppState;

/// Server and clients are released together, with the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The Windows client, in `DOWNLOADS_DIR` (the release image ships it).
pub const CLIENT_FILE: &str = "plume-lite.exe";

pub async fn info(State(state): State<AppState>) -> Json<InstanceInfo> {
    Json(InstanceInfo {
        version: VERSION.to_owned(),
        open_registration: state.config.open_registration,
        client: client_release(&state).await,
    })
}

async fn client_release(state: &AppState) -> Option<ClientRelease> {
    let path = state.config.downloads_dir.as_ref()?.join(CLIENT_FILE);
    let file = tokio::fs::metadata(path).await.ok()?;
    file.is_file()
        .then(|| ClientRelease { version: VERSION.to_owned(), download_path: format!("/download/{CLIENT_FILE}") })
}

/// `GET /`: how to join, in four steps, with the download link.
pub async fn home(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let download = match client_release(&state).await {
        Some(client) => format!(
            r#"<a class="button" href="{}">Télécharger pour Windows</a><span class="muted">version {}</span>"#,
            client.download_path, client.version
        ),
        None => {
            r#"<span class="muted">Le client Windows n'est pas encore disponible sur ce serveur.</span>"#.to_owned()
        }
    };
    let page = HOME.replace("{download}", &download).replace("{address}", &address(&headers));
    let mut response = Html(page).into_response();
    // No script at all; inline styles only.
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
    );
    response
}

/// The address to type in the client, from the `Host` header. Only plain host names are
/// echoed back into the page.
fn address(headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|value| value.to_str().ok()).unwrap_or_default();
    let plain = !host.is_empty()
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !plain {
        return "l'adresse de cette page".to_owned();
    }
    let local = host.starts_with("127.") || host.starts_with("localhost") || host.starts_with('[');
    format!("<code>{}://{host}</code>", if local { "http" } else { "https" })
}

const HOME: &str = r#"<!doctype html>
<html lang="fr">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Plume Lite</title>
<style>
  body { margin: 0; font-family: system-ui, sans-serif; background: #e3eefa; color: #1d2939; }
  main { max-width: 560px; margin: 0 auto; padding: 48px 20px; }
  h1 { margin: 0 0 4px; color: #0e1a2c; }
  .muted { color: #42526a; }
  ol { padding-left: 20px; line-height: 1.6; }
  li { margin-bottom: 12px; }
  .button { display: inline-block; margin: 8px 12px 4px 0; padding: 10px 18px; border-radius: 6px;
            background: #2b67ad; color: #fff; text-decoration: none; font-weight: 600; }
  code { background: #f3f8fe; padding: 1px 6px; border-radius: 4px; }
  @media (prefers-color-scheme: dark) {
    body { background: #171c27; color: #e6ecf5; }
    h1 { color: #f5f8fc; }
    .muted { color: #9aa7ba; }
    .button { background: #8db8ec; color: #10141c; }
    code { background: #232a39; }
  }
</style>
</head>
<body>
<main>
  <h1>Plume Lite</h1>
  <p class="muted">Une alternative à Discord, légère et hébergée entre amis.</p>
  <ol>
    <li>{download}</li>
    <li>Lancez <code>plume-lite.exe</code>. Windows peut afficher un avertissement (fichier non signé) :
      « Informations complémentaires », puis « Exécuter quand même ».</li>
    <li>Cliquez sur « Inscrivez-vous » et indiquez comme serveur {address}.</li>
    <li>Entrez le code d'invitation que l'on vous a donné.</li>
  </ol>
</main>
</body>
</html>
"#;
