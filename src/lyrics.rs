use anyhow::{Context, Result, anyhow};
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::FileExtManual;
use serde::{Deserialize, Serialize};

use crate::spotify_official;
use crate::spotify::TrackInfo;
use crate::storage::LyricsSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricsCandidate {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "trackName", default)]
    pub track_name: String,
    #[serde(rename = "artistName", default)]
    pub artist_name: String,
    #[serde(rename = "albumName", default)]
    pub album_name: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(rename = "plainLyrics", default)]
    pub plain_lyrics: String,
    #[serde(rename = "syncedLyrics", default)]
    pub synced_lyrics: String,
    #[serde(rename = "lyricsfile", default)]
    pub lyrics_file: Option<String>,
}

impl LyricsCandidate {
    pub fn has_synced(&self) -> bool {
        !self.synced_lyrics.trim().is_empty()
    }

    pub fn has_plain(&self) -> bool {
        !self.plain_lyrics.trim().is_empty()
    }

    pub fn display_title(&self) -> String {
        let mut parts = Vec::new();
        if !self.artist_name.is_empty() {
            parts.push(self.artist_name.clone());
        }
        if !self.track_name.is_empty() {
            parts.push(self.track_name.clone());
        }
        if !self.album_name.is_empty() {
            parts.push(self.album_name.clone());
        }
        if parts.is_empty() {
            parts.push(format!("id {}", self.id));
        }
        let mut label = parts.join(" - ");
        if self.has_synced() {
            label.push_str(" [synced]");
        } else if self.has_plain() {
            label.push_str(" [plain]");
        }
        label
    }
}

#[derive(Debug, Clone)]
pub struct ParsedLyrics {
    pub lines: Vec<LyricLine>,
    pub plain_lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LyricLine {
    pub time_ms: u64,
    pub text: String,
}

pub fn fetch_best_candidate(track: &TrackInfo) -> Result<LyricsCandidate> {
    let url = lyrics_url("/api/get", track);
    let file = gio::File::for_uri(&url);
    let (bytes, _etag) = file.load_contents(None::<&gio::Cancellable>)?;
    serde_json::from_slice::<LyricsCandidate>(&bytes)
        .context("failed to parse lrclib best response")
}

pub fn fetch_search_candidates(track: &TrackInfo) -> Result<Vec<LyricsCandidate>> {
    let url = lyrics_url("/api/search", track);
    let file = gio::File::for_uri(&url);
    let (bytes, _etag) = file.load_contents(None::<&gio::Cancellable>)?;
    let mut candidates = serde_json::from_slice::<Vec<LyricsCandidate>>(&bytes)
        .context("failed to parse lrclib search response")?;
    dedup_candidates(&mut candidates);
    if candidates.is_empty() {
        Err(anyhow!("no lyrics candidates found"))
    } else {
        Ok(candidates)
    }
}

pub fn fetch_candidate_for_source(
    track: &TrackInfo,
    source: LyricsSource,
    sp_dc: &str,
) -> Result<LyricsCandidate> {
    match source {
        LyricsSource::Lrclib => fetch_best_candidate(track),
        LyricsSource::SpotifyOfficial => spotify_official::fetch_candidate(track, sp_dc),
    }
}

pub fn parse_candidate(candidate: &LyricsCandidate) -> ParsedLyrics {
    let mut lines = Vec::new();

    for line in candidate.synced_lyrics.lines() {
        if let Some(parsed) = parse_lrc_line(line) {
            lines.push(parsed);
        }
    }

    lines.sort_by_key(|line| line.time_ms);

    let plain_lines = candidate
        .plain_lyrics
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    ParsedLyrics { lines, plain_lines }
}

fn lyrics_url(path: &str, track: &TrackInfo) -> String {
    let artist = glib::uri_escape_string(&track.artist, None::<&str>, true);
    let title = glib::uri_escape_string(&track.title, None::<&str>, true);
    format!("https://lrclib.net{path}?artist_name={artist}&track_name={title}")
}

fn dedup_candidates(candidates: &mut Vec<LyricsCandidate>) {
    let mut seen = std::collections::BTreeSet::new();
    candidates.retain(|candidate| seen.insert(candidate.id));
}

fn parse_lrc_line(line: &str) -> Option<LyricLine> {
    let line = line.trim();
    let (ts, text) = line.split_once(']')?;
    let ts = ts.strip_prefix('[')?;
    let (min, rest) = ts.split_once(':')?;
    let (sec, frac) = rest.split_once('.')?;

    let minutes = min.parse::<u64>().ok()?;
    let seconds = sec.parse::<u64>().ok()?;
    let millis = match frac.len() {
        0 => 0,
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse::<u64>().ok()?,
    };

    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    Some(LyricLine {
        time_ms: (minutes * 60 + seconds) * 1000 + millis,
        text: text.to_string(),
    })
}
