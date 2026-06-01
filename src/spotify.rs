use anyhow::{Context, Result, anyhow, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use std::process::Command;

static TRACK_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"string "mpris:trackid"\s+variant\s+string "([^"]*)""#).unwrap());
static URL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"string "xesam:url"\s+variant\s+string "([^"]*)""#).unwrap());
static ALBUM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"string "xesam:album"\s+variant\s+string "([^"]*)""#).unwrap());
static TITLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"string "xesam:title"\s+variant\s+string "([^"]*)""#).unwrap());
static ARTIST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"string "xesam:artist"\s+variant\s+array\s+\[\s+string "([^"]*)""#).unwrap()
});
static DURATION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"string "mpris:length"\s+variant\s+uint64\s+(\d+)"#).unwrap());
static POSITION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"int64\s+(\d+)"#).unwrap());
static STATUS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"variant\s+string "([^"]+)""#).unwrap());

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub track_id: Option<String>,
    pub spotify_url: Option<String>,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_ms: u64,
    pub position_ms: u64,
    pub playback_status: String,
}

impl TrackInfo {
    pub fn cache_key(&self) -> String {
        if let Some(track_id) = self.spotify_track_id() {
            return format!("spotify:{track_id}");
        }

        if let Some(track_id) = &self.track_id {
            return track_id.clone();
        }

        format!(
            "{}|{}|{}",
            normalize(&self.artist),
            normalize(&self.title),
            self.duration_ms / 1000
        )
    }

    pub fn spotify_track_id(&self) -> Option<String> {
        self.spotify_url
            .as_deref()
            .and_then(extract_spotify_track_id)
            .or_else(|| self.track_id.as_deref().and_then(extract_spotify_track_id))
    }
}

#[derive(Debug, Clone)]
pub struct SpotifySnapshot {
    pub track: Option<TrackInfo>,
}

pub fn read_snapshot() -> Result<SpotifySnapshot> {
    let metadata = dbus_send(&[
        "--session",
        "--dest=org.mpris.MediaPlayer2.spotify",
        "--type=method_call",
        "--print-reply",
        "/org/mpris/MediaPlayer2",
        "org.freedesktop.DBus.Properties.Get",
        "string:org.mpris.MediaPlayer2.Player",
        "string:Metadata",
    ])?;
    let playback_status = dbus_send(&[
        "--session",
        "--dest=org.mpris.MediaPlayer2.spotify",
        "--type=method_call",
        "--print-reply",
        "/org/mpris/MediaPlayer2",
        "org.freedesktop.DBus.Properties.Get",
        "string:org.mpris.MediaPlayer2.Player",
        "string:PlaybackStatus",
    ])?;
    let position = dbus_send(&[
        "--session",
        "--dest=org.mpris.MediaPlayer2.spotify",
        "--type=method_call",
        "--print-reply",
        "/org/mpris/MediaPlayer2",
        "org.freedesktop.DBus.Properties.Get",
        "string:org.mpris.MediaPlayer2.Player",
        "string:Position",
    ])?;

    let track_id = capture_string(&TRACK_ID_RE, &metadata);
    let spotify_url = capture_string(&URL_RE, &metadata);
    let artist = capture_string(&ARTIST_RE, &metadata).unwrap_or_default();
    let title = capture_string(&TITLE_RE, &metadata).unwrap_or_default();
    let album = capture_string(&ALBUM_RE, &metadata).unwrap_or_default();
    let duration_ms = capture_u64(&DURATION_RE, &metadata).unwrap_or_default() / 1000;
    let position_ms = capture_u64(&POSITION_RE, &position).unwrap_or_default() / 1000;
    let playback_status = capture_string(&STATUS_RE, &playback_status)
        .ok_or_else(|| anyhow!("failed to parse playback status"))?;

    if artist.is_empty() && title.is_empty() {
        return Ok(SpotifySnapshot { track: None });
    }

    Ok(SpotifySnapshot {
        track: Some(TrackInfo {
            track_id,
            spotify_url,
            artist,
            title,
            album,
            duration_ms,
            position_ms,
            playback_status,
        }),
    })
}

fn dbus_send(args: &[&str]) -> Result<String> {
    let output = Command::new("dbus-send")
        .args(args)
        .output()
        .context("failed to run dbus-send")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("dbus-send failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn capture_string(re: &Regex, text: &str) -> Option<String> {
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn capture_u64(re: &Regex, text: &str) -> Option<u64> {
    re.captures(text)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
}

fn extract_spotify_track_id(value: &str) -> Option<String> {
    for pattern in [
        r"spotify:track:([A-Za-z0-9]+)",
        r"https?://open\.spotify\.com/track/([A-Za-z0-9]+)",
        r".*/track/([A-Za-z0-9]+)$",
    ] {
        if let Some(captures) = Regex::new(pattern).ok()?.captures(value) {
            if let Some(m) = captures.get(1) {
                return Some(m.as_str().to_string());
            }
        }
    }
    None
}

fn normalize(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .replace('\u{3000}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
