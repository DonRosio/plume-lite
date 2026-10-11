//! Public facts about the server, and the page that hands out the desktop client.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use plume_lite_protocol::http::{ClientDownload, ClientRelease, InstanceInfo, Platform};

use crate::downloads;
use crate::extract::Json;
use crate::state::AppState;

/// Server and clients are released together, with the workspace version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn info(State(state): State<AppState>) -> Json<InstanceInfo> {
    Json(InstanceInfo {
        version: VERSION.to_owned(),
        open_registration: state.config.open_registration,
        client: client_release(&state).await,
    })
}

/// The builds in `DOWNLOADS_DIR` (the release image ships them), `None` when there are none.
async fn client_release(state: &AppState) -> Option<ClientRelease> {
    let downloads = state.downloads.list(state.config.downloads_dir.as_ref()?).await;
    // Clients before 0.5.1 only know `download_path`, and they are Windows clients.
    let legacy = downloads.iter().find(|d| d.platform == Platform::Windows).or(downloads.first())?;
    Some(ClientRelease { version: VERSION.to_owned(), download_path: legacy.path.clone(), downloads })
}

/// `GET /`: how to join, in four steps, with the download links.
pub async fn home(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let download = match client_release(&state).await {
        Some(client) => {
            let links: String = client.downloads.iter().map(download_link).collect();
            format!(r#"{links}<p class="muted">Version {}.</p>"#, client.version)
        }
        None => r#"<span class="muted">Le client n'est pas encore disponible sur ce serveur.</span>"#.to_owned(),
    };
    let page = HOME
        .replace("{download}", &download)
        .replace("{address}", &address(&headers))
        .replace("{public_key}", downloads::public_key_base64());
    let mut response = Html(page).into_response();
    // No script at all; inline styles only.
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
    );
    response
}

/// A download button, with what it takes to check the file.
fn download_link(download: &ClientDownload) -> String {
    let label = match download.platform {
        Platform::Windows => "Télécharger pour Windows",
        Platform::Linux => "Télécharger pour Linux (x86-64)",
        Platform::Unknown => "Télécharger",
    };
    let signature = download
        .signature_path
        .as_ref()
        .map(|path| format!(r#" · <a href="{path}">signature</a>"#))
        .unwrap_or_default();
    format!(
        r#"<div class="download"><a class="button" href="{path}">{label}</a><span class="muted">{size:.1} Mo{signature}</span><br><span class="hash">SHA-256 <code>{sha256}</code></span></div>"#,
        path = download.path,
        size = download.size as f64 / 1_000_000.0,
        sha256 = download.sha256,
    )
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
  .download { margin-bottom: 8px; }
  .hash { font-size: 12px; word-break: break-all; }
  code { background: #f3f8fe; padding: 1px 6px; border-radius: 4px; }
  a { color: inherit; }
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
    <li>Sous Windows, lancez <code>plume-lite.exe</code>. Windows peut afficher un avertissement
      (pas de certificat de signature Windows) : « Informations complémentaires », puis
      « Exécuter quand même ». Sous Linux : <code>tar xzf plume-lite-linux-x64.tar.gz</code>, puis
      <code>./plume-lite/plume-lite</code>.</li>
    <li>Cliquez sur « Inscrivez-vous » et indiquez comme serveur {address}.</li>
    <li>Entrez le code d'invitation que l'on vous a donné.</li>
  </ol>
  <p class="muted hash">Chaque fichier est signé avec <a href="https://jedisct1.github.io/minisign/">minisign</a> :
    <code>minisign -Vm &lt;fichier&gt; -P {public_key}</code>. Une fois installé, le client vérifie
    lui-même la signature de ses mises à jour.</p>
</main>
</body>
</html>
"#;
