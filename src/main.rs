
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
use tokio::{io::{AsyncReadExt, AsyncSeekExt}, net::TcpListener};
use url::Url;

const CLIENT_FILE: &str = "client_secret.json";
const TOKEN_FILE: &str = "token.json";
const REDIRECT: &str = "http://127.0.0.1:8765/callback";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
const SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
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
    let data = fs::read(CLIENT_FILE)
        .context("Put client_secret.json in the project root")?;
    let file: GoogleClientFile = serde_json::from_slice(&data)?;

    file.installed
        .or(file.web)
        .context("OAuth JSON has no installed or web client")
}

async fn exchange_code(
    http: &Client,
    google: &GoogleClient,
    code: &str,
    verifier: &str,
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
        bail!("OAuth token exchange failed ({status}): {body}");
    }

    let data: TokenResponse = serde_json::from_str(&body)?;
    let token = Token {
        access_token: data.access_token,
        refresh_token: data.refresh_token,
        expires_at: now() + data.expires_in.unwrap_or(3600),
    };

    fs::write(TOKEN_FILE, serde_json::to_vec_pretty(&token)?)?;
    Ok(token)
}

async fn refresh_token(
    http: &Client,
    google: &GoogleClient,
    old: &Token,
) -> Result<Token> {
    let refresh = old.refresh_token.as_deref()
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
        bail!("Token refresh failed ({status}): {body}");
    }

    let data: TokenResponse = serde_json::from_str(&body)?;
    let token = Token {
        access_token: data.access_token,
        refresh_token: data.refresh_token.or_else(|| old.refresh_token.clone()),
        expires_at: now() + data.expires_in.unwrap_or(3600),
    };

    fs::write(TOKEN_FILE, serde_json::to_vec_pretty(&token)?)?;
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
    if let Ok(bytes) = fs::read(TOKEN_FILE) {
        if let Ok(token) = serde_json::from_slice::<Token>(&bytes) {
            if token.expires_at > now() + 60 {
                return Ok(token);
            }

            if token.refresh_token.is_some() {
                return refresh_token(http, google, &token).await;
            }
        }
    }

    let state_value = random_string(32);
    let verifier = random_string(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let mut auth_url = Url::parse(&google.auth_uri)?;
    auth_url.query_pairs_mut()
        .append_pair("client_id", &google.client_id)
        .append_pair("redirect_uri", REDIRECT)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPE)
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

    let listener = TcpListener::bind("127.0.0.1:8765").await
        .context("Cannot bind OAuth callback port 8765")?;

    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    println!("Opening Google authorization in your browser...");
    if let Err(error) = open::that(auth_url.as_str()) {
        eprintln!("Could not open browser automatically: {error}");
        println!("Open this URL manually:\n{auth_url}");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);

    loop {
        let received = code.lock().unwrap().clone();

        if let Some(code) = received {
            server.abort();
            return exchange_code(http, google, &code, &verifier).await;
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

    let mut file = tokio::fs::File::open(path).await
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
        .post(format!("{UPLOAD_URL}?uploadType=resumable&part=snippet,status"))
        .bearer_auth(&token.access_token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Type", "video/mp4")
        .header("X-Upload-Content-Length", size.to_string())
        .json(&metadata)
        .send()
        .await?;

    let status = init.status();
    if !status.is_success() {
        bail!("Could not start upload ({status}): {}", init.text().await?);
    }

    let upload_url = init.headers()
        .get("Location")
        .context("YouTube did not return an upload URL")?
        .to_str()?.to_string();

    let mut offset = 0_u64;

    while offset < size {
        let remaining = size - offset;
        let count = remaining.min(CHUNK_SIZE as u64) as usize;
        let mut buffer = vec![0_u8; count];

        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.read_exact(&mut buffer).await?;

        let end = offset + count as u64 - 1;
        let range = format!("bytes {offset}-{end}/{size}");

        println!(
            "Uploading {:.1}%...",
            (end + 1) as f64 / size as f64 * 100.0
        );

        let response = http
            .put(&upload_url)
            .header("Content-Type", "video/mp4")
            .header("Content-Length", count.to_string())
            .header("Content-Range", range)
            .body(buffer)
            .send()
            .await?;

        let status = response.status();

        if status == StatusCode::PERMANENT_REDIRECT {
            offset += count as u64;
            continue;
        }

        if status == StatusCode::OK || status == StatusCode::CREATED {
            let result: UploadResponse = response.json().await?;
            return Ok(result.id);
        }

        bail!("Upload failed ({status}): {}", response.text().await?);
    }

    bail!("Upload ended without YouTube confirming completion")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!(
            "FRC Match Clipper\n\n\
             Usage:\n\
             cargo run -- upload FILE --title TITLE \\\n\
               --description DESCRIPTION --privacy unlisted\n"
        );
        return Ok(());
    }

    if args[0] != "upload" || args.len() < 2 {
        bail!("Expected: upload FILE --title TITLE");
    }

    let path = Path::new(&args[1]);
    let value = |flag: &str, default: Option<&str>| -> Result<String> {
        match args.iter().position(|a| a == flag) {
            Some(i) => args.get(i + 1).cloned()
                .with_context(|| format!("Missing value after {flag}")),
            None => default.map(str::to_string)
                .with_context(|| format!("Missing {flag}")),
        }
    };

    let title = value("--title", None)?;
    let description = value("--description", Some("FRC match footage"))?;
    let privacy = value("--privacy", Some("unlisted"))?;

    let google = load_google_client()?;
    let http = Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()?;

    let token = authenticate(&http, &google).await?;

    let id = upload(
        &http, &token, path, &title, &description, &privacy
    ).await?;

    println!("Upload complete: https://www.youtube.com/watch?v={id}");
    Ok(())
}
