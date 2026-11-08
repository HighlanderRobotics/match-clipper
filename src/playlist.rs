//! Playlist checkpoints are separate from immutable clip plans.
use crate::{logging, provider_error, Token};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path};

#[derive(Serialize, Deserialize)]
struct State {
    title: String,
    #[serde(default = "default_privacy")]
    privacy: String,
    id: Option<String>,
}
fn default_privacy() -> String {
    "unlisted".into()
}
async fn save(path: &Path, state: &State) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(state)?).await?;
    tokio::fs::rename(tmp, path).await?;
    Ok(())
}
async fn response(r: reqwest::Response) -> Result<Value> {
    let status = r.status();
    let text = r.text().await?;
    if !status.is_success() {
        bail!(
            "YouTube playlist request failed ({status}): {}",
            provider_error(&text)
        );
    }
    Ok(serde_json::from_str(&text)?)
}
fn validate(state: &State, title: &str, privacy: &str) -> Result<()> {
    if state.title != title {
        bail!("Playlist title differs from playlist.json; use the original title");
    }
    if state.privacy != privacy {
        bail!("Playlist privacy differs from playlist.json; use the original privacy");
    }
    if state.id.is_none() {
        bail!("Uncertain playlist creation. Check YouTube Studio and set its ID in playlist.json before resuming; do not recreate blindly");
    }
    Ok(())
}
pub async fn sync(
    http: &Client,
    token: &Token,
    dir: &Path,
    title: &str,
    privacy: &str,
    videos: &[String],
) -> Result<()> {
    let path = dir.join("playlist.json");
    let state = if tokio::fs::try_exists(&path).await? {
        let s: State = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
        validate(&s, title, privacy)?;
        s
    } else {
        let mut s = State {
            title: title.into(),
            privacy: privacy.into(),
            id: None,
        };
        save(&path, &s).await?; // Never retry ambiguous creation automatically.
        let data = response(
            http.post("https://www.googleapis.com/youtube/v3/playlists")
                .query(&[("part", "snippet,status")])
                .bearer_auth(&token.access_token)
                .json(&json!({"snippet":{"title":title},"status":{"privacyStatus":privacy}}))
                .send()
                .await?,
        )
        .await?;
        s.id = Some(
            data["id"]
                .as_str()
                .context("Playlist response has no ID")?
                .into(),
        );
        save(&path, &s).await?;
        s
    };
    let id = state.id.as_deref().context("Missing playlist ID")?;
    if !videos.is_empty() {
        // Read actual membership, including after interrupted inserts; prevents duplicates.
        let mut existing = HashSet::new();
        let mut page = String::new();
        loop {
            let data = response(
                http.get("https://www.googleapis.com/youtube/v3/playlistItems")
                    .bearer_auth(&token.access_token)
                    .query(&[
                        ("part", "snippet"),
                        ("playlistId", id),
                        ("maxResults", "50"),
                        ("pageToken", &page),
                    ])
                    .send()
                    .await?,
            )
            .await?;
            let items = data["items"]
                .as_array()
                .context("Playlist response has no items")?;
            for item in items {
                if let Some(video) = item
                    .pointer("/snippet/resourceId/videoId")
                    .and_then(Value::as_str)
                {
                    existing.insert(video.to_owned());
                }
            }
            match data["nextPageToken"].as_str() {
                Some(next) => page = next.into(),
                None => break,
            }
        }
        for video in videos {
            if existing.insert(video.clone()) {
                response(http.post("https://www.googleapis.com/youtube/v3/playlistItems")
                    .bearer_auth(&token.access_token).query(&[("part","snippet")])
                    .json(&json!({"snippet":{"playlistId":id,"resourceId":{"kind":"youtube#video","videoId":video}}}))
                    .send().await?).await?;
            }
        }
    }
    logging::info(
        "playlist_ready",
        &format!("Playlist · https://www.youtube.com/playlist?list={id}"),
        json!({"playlistId":id}),
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_creation_cannot_duplicate_playlist() {
        let s = State {
            title: "Saturday".into(),
            privacy: "unlisted".into(),
            id: None,
        };
        assert!(validate(&s, "Saturday", "unlisted").is_err());
    }
    #[test]
    fn resumed_playlist_must_match_title() {
        let s = State {
            title: "Saturday".into(),
            privacy: "unlisted".into(),
            id: Some("PLexisting".into()),
        };
        assert!(validate(&s, "Saturday", "unlisted").is_ok());
        assert!(validate(&s, "Sunday", "unlisted").is_err());
        assert!(validate(&s, "Saturday", "public").is_err());
    }
    #[test]
    fn legacy_state_preserves_unlisted_privacy() {
        let s: State = serde_json::from_str(r#"{"title":"Saturday","id":"PLexisting"}"#).unwrap();
        assert!(validate(&s, "Saturday", "unlisted").is_ok());
        assert!(validate(&s, "Saturday", "public").is_err());
    }
}
