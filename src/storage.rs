use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::lyrics::LyricsCandidate;
use crate::spotify::TrackInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricsSource {
    Lrclib,
    SpotifyOfficial,
}

impl Default for LyricsSource {
    fn default() -> Self {
        Self::Lrclib
    }
}

impl LyricsSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lrclib => "lrclib",
            Self::SpotifyOfficial => "spotify_official",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "spotify_official" => Self::SpotifyOfficial,
            _ => Self::Lrclib,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedLyrics {
    pub track_key: String,
    pub selected_candidate_id: Option<i64>,
    pub candidates: Vec<LyricsCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub topmost: bool,
    pub auto_line_count: bool,
    pub max_lines: i32,
    pub background_opacity: f64,
    pub timing_offset_ms: i32,
    pub char_follow: bool,
    pub lyrics_source: LyricsSource,
    pub sp_dc: String,
    pub window_width: i32,
    pub window_height: i32,
    pub window_x: i32,
    pub window_y: i32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            topmost: true,
            auto_line_count: true,
            max_lines: 18,
            background_opacity: 0.94,
            timing_offset_ms: 0,
            char_follow: false,
            lyrics_source: LyricsSource::Lrclib,
            sp_dc: String::new(),
            window_width: 720,
            window_height: 560,
            window_x: 80,
            window_y: 80,
        }
    }
}

pub struct LyricsStore {
    conn: Connection,
}

impl LyricsStore {
    pub fn open() -> Result<Self> {
        let path = data_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS lyrics_cache (
                track_key TEXT PRIMARY KEY,
                artist TEXT NOT NULL,
                title TEXT NOT NULL,
                album TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                selected_candidate_id INTEGER,
                candidates_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );

            CREATE TABLE IF NOT EXISTS app_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                topmost INTEGER NOT NULL,
                auto_line_count INTEGER NOT NULL,
                max_lines INTEGER NOT NULL,
                background_opacity REAL NOT NULL,
                timing_offset_ms INTEGER NOT NULL,
                char_follow INTEGER NOT NULL,
                lyrics_source TEXT NOT NULL DEFAULT 'lrclib',
                sp_dc TEXT NOT NULL DEFAULT '',
                window_width INTEGER NOT NULL,
                window_height INTEGER NOT NULL,
                window_x INTEGER NOT NULL,
                window_y INTEGER NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );
            "#,
        )?;

        migrate_settings_schema(&conn)?;

        Ok(Self { conn })
    }

    pub fn load_settings(&self) -> Result<AppSettings> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT topmost, auto_line_count, max_lines, background_opacity, timing_offset_ms, char_follow, lyrics_source, sp_dc, window_width, window_height, window_x, window_y
            FROM app_settings
            WHERE id = 1
            "#,
        )?;

        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let settings = AppSettings {
                topmost: row.get::<_, i64>(0)? != 0,
                auto_line_count: row.get::<_, i64>(1)? != 0,
                max_lines: row.get::<_, i64>(2)? as i32,
                background_opacity: row.get::<_, f64>(3)?,
                timing_offset_ms: row.get::<_, i64>(4)? as i32,
                char_follow: row.get::<_, i64>(5)? != 0,
                lyrics_source: LyricsSource::from_db(&row.get::<_, String>(6)?),
                sp_dc: row.get::<_, String>(7)?,
                window_width: row.get::<_, i64>(8)? as i32,
                window_height: row.get::<_, i64>(9)? as i32,
                window_x: row.get::<_, i64>(10)? as i32,
                window_y: row.get::<_, i64>(11)? as i32,
            };
            eprintln!(
                "load_settings: topmost={} auto_line_count={} max_lines={} opacity={:.2} offset={} char={} source={} geometry={}x{}+{}+{}",
                settings.topmost,
                settings.auto_line_count,
                settings.max_lines,
                settings.background_opacity,
                settings.timing_offset_ms,
                settings.char_follow,
                settings.lyrics_source.as_str(),
                settings.window_width,
                settings.window_height,
                settings.window_x,
                settings.window_y
            );
            Ok(settings)
        } else {
            eprintln!("load_settings: no saved row, using defaults");
            Ok(AppSettings::default())
        }
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        eprintln!(
            "save_settings: topmost={} auto_line_count={} max_lines={} opacity={:.2} offset={} char={} source={} geometry={}x{}+{}+{}",
            settings.topmost,
            settings.auto_line_count,
            settings.max_lines,
            settings.background_opacity,
            settings.timing_offset_ms,
            settings.char_follow,
            settings.lyrics_source.as_str(),
            settings.window_width,
            settings.window_height,
            settings.window_x,
            settings.window_y
        );
        self.conn.execute(
            r#"
            INSERT INTO app_settings (
                id, topmost, auto_line_count, max_lines, background_opacity, timing_offset_ms, char_follow, lyrics_source, sp_dc, window_width, window_height, window_x, window_y, updated_at
            )
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, strftime('%s', 'now'))
            ON CONFLICT(id) DO UPDATE SET
                topmost = excluded.topmost,
                auto_line_count = excluded.auto_line_count,
                max_lines = excluded.max_lines,
                background_opacity = excluded.background_opacity,
                timing_offset_ms = excluded.timing_offset_ms,
                char_follow = excluded.char_follow,
                lyrics_source = excluded.lyrics_source,
                sp_dc = excluded.sp_dc,
                window_width = excluded.window_width,
                window_height = excluded.window_height,
                window_x = excluded.window_x,
                window_y = excluded.window_y,
                updated_at = excluded.updated_at
            "#,
            params![
                settings.topmost as i64,
                settings.auto_line_count as i64,
                settings.max_lines as i64,
                settings.background_opacity,
                settings.timing_offset_ms as i64,
                settings.char_follow as i64,
                settings.lyrics_source.as_str(),
                settings.sp_dc,
                settings.window_width as i64,
                settings.window_height as i64,
                settings.window_x as i64,
                settings.window_y as i64,
            ],
        )?;
        Ok(())
    }

    pub fn load(&self, track_key: &str) -> Result<Option<CachedLyrics>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT selected_candidate_id, candidates_json
            FROM lyrics_cache
            WHERE track_key = ?1
            "#,
        )?;

        let mut rows = stmt.query(params![track_key])?;
        if let Some(row) = rows.next()? {
            let selected_candidate_id = row.get::<_, Option<i64>>(0)?;
            let candidates_json = row.get::<_, String>(1)?;
            let candidates = serde_json::from_str::<Vec<LyricsCandidate>>(&candidates_json)?;
            Ok(Some(CachedLyrics {
                track_key: track_key.to_string(),
                selected_candidate_id,
                candidates,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn save(
        &self,
        track_key: &str,
        track: &TrackInfo,
        selected_candidate_id: Option<i64>,
        candidates: &[LyricsCandidate],
    ) -> Result<()> {
        let candidates_json = serde_json::to_string(candidates)?;
        self.conn.execute(
            r#"
            INSERT INTO lyrics_cache (
                track_key, artist, title, album, duration_ms,
                selected_candidate_id, candidates_json, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%s', 'now'))
            ON CONFLICT(track_key) DO UPDATE SET
                artist = excluded.artist,
                title = excluded.title,
                album = excluded.album,
                duration_ms = excluded.duration_ms,
                selected_candidate_id = excluded.selected_candidate_id,
                candidates_json = excluded.candidates_json,
                updated_at = excluded.updated_at
            "#,
            params![
                track_key,
                track.artist,
                track.title,
                track.album,
                track.duration_ms as i64,
                selected_candidate_id,
                candidates_json
            ],
        )?;
        Ok(())
    }
}

fn migrate_settings_schema(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(app_settings)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
        if columns.is_empty() {
            return Ok(());
        }

    let required = [
        "auto_line_count",
        "max_lines",
        "lyrics_source",
        "sp_dc",
        "window_width",
        "window_height",
        "window_x",
        "window_y",
    ];
    for column in required {
        if !columns.iter().any(|existing| existing == column) {
            let sql = match column {
                "auto_line_count" => "ALTER TABLE app_settings ADD COLUMN auto_line_count INTEGER NOT NULL DEFAULT 1",
                "max_lines" => "ALTER TABLE app_settings ADD COLUMN max_lines INTEGER NOT NULL DEFAULT 18",
                "lyrics_source" => "ALTER TABLE app_settings ADD COLUMN lyrics_source TEXT NOT NULL DEFAULT 'lrclib'",
                "sp_dc" => "ALTER TABLE app_settings ADD COLUMN sp_dc TEXT NOT NULL DEFAULT ''",
                "window_width" => "ALTER TABLE app_settings ADD COLUMN window_width INTEGER NOT NULL DEFAULT 720",
                "window_height" => "ALTER TABLE app_settings ADD COLUMN window_height INTEGER NOT NULL DEFAULT 560",
                "window_x" => "ALTER TABLE app_settings ADD COLUMN window_x INTEGER NOT NULL DEFAULT 80",
                "window_y" => "ALTER TABLE app_settings ADD COLUMN window_y INTEGER NOT NULL DEFAULT 80",
                _ => unreachable!(),
            };
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

fn data_path() -> PathBuf {
    if let Some(dir) = dirs::data_local_dir() {
        return dir.join("spotify_lyrics").join("lyrics.sqlite3");
    }
    PathBuf::from("lyrics.sqlite3")
}
