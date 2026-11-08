mod batch;
mod cli;
mod clip;
mod config;
mod logging;
mod metadata;
mod playlist;
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{distributions::Alphanumeric, Rng};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    net::TcpListener,
};
use url::Url;

const CLIENT_FILE: &str = "client_secret.json";
const TOKEN_FILE: &str = "token.json";
const REDIRECT: &str = "http://127.0.0.1:8765/callback";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
const SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
const PLAYLIST_SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";
const CHUNK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct GoogleClientFile {
    installed: Option<GoogleClient>,
    web: Option<GoogleClient>,
}

#[derive(Deserialize)]
struct GoogleClient {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
    #[serde(default)]
    scope: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    code: Arc<Mutex<Option<String>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    snippet: Snippet,
    status: VideoStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snippet {
    title: String,
    description: String,
    tags: Vec<String>,
    category_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoStatus {
    privacy_status: String,
    self_declared_made_for_kids: bool,
}

#[derive(Deserialize)]
struct UploadResponse {
    id: String,
}

#[derive(Debug)]
struct UploadNotStarted(String);
impl std::fmt::Display for UploadNotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UploadNotStarted {}

fn now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn random_string(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

fn load_google_client() -> Result<GoogleClient> {
    let path = config::credential_file(CLIENT_FILE);
    let data = fs::read(&path).with_context(|| {
        format!(
            "Put client_secret.json in {}",
            config::directory().display()
        )
    })?;
    let file: GoogleClientFile = serde_json::from_slice(&data)?;

    file.installed
        .or(file.web)
        .context("OAuth JSON has no installed or web client")
}

fn save_token(token: &Token) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let path = config::credential_file(TOKEN_FILE);
    if let Some(parent) = path.parent() {
        config::ensure_directory(parent)?;
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec_pretty(token)?)?;
    Ok(())
}

async fn exchange_code(
    http: &Client,
    google: &GoogleClient,
    code: &str,
    verifier: &str,
    scope: &str,
) -> Result<Token> {
    let response = http
        .post(&google.token_uri)
        .form(&[
            ("code", code),
            ("client_id", google.client_id.as_str()),
            ("client_secret", google.client_secret.as_str()),
            ("redirect_uri", REDIRECT),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        bail!(
            "OAuth token exchange failed ({status}): {}",
            provider_error(&body)
        );
    }

    let data: TokenResponse = serde_json::from_str(&body)?;
    let token = Token {
        access_token: data.access_token,
        refresh_token: data.refresh_token,
        scope: scope.to_owned(),
        expires_at: now() + data.expires_in.unwrap_or(3600),
    };

    save_token(&token)?;
    Ok(token)
}

async fn refresh_token(http: &Client, google: &GoogleClient, old: &Token) -> Result<Token> {
    let refresh = old
        .refresh_token
        .as_deref()
        .context("No refresh token. Delete token.json and authenticate again.")?;

    let response = http
        .post(&google.token_uri)
        .form(&[
            ("client_id", google.client_id.as_str()),
            ("client_secret", google.client_secret.as_str()),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        bail!(
            "Token refresh failed ({status}): {}. Reauthorize using the token file in {}",
            provider_error(&body),
            config::credential_directory().display()
        );
    }

    let data: TokenResponse = serde_json::from_str(&body)?;
    let token = Token {
        access_token: data.access_token,
        refresh_token: data.refresh_token.or_else(|| old.refresh_token.clone()),
        scope: old.scope.clone(),
        expires_at: now() + data.expires_in.unwrap_or(3600),
    };

    save_token(&token)?;
    Ok(token)
}

async fn callback(
    State(state): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Html<String> {
    if let Some(error) = query.error {
        return Html(format!(
            "<h2>Google authorization failed</h2><p>{}</p>",
            error
        ));
    }

    if query.state.as_deref() != Some(state.expected_state.as_str()) {
        return Html("<h2>OAuth state mismatch. Try again.</h2>".into());
    }

    if let Some(code) = query.code {
        *state.code.lock().unwrap() = Some(code);
        Html("<h2>Authorization received!</h2><p>Return to Terminal.</p>".into())
    } else {
        Html("<h2>No authorization code received.</h2>".into())
    }
}

async fn authenticate(http: &Client, google: &GoogleClient) -> Result<Token> {
    authenticate_scope(http, google, false).await
}
async fn authenticate_scope(http: &Client, google: &GoogleClient, playlist: bool) -> Result<Token> {
    let scope = if playlist { PLAYLIST_SCOPE } else { SCOPE };
    if let Ok(bytes) = fs::read(config::credential_file(TOKEN_FILE)) {
        if let Ok(token) = serde_json::from_slice::<Token>(&bytes) {
            if (!playlist || token.scope.split_whitespace().any(|s| s == PLAYLIST_SCOPE))
                && token.expires_at > now() + 60
            {
                return Ok(token);
            }

            if (!playlist || token.scope.split_whitespace().any(|s| s == PLAYLIST_SCOPE))
                && token.refresh_token.is_some()
            {
                return refresh_token(http, google, &token).await;
            }
        }
    }

    let state_value = random_string(32);
    let verifier = random_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let mut auth_url = Url::parse(&google.auth_uri)?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &google.client_id)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("response_type", "code")
        .append_pair("scope", scope)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", &state_value)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    let code = Arc::new(Mutex::new(None));
    let app_state = CallbackState {
        expected_state: state_value,
        code: code.clone(),
    };

    let app = Router::new()
        .route("/callback", get(callback))
        .with_state(app_state);

    let listener = TcpListener::bind("127.0.0.1:8765")
        .await
        .context("Cannot bind OAuth callback port 8765")?;

    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    logging::info(
        "auth_start",
        "Opening Google authorization in your browser",
        serde_json::json!({}),
    );
    if let Err(error) = open::that(auth_url.as_str()) {
        logging::warn(
            "auth_browser_failed",
            &format!("Could not open browser: {error}. Open {auth_url}"),
            serde_json::json!({}),
        );
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);

    loop {
        let received = code.lock().unwrap().clone();

        if let Some(code) = received {
            server.abort();
            return exchange_code(http, google, &code, &verifier, scope).await;
        }

        if tokio::time::Instant::now() > deadline {
            server.abort();
            bail!("OAuth timed out. Run the command again.");
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn upload(
    http: &Client,
    token: &Token,
    path: &Path,
    title: &str,
    description: &str,
    privacy: &str,
) -> Result<String> {
    if !["private", "unlisted", "public"].contains(&privacy) {
        bail!("Privacy must be private, unlisted, or public");
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Cannot open {}", path.display()))?;
    let size = file.metadata().await?.len();

    if size == 0 {
        bail!("Video file is empty");
    }

    let metadata = Metadata {
        snippet: Snippet {
            title: title.to_string(),
            description: description.to_string(),
            tags: vec!["FRC".into(), "FIRST Robotics Competition".into()],
            category_id: "28".into(),
        },
        status: VideoStatus {
            privacy_status: privacy.to_string(),
            self_declared_made_for_kids: false,
        },
    };

    let init = http
        .post(format!(
            "{UPLOAD_URL}?uploadType=resumable&part=snippet,status"
        ))
        .bearer_auth(&token.access_token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Type", "video/mp4")
        .header("X-Upload-Content-Length", size.to_string())
        .json(&metadata)
        .send()
        .await?;

    let status = init.status();
    if !status.is_success() {
        return Err(UploadNotStarted(format!(
            "Could not start upload ({status}): {}. No video bytes were sent; the clip is retained for retry",
            provider_error(&init.text().await?)
        )).into());
    }

    let upload_url = init
        .headers()
        .get("Location")
        .context("YouTube did not return an upload URL")?
        .to_str()?
        .to_string();

    let mut offset = 0_u64;
    let mut last_step = 0;
    let upload_started = std::time::Instant::now();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    logging::info(
        "upload_start",
        &format!("{name} · uploading {:.1} MB", size as f64 / 1048576.0),
        serde_json::json!({"bytes":size}),
    );

    while offset < size {
        let remaining = size - offset;
        let count = remaining.min(CHUNK_SIZE as u64) as usize;
        let mut buffer = vec![0_u8; count];

        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.read_exact(&mut buffer).await?;

        let end = offset + count as u64 - 1;
        let range = format!("bytes {offset}-{end}/{size}");

        let response = http
            .put(&upload_url)
            .header("Content-Type", "video/mp4")
            .header("Content-Length", count.to_string())
            .header("Content-Range", range)
            .body(buffer)
            .send()
            .await?;

        let status = response.status();
        if status == StatusCode::PERMANENT_REDIRECT
            || status == StatusCode::OK
            || status == StatusCode::CREATED
        {
            let percent = (end + 1) * 100 / size;
            if percent / 25 > last_step || end + 1 == size {
                last_step = percent / 25;
                logging::info(
                    "upload_progress",
                    &format!(
                        "{name} · {percent}% ({:.1}/{:.1} MB)",
                        (end + 1) as f64 / 1048576.0,
                        size as f64 / 1048576.0
                    ),
                    serde_json::json!({"percent":percent,"bytesSent":end+1,"totalBytes":size}),
                );
            }
        }

        if status == StatusCode::PERMANENT_REDIRECT {
            offset += count as u64;
            continue;
        }

        if status == StatusCode::OK || status == StatusCode::CREATED {
            let result: UploadResponse = response.json().await?;
            logging::debug(
                "upload_timing",
                "Upload completed",
                serde_json::json!({"seconds":upload_started.elapsed().as_secs_f64()}),
            );
            return Ok(result.id);
        }

        bail!(
            "Upload failed ({status}): {}",
            provider_error(&response.text().await?)
        );
    }

    bail!("Upload ended without YouTube confirming completion")
}

fn provider_error(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let reason = value
        .get("error")
        .and_then(|e| {
            e.as_str()
                .or_else(|| e.pointer("/errors/0/reason").and_then(|v| v.as_str()))
                .or_else(|| e.get("status").and_then(|v| v.as_str()))
        })
        .unwrap_or("provider rejected the request");
    reason
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || " _-.".contains(*c))
        .take(100)
        .collect()
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_cli().await {
        logging::error(
            "command_failed",
            &format!("{error:#}"),
            serde_json::json!({}),
        );
        std::process::exit(1);
    }
}
async fn run_cli() -> Result<()> {
    let (args, settings, directory) = cli::global_options(std::env::args().skip(1).collect())?;
    logging::init(settings);
    let command = cli::parse(args)?;
    if !matches!(&command, cli::Command::Help | cli::Command::Version) {
        config::init(directory)?;
    }
    match command {
        cli::Command::Help => println!("{}", cli::HELP),
        cli::Command::Version => println!("frc-match-clipper {}", env!("CARGO_PKG_VERSION")),
        cli::Command::ConfigPath => println!("{}", config::directory().display()),
        cli::Command::Clip(args) => batch::run(args).await?,
        cli::Command::Upload {
            file,
            title,
            description,
            privacy,
        } => {
            let google = load_google_client()?;
            let http = Client::builder()
                .user_agent(concat!("frc-match-clipper/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(120))
                .build()?;
            let token = authenticate(&http, &google).await?;
            let id = upload(&http, &token, &file, &title, &description, &privacy).await?;
            logging::info(
                "upload_complete",
                &format!("Uploaded: https://www.youtube.com/watch?v={id}"),
                serde_json::json!({"videoId":id}),
            );
        }
    }
    Ok(())
}
#[cfg(test)]
mod provider_tests {
    #[test]
    fn errors_do_not_echo_provider_secrets() {
        assert_eq!(
            super::provider_error(r#"{"error":"invalid_grant","access_token":"do-not-print"}"#),
            "invalid_grant"
        );
        assert_eq!(
            super::provider_error(
                r#"{"error":{"errors":[{"reason":"quotaExceeded"}],"message":"sensitive"}}"#
            ),
            "quotaExceeded"
        );
    }
}
