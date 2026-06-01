use anyhow::{anyhow, Context, Result};
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use reqwest::cookie::Jar;
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::Url;
use serde::Deserialize;
use sha1::Sha1;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::sync::Arc;

use crate::lyrics::LyricsCandidate;
use crate::spotify::TrackInfo;

const SPOTIFY_HOME: &str = "https://open.spotify.com";
const TOKEN_URL: &str = "https://open.spotify.com/api/token";
const LYRICS_URL: &str = "https://spclient.wg.spotify.com/color-lyrics/v2/track/{track_id}";
const SECRET_CIPHER_DICT_URL: &str =
    "https://code.thetadev.de/ThetaDev/spotify-secrets/raw/branch/main/secrets/secretDict.json";
const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpotifyLyricsResponse {
    lyrics: Option<SpotifyLyricsBlock>,
}

#[derive(Debug, Deserialize)]
struct SpotifyLyricsBlock {
    #[serde(rename = "syncType", default)]
    _sync_type: Option<String>,
    #[serde(default)]
    lines: Vec<SpotifyLyricsLine>,
}

#[derive(Debug, Deserialize)]
struct SpotifyLyricsLine {
    #[serde(rename = "startTimeMs", default)]
    start_time_ms: Option<String>,
    #[serde(rename = "endTimeMs", default)]
    _end_time_ms: Option<String>,
    #[serde(default)]
    words: String,
}

struct Totp {
    secret: Vec<u8>,
    version: String,
    period: u64,
    digits: usize,
}

impl Totp {
    fn new() -> Result<Self> {
        let resp = reqwest::blocking::get(SECRET_CIPHER_DICT_URL)
            .context("failed to fetch Spotify TOTP secret dictionary")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "failed to fetch Spotify TOTP secret dictionary: HTTP {}",
                resp.status()
            ));
        }

        let data: serde_json::Value = resp
            .json()
            .context("failed to parse Spotify TOTP secret dictionary")?;
        let object = data
            .as_object()
            .ok_or_else(|| anyhow!("Spotify TOTP secret dictionary is not an object"))?;
        let Some((version, ascii_codes)) = object.iter().last() else {
            return Err(anyhow!("Spotify TOTP secret dictionary is empty"));
        };
        let codes = ascii_codes
            .as_array()
            .ok_or_else(|| anyhow!("Spotify TOTP secret dictionary entry is not an array"))?;

        let transformed = codes
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let raw = value
                    .as_i64()
                    .ok_or_else(|| anyhow!("Spotify TOTP secret dictionary entry is not an integer"))?;
                let xored = raw ^ ((index % 33) as i64 + 9);
                Ok(char::from(
                    u8::try_from(xored)
                        .map_err(|_| anyhow!("Spotify TOTP secret dictionary entry out of range"))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let secret_key = transformed
            .iter()
            .map(|ch| (*ch as u32).to_string())
            .collect::<String>();

        Ok(Self {
            secret: secret_key.into_bytes(),
            version: version.clone(),
            period: 30,
            digits: 6,
        })
    }

    fn generate(&self, timestamp_ms: u64) -> Result<String> {
        let counter = timestamp_ms / 1000 / self.period;
        let mut mac = HmacSha1::new_from_slice(&self.secret)
            .context("failed to initialize Spotify TOTP HMAC")?;
        mac.update(&counter.to_be_bytes());
        let hmac_result = mac.finalize().into_bytes();
        let offset = (hmac_result[19] & 0x0f) as usize;
        let binary = ((u32::from(hmac_result[offset] & 0x7f)) << 24)
            | ((u32::from(hmac_result[offset + 1] & 0xff)) << 16)
            | ((u32::from(hmac_result[offset + 2] & 0xff)) << 8)
            | u32::from(hmac_result[offset + 3] & 0xff);
        Ok(format!("{:0width$}", binary % 10u32.pow(self.digits as u32), width = self.digits))
    }
}

pub fn fetch_candidate(track: &TrackInfo, sp_dc: &str) -> Result<LyricsCandidate> {
    let track_id = track
        .spotify_track_id()
        .ok_or_else(|| anyhow!("missing Spotify track id"))?;
    let session = build_session(sp_dc)?;
    prime_session(&session)?;
    let access_token = fetch_access_token(&session)?;
    let response = fetch_lyrics_response(&session.client, &track_id, &access_token)?;
    response_to_candidate(track, response)
}

struct SpotifySession {
    client: Client,
    _jar: Arc<Jar>,
}

fn build_session(sp_dc: &str) -> Result<SpotifySession> {
    let sp_dc = sp_dc.trim();
    if sp_dc.is_empty() {
        return Err(anyhow!("missing Spotify sp_dc cookie"));
    }
    let jar = Arc::new(Jar::default());
    let home = Url::parse(SPOTIFY_HOME).context("failed to parse Spotify home URL")?;
    jar.add_cookie_str(&format!("sp_dc={sp_dc}"), &home);

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .cookie_provider(Arc::clone(&jar))
        .build()
        .context("failed to build Spotify client")?;

    Ok(SpotifySession { client, _jar: jar })
}

fn prime_session(session: &SpotifySession) -> Result<()> {
    let resp = session
        .client
        .get(SPOTIFY_HOME)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, USER_AGENT_VALUE)
        .send()
        .context("failed to request Spotify home page")?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Spotify home page request failed: HTTP {}",
            resp.status()
        ));
    }

    Ok(())
}

fn fetch_access_token(session: &SpotifySession) -> Result<String> {
    let totp = Totp::new()?;
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_millis() as u64;
    let code = totp.generate(timestamp_ms)?;

    let resp = session
        .client
        .get(TOKEN_URL)
        .query(&[
            ("reason", "init"),
            ("productType", "web-player"),
            ("totp", code.as_str()),
            ("totpVer", totp.version.as_str()),
            ("totpServer", code.as_str()),
        ])
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, USER_AGENT_VALUE)
        .send()
        .context("failed to request Spotify access token")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(anyhow!(
            "Spotify access token request failed: HTTP {}{}",
            status,
            if body.is_empty() {
                String::new()
            } else {
                format!(" body={body}")
            }
        ));
    }

    let payload: AccessTokenResponse = resp
        .json()
        .context("failed to parse Spotify access token response")?;
    payload
        .access_token
        .ok_or_else(|| anyhow!("Spotify access token response did not contain an access token"))
}

fn fetch_lyrics_response(
    client: &Client,
    track_id: &str,
    access_token: &str,
) -> Result<SpotifyLyricsResponse> {
    let url = LYRICS_URL.replace("{track_id}", track_id);
    let app_platform = HeaderName::from_static("app-platform");
    let resp = client
        .get(url)
        .query(&[("format", "json"), ("market", "from_token")])
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header(app_platform, HeaderValue::from_static("WebPlayer"))
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, USER_AGENT_VALUE)
        .send()
        .context("failed to request Spotify lyrics")?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Spotify lyrics request failed: HTTP {}",
            resp.status()
        ));
    }

    resp.json()
        .context("failed to parse Spotify lyrics response")
}

fn response_to_candidate(track: &TrackInfo, response: SpotifyLyricsResponse) -> Result<LyricsCandidate> {
    let Some(lyrics) = response.lyrics else {
        return Err(anyhow!("no Spotify lyrics available"));
    };

    let has_timed_lines = lyrics
        .lines
        .iter()
        .any(|line| line.start_time_ms.as_deref().is_some());

    let plain_lines = lyrics
        .lines
        .iter()
        .map(|line| line.words.trim())
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let synced_lyrics = if has_timed_lines {
        lyrics
            .lines
            .iter()
            .filter_map(|line| {
                let time_ms = line.start_time_ms.as_deref()?.parse::<u64>().ok()?;
                let text = line.words.trim();
                if text.is_empty() {
                    return None;
                }
                Some(format!("[{}]{}", format_timestamp(time_ms), text))
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };

    if plain_lines.is_empty() && synced_lyrics.is_empty() {
        return Err(anyhow!("Spotify lyrics response did not contain any lyrics"));
    }

    Ok(LyricsCandidate {
        id: 0,
        name: "Spotify official".to_string(),
        track_name: track.title.clone(),
        artist_name: track.artist.clone(),
        album_name: track.album.clone(),
        duration: track.duration_ms as f64 / 1000.0,
        instrumental: plain_lines.is_empty() && synced_lyrics.is_empty(),
        plain_lyrics: plain_lines.join("\n"),
        synced_lyrics,
        lyrics_file: None,
    })
}

fn format_timestamp(time_ms: u64) -> String {
    let minutes = time_ms / 60_000;
    let seconds = (time_ms % 60_000) / 1_000;
    let millis = time_ms % 1_000;
    format!("{minutes:02}:{seconds:02}.{millis:03}")
}
