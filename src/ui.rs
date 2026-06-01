use anyhow::Result;
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk4 as gtk;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask};
use x11rb::wrapper::ConnectionExt as X11WrapperConnectionExt;
use std::cell::RefCell;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::lyrics::{
    fetch_candidate_for_source, fetch_search_candidates, parse_candidate, LyricsCandidate,
    ParsedLyrics,
};
use crate::spotify::{read_snapshot, SpotifySnapshot, TrackInfo};
use crate::storage::{AppSettings, CachedLyrics, LyricsSource, LyricsStore};

enum WorkerMsg {
    BestFetched {
        fetch_generation: u64,
        track_key: String,
        track: TrackInfo,
        candidate: LyricsCandidate,
    },
    SearchFetched {
        fetch_generation: u64,
        track_key: String,
        candidates: Vec<LyricsCandidate>,
    },
    Failed {
        fetch_generation: u64,
        track_key: String,
        source: &'static str,
        error: String,
    },
}

#[derive(Clone)]
struct AppConfig {
    topmost: bool,
    auto_line_count: bool,
    max_lines: i32,
    background_opacity: f64,
    timing_offset_ms: i32,
    char_follow: bool,
    lyrics_source: LyricsSource,
    sp_dc: String,
    window_width: i32,
    window_height: i32,
    window_x: i32,
    window_y: i32,
}

impl From<AppSettings> for AppConfig {
    fn from(value: AppSettings) -> Self {
        Self {
            topmost: value.topmost,
            auto_line_count: value.auto_line_count,
            max_lines: value.max_lines,
            background_opacity: value.background_opacity,
            timing_offset_ms: value.timing_offset_ms,
            char_follow: value.char_follow,
            lyrics_source: value.lyrics_source,
            sp_dc: value.sp_dc,
            window_width: value.window_width,
            window_height: value.window_height,
            window_x: value.window_x,
            window_y: value.window_y,
        }
    }
}

impl From<AppConfig> for AppSettings {
    fn from(value: AppConfig) -> Self {
        Self {
            topmost: value.topmost,
            auto_line_count: value.auto_line_count,
            max_lines: value.max_lines,
            background_opacity: value.background_opacity,
            timing_offset_ms: value.timing_offset_ms,
            char_follow: value.char_follow,
            lyrics_source: value.lyrics_source,
            sp_dc: value.sp_dc,
            window_width: value.window_width,
            window_height: value.window_height,
            window_x: value.window_x,
            window_y: value.window_y,
        }
    }
}

impl Default for AppConfig {
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

struct RuntimeState {
    current_track_key: Option<String>,
    current_track: Option<TrackInfo>,
    current_candidates: Vec<LyricsCandidate>,
    selected_candidate_id: Option<i64>,
    selected_lyrics: Option<ParsedLyrics>,
    current_line_index: usize,
    loading_best: Option<String>,
    loading_search: Option<String>,
    user_scrolled: bool,
    fetch_generation: u64,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            current_track_key: None,
            current_track: None,
            current_candidates: Vec::new(),
            selected_candidate_id: None,
            selected_lyrics: None,
            current_line_index: 0,
            loading_best: None,
            loading_search: None,
            user_scrolled: false,
            fetch_generation: 0,
        }
    }
}

pub struct AppController {
    window: gtk::ApplicationWindow,
    track_label: gtk::Label,
    lyrics_area: gtk::ScrolledWindow,
    lyrics_box: gtk::Box,
    lyrics_line_widgets: RefCell<Vec<gtk::Label>>,
    lyrics_vadjustment: gtk::Adjustment,
    _menu_button: gtk::MenuButton,
    menu_popover: gtk::Popover,
    change_button: gtk::Button,
    settings_button: gtk::Button,
    change_popover: gtk::Popover,
    candidate_list: gtk::ListBox,
    settings_popover: gtk::Popover,
    settings_close_button: gtk::Button,
    topmost_switch: gtk::Switch,
    opacity_spin: gtk::SpinButton,
    timing_offset_spin: gtk::SpinButton,
    char_follow_switch: gtk::Switch,
    lyrics_source_combo: gtk::ComboBoxText,
    sp_dc_entry: gtk::Entry,
    debug_track_label: gtk::Label,
    debug_playback_label: gtk::Label,
    debug_state_label: gtk::Label,
    debug_candidate_label: gtk::Label,
    debug_timing_label: gtk::Label,
    sync_button: gtk::Button,
    app_hold: RefCell<Option<gtk::gio::ApplicationHoldGuard>>,
    store: LyricsStore,
    tx: Sender<WorkerMsg>,
    rx: RefCell<Receiver<WorkerMsg>>,
    config: RefCell<AppConfig>,
    state: RefCell<RuntimeState>,
    restore_applied: Cell<bool>,
    restore_attempts: Cell<u8>,
    suppress_scroll_signal: Cell<bool>,
    last_window_geometry: RefCell<Option<(i32, i32, i32, i32)>>,
    suppress_setting_updates: Cell<bool>,
    settings_dirty: Cell<bool>,
}

impl AppController {
    pub fn new(app: &gtk::Application) -> Result<Rc<Self>> {
        let store = LyricsStore::open()?;
        let (tx, rx) = mpsc::channel();

        let initial_config = AppConfig::from(store.load_settings()?);
        let config = RefCell::new(initial_config.clone());

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Spotify Lyrics")
            .default_width(initial_config.window_width)
            .default_height(initial_config.window_height)
            .build();

        let header = gtk::HeaderBar::builder()
            .title_widget(&gtk::Label::new(Some("Spotify Lyrics")))
            .show_title_buttons(true)
            .build();
        window.set_titlebar(Some(&header));
        window.set_size_request(320, 240);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .build();
        header.pack_end(&menu_button);

        let menu_popover = gtk::Popover::new();
        menu_popover.set_has_arrow(true);
        let menu_root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        menu_root.set_margin_top(12);
        menu_root.set_margin_bottom(12);
        menu_root.set_margin_start(12);
        menu_root.set_margin_end(12);
        let change_button = gtk::Button::with_label("Change Lyrics");
        let settings_button = gtk::Button::with_label("Settings");
        menu_root.append(&change_button);
        menu_root.append(&settings_button);
        menu_popover.set_child(Some(&menu_root));
        menu_button.set_popover(Some(&menu_popover));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        window.set_child(Some(&root));

        let track_label = gtk::Label::new(Some("No music playing"));
        track_label.set_wrap(true);
        track_label.set_xalign(0.0);
        track_label.set_markup("<b>No music playing</b>");

        root.append(&track_label);

        let lyrics_area = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .build();
        let lyrics_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        lyrics_box.set_hexpand(true);
        lyrics_box.set_vexpand(true);
        lyrics_area.set_child(Some(&lyrics_box));
        let lyrics_overlay = gtk::Overlay::new();
        lyrics_overlay.set_child(Some(&lyrics_area));

        let sync_button = gtk::Button::with_label("Sync");
        sync_button.set_halign(gtk::Align::Center);
        sync_button.set_valign(gtk::Align::End);
        sync_button.set_margin_bottom(12);
        sync_button.set_visible(false);
        sync_button.add_css_class("suggested-action");
        lyrics_overlay.add_overlay(&sync_button);
        root.append(&lyrics_overlay);

        let lyrics_vadjustment = lyrics_area.vadjustment();
        let scroll_page_size = lyrics_vadjustment.page_size();
        let _ = scroll_page_size;

        let change_popover = gtk::Popover::new();
        change_popover.set_has_arrow(true);
        let change_root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        change_root.set_margin_top(12);
        change_root.set_margin_bottom(12);
        change_root.set_margin_start(12);
        change_root.set_margin_end(12);
        let change_title = gtk::Label::new(Some("Pick a lyrics candidate"));
        change_title.set_xalign(0.0);
        let change_hint = gtk::Label::new(Some(
            "This fetches search candidates only when you ask for them.",
        ));
        change_hint.set_xalign(0.0);
        change_hint.add_css_class("dim-label");
        let candidate_list = gtk::ListBox::new();
        candidate_list.set_selection_mode(gtk::SelectionMode::None);
        let candidate_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .min_content_width(320)
            .min_content_height(220)
            .build();
        candidate_scroll.set_child(Some(&candidate_list));
        change_root.append(&change_title);
        change_root.append(&change_hint);
        change_root.append(&candidate_scroll);
        change_popover.set_child(Some(&change_root));
        change_popover.set_parent(&window);

        let settings_popover = gtk::Popover::new();
        settings_popover.set_has_arrow(true);
        settings_popover.set_autohide(true);
        let settings_root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        settings_root.set_margin_top(12);
        settings_root.set_margin_bottom(12);
        settings_root.set_margin_start(12);
        settings_root.set_margin_end(12);

        let topmost_switch = gtk::Switch::builder().active(true).build();
        let opacity_adjustment = gtk::Adjustment::new(0.94, 0.2, 1.0, 0.01, 0.1, 0.0);
        let opacity_spin = gtk::SpinButton::new(Some(&opacity_adjustment), 0.01, 2);
        opacity_spin.set_value(0.94);
        let timing_adjustment = gtk::Adjustment::new(0.0, -5000.0, 5000.0, 50.0, 250.0, 0.0);
        let timing_offset_spin = gtk::SpinButton::new(Some(&timing_adjustment), 1.0, 0);
        timing_offset_spin.set_value(0.0);
        let char_follow_switch = gtk::Switch::builder().active(false).build();
        let lyrics_source_combo = gtk::ComboBoxText::new();
        lyrics_source_combo.append(Some(LyricsSource::Lrclib.as_str()), "LRCLIB");
        lyrics_source_combo.append(
            Some(LyricsSource::SpotifyOfficial.as_str()),
            "Spotify official",
        );
        let sp_dc_entry = gtk::Entry::new();
        sp_dc_entry.set_visibility(false);
        sp_dc_entry.set_input_purpose(gtk::InputPurpose::Password);
        sp_dc_entry.set_placeholder_text(Some("sp_dc cookie"));
        sp_dc_entry.set_width_chars(36);

        settings_root.append(&labeled_row("Always on top", &topmost_switch));
        settings_root.append(&labeled_row("Background opacity", &opacity_spin));
        settings_root.append(&labeled_row("Timing offset (ms)", &timing_offset_spin));
        settings_root.append(&labeled_row("Character follow", &char_follow_switch));
        settings_root.append(&labeled_row("Lyrics source", &lyrics_source_combo));
        settings_root.append(&labeled_row("Spotify sp_dc", &sp_dc_entry));
        let sp_dc_note = gtk::Label::new(Some(
            "Used only for Spotify official lyrics. Paste the browser cookie value here.",
        ));
        sp_dc_note.set_wrap(true);
        sp_dc_note.set_xalign(0.0);
        sp_dc_note.add_css_class("dim-label");
        settings_root.append(&sp_dc_note);

        let debug_title = gtk::Label::new(Some("Debug"));
        debug_title.set_xalign(0.0);
        settings_root.append(&debug_title);

        let debug_track_label = gtk::Label::new(Some("Track: -"));
        debug_track_label.set_wrap(true);
        debug_track_label.set_xalign(0.0);
        let debug_playback_label = gtk::Label::new(Some("Playback: -"));
        debug_playback_label.set_wrap(true);
        debug_playback_label.set_xalign(0.0);
        let debug_state_label = gtk::Label::new(Some("State: -"));
        debug_state_label.set_wrap(true);
        debug_state_label.set_xalign(0.0);
        let debug_candidate_label = gtk::Label::new(Some("Candidate: -"));
        debug_candidate_label.set_wrap(true);
        debug_candidate_label.set_xalign(0.0);
        let debug_timing_label = gtk::Label::new(Some("Timing: line-sync only"));
        debug_timing_label.set_wrap(true);
        debug_timing_label.set_xalign(0.0);
        settings_root.append(&debug_track_label);
        settings_root.append(&debug_playback_label);
        settings_root.append(&debug_state_label);
        settings_root.append(&debug_candidate_label);
        settings_root.append(&debug_timing_label);

        let char_note = gtk::Label::new(Some(
            "Character-level karaoke timing is heuristic and line-based.",
        ));
        char_note.set_wrap(true);
        char_note.set_xalign(0.0);
        char_note.add_css_class("dim-label");
        settings_root.append(&char_note);

        let settings_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        settings_actions.set_halign(gtk::Align::End);
        let settings_close_button = gtk::Button::with_label("Close");
        settings_actions.append(&settings_close_button);
        settings_root.append(&settings_actions);

        settings_popover.set_child(Some(&settings_root));
        settings_popover.set_parent(&window);

        let controller = Rc::new(Self {
            window,
            track_label,
            lyrics_area,
            lyrics_box,
            lyrics_line_widgets: RefCell::new(Vec::new()),
            lyrics_vadjustment,
            _menu_button: menu_button,
            menu_popover,
            change_button,
            settings_button,
            change_popover,
            candidate_list,
            settings_popover,
            settings_close_button,
            topmost_switch,
            opacity_spin,
            timing_offset_spin,
            char_follow_switch,
            lyrics_source_combo,
            sp_dc_entry,
            debug_track_label,
            debug_playback_label,
            debug_state_label,
            debug_candidate_label,
            debug_timing_label,
            sync_button,
            app_hold: RefCell::new(None),
            store,
            tx,
            rx: RefCell::new(rx),
            config,
            state: RefCell::new(RuntimeState::new()),
            restore_applied: Cell::new(false),
            restore_attempts: Cell::new(0),
            suppress_scroll_signal: Cell::new(false),
            last_window_geometry: RefCell::new(None),
            suppress_setting_updates: Cell::new(false),
            settings_dirty: Cell::new(false),
        });

        controller.install_handlers();
        controller.install_timers();
        controller.apply_visual_settings();
        controller.sync_settings_ui();
        controller.refresh_debug();

        Ok(controller)
    }

    pub fn show(self: &Rc<Self>) {
        if let Some(app) = self.window.application() {
            *self.app_hold.borrow_mut() = Some(app.hold());
        }
        let cfg = self.config.borrow().clone();
        eprintln!(
            "show: applying initial geometry {}x{}+{}+{}",
            cfg.window_width, cfg.window_height, cfg.window_x, cfg.window_y
        );
        self.window.unmaximize();
        self.window.unfullscreen();
        self.window.set_default_size(cfg.window_width, cfg.window_height);
        self.window.present();
        self.schedule_topmost_refresh();
        self.apply_visual_settings();
    }

    fn install_handlers(self: &Rc<Self>) {
        let this = Rc::clone(self);
        self.topmost_switch.connect_state_set(move |_, active| {
            if this.suppress_setting_updates.get() {
                return glib::Propagation::Proceed;
            }
            this.config.borrow_mut().topmost = active;
            this.persist_settings();
            this.schedule_topmost_refresh();
            this.refresh_debug();
            glib::Propagation::Proceed
        });

        let this = Rc::clone(self);
        self.opacity_spin.connect_value_changed(move |spin| {
            if this.suppress_setting_updates.get() {
                return;
            }
            this.config.borrow_mut().background_opacity = spin.value().clamp(0.2, 1.0);
            this.apply_visual_settings();
            this.persist_settings();
            this.refresh_debug();
        });

        let this = Rc::clone(self);
        self.timing_offset_spin.connect_value_changed(move |spin| {
            if this.suppress_setting_updates.get() {
                return;
            }
            this.config.borrow_mut().timing_offset_ms = spin.value() as i32;
            this.persist_settings();
            this.render_lyrics();
            this.refresh_debug();
        });

        let this = Rc::clone(self);
        self.char_follow_switch.connect_state_set(move |_, active| {
            if this.suppress_setting_updates.get() {
                return glib::Propagation::Proceed;
            }
            this.config.borrow_mut().char_follow = active;
            this.persist_settings();
            this.render_lyrics();
            this.refresh_debug();
            glib::Propagation::Proceed
        });

        let this = Rc::clone(self);
        self.lyrics_source_combo.connect_changed(move |combo| {
            if this.suppress_setting_updates.get() {
                return;
            }
            let Some(active_id) = combo.active_id() else {
                return;
            };
            let source = match active_id.as_str() {
                "spotify_official" => LyricsSource::SpotifyOfficial,
                _ => LyricsSource::Lrclib,
            };
            {
                let mut config = this.config.borrow_mut();
                config.lyrics_source = source;
            }
            this.persist_settings();
            this.settings_dirty.set(true);
            this.refresh_debug();
        });

        let this = Rc::clone(self);
        self.sp_dc_entry.connect_changed(move |entry| {
            if this.suppress_setting_updates.get() {
                return;
            }
            let value = entry.text().trim().to_string();
            {
                let mut config = this.config.borrow_mut();
                config.sp_dc = value;
            }
            this.persist_settings();
            if this.config.borrow().lyrics_source == LyricsSource::SpotifyOfficial {
                this.settings_dirty.set(true);
            }
            this.refresh_debug();
        });

        let this = Rc::clone(self);
        self.window.connect_notify_local(Some("width"), move |_, _| {
            this.render_lyrics();
        });

        let this = Rc::clone(self);
        self.window.connect_notify_local(Some("height"), move |_, _| {
            this.render_lyrics();
        });

        let this = Rc::clone(self);
        self.change_button.connect_clicked(move |_| {
            this.menu_popover.popdown();
            this.open_change_popover();
        });

        let this = Rc::clone(self);
        self.settings_button.connect_clicked(move |_| {
            this.menu_popover.popdown();
            if this.settings_popover.is_visible() {
                this.settings_popover.popdown();
            } else {
                this.sync_settings_ui();
                this.refresh_debug();
                this.settings_popover.popup();
            }
        });

        let this = Rc::clone(self);
        self.settings_close_button.connect_clicked(move |_| {
            this.settings_popover.popdown();
        });

        let this = Rc::clone(self);
        self.settings_popover.connect_notify_local(Some("visible"), move |popover, _| {
            if popover.is_visible() {
                return;
            }
            if this.settings_dirty.replace(false) {
                this.reload_current_track();
            }
        });

        let this = Rc::clone(self);
        self.window.connect_realize(move |_| {
            this.schedule_topmost_refresh();
            this.schedule_restore_geometry();
        });

        let this = Rc::clone(self);
        self.window.connect_close_request(move |_| {
            this.save_window_geometry();
            this.app_hold.borrow_mut().take();
            glib::Propagation::Proceed
        });

        let this = Rc::clone(self);
        self.sync_button.connect_clicked(move |_| {
            this.resume_following();
        });

        let this = Rc::clone(self);
        self.lyrics_vadjustment.connect_value_changed(move |adj| {
            if this.suppress_scroll_signal.get() {
                return;
            }
            if this.state.borrow().selected_lyrics.is_some() {
                this.mark_manual_scroll(adj.value());
            }
        });
    }

    fn install_timers(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::timeout_add_local(Duration::from_millis(180), move || {
            this.tick();
            ControlFlow::Continue
        });

        let this = Rc::clone(self);
        glib::timeout_add_local(Duration::from_millis(80), move || {
            this.drain_worker_messages();
            ControlFlow::Continue
        });
    }

    fn tick(self: &Rc<Self>) {
        match read_snapshot() {
            Ok(snapshot) => self.handle_snapshot(snapshot),
            Err(err) => self
                .debug_state_label
                .set_text(&format!("State: DBus read failed: {err:#}")),
        }

        let current_track = { self.state.borrow().current_track.clone() };
        if let Some(track) = current_track {
            self.update_current_line(&track);
            if self.config.borrow().char_follow {
                self.refresh_current_line_markup();
            }
        }
    }

    fn handle_snapshot(self: &Rc<Self>, snapshot: SpotifySnapshot) {
        let Some(track) = snapshot.track else {
            self.clear_track_view("No music playing");
            return;
        };

        let key = self.active_lyrics_key(&track);
        let track_changed = {
            let state = self.state.borrow();
            state.current_track_key.as_ref() != Some(&key)
        };

        if track_changed {
            self.reset_for_new_track(track.clone(), key.clone());
            self.set_track_header(&track);
            self.load_cached_or_fetch(track, key);
            self.render_lyrics();
            self.refresh_debug();
            return;
        }

        {
            let mut state = self.state.borrow_mut();
            state.current_track = Some(track.clone());
        }

        self.set_track_header(&track);
        self.refresh_debug();
    }

    fn active_lyrics_key(&self, track: &TrackInfo) -> String {
        let source = self.config.borrow().lyrics_source.as_str();
        format!("{source}:{}", track.cache_key())
    }

    fn reset_for_new_track(&self, track: TrackInfo, key: String) {
        let mut state = self.state.borrow_mut();
        state.current_track_key = Some(key);
        state.current_track = Some(track);
        state.current_candidates.clear();
        state.selected_candidate_id = None;
        state.selected_lyrics = None;
        state.current_line_index = 0;
        state.loading_best = None;
        state.loading_search = None;
        state.fetch_generation = state.fetch_generation.saturating_add(1);
    }

    fn load_cached_or_fetch(self: &Rc<Self>, track: TrackInfo, key: String) {
        let source = self.config.borrow().lyrics_source;
        let legacy_key = legacy_track_key(&track);
        let loaded = match self.store.load(&key) {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) if source == LyricsSource::Lrclib => self.store.load(&legacy_key),
            Ok(None) => Ok(None),
            Err(err) if source == LyricsSource::Lrclib => {
                match self.store.load(&legacy_key) {
                    Ok(value) => Ok(value),
                    Err(_) => Err(err),
                }
            }
            Err(err) => Err(err),
        };

        match loaded {
            Ok(Some(CachedLyrics {
                selected_candidate_id,
                candidates,
                ..
            })) => {
                self.apply_candidates(track, key, candidates, selected_candidate_id, true, true);
            }
            Ok(None) => match source {
                LyricsSource::Lrclib => self.spawn_primary_fetch(track, key),
                LyricsSource::SpotifyOfficial => self.spawn_primary_fetch(track, key),
            },
            Err(err) => {
                self.debug_state_label
                    .set_text(&format!("State: Cache read failed: {err:#}"));
                self.spawn_primary_fetch(track, key);
            }
        }
    }

    fn spawn_primary_fetch(self: &Rc<Self>, track: TrackInfo, key: String) {
        let (generation, source) = {
            let mut state = self.state.borrow_mut();
            let loading = state.loading_best.as_ref().is_some_and(|current| current == &key);
            if loading {
                return;
            }
            state.loading_best = Some(key.clone());
            (state.fetch_generation, self.config.borrow().lyrics_source)
        };

        self.debug_state_label.set_text(match source {
            LyricsSource::Lrclib => "State: Loading LRCLIB lyrics...",
            LyricsSource::SpotifyOfficial => "State: Loading Spotify official lyrics...",
        });
        let tx = self.tx.clone();
        let sp_dc = self.config.borrow().sp_dc.clone();
        thread::spawn(move || {
            let result = fetch_candidate_for_source(&track, source, &sp_dc);
            match result {
                Ok(candidate) => {
                    eprintln!(
                        "primary fetched: {} - {} | synced={} plain={} instrumental={} source={}",
                        track.artist,
                        track.title,
                        candidate.has_synced(),
                        candidate.has_plain(),
                        candidate.instrumental,
                        source.as_str()
                    );
                    let _ = tx.send(WorkerMsg::BestFetched {
                        fetch_generation: generation,
                        track_key: key,
                        track,
                        candidate,
                    });
                }
                Err(err) => {
                    eprintln!(
                        "primary fetch failed: {} - {} | {err:#}",
                        track.artist, track.title
                    );
                    let _ = tx.send(WorkerMsg::Failed {
                        fetch_generation: generation,
                        track_key: key,
                        source: match source {
                            LyricsSource::Lrclib => "lrclib",
                            LyricsSource::SpotifyOfficial => "spotify_official",
                        },
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    fn spawn_search_fetch(self: &Rc<Self>, track: TrackInfo, key: String) {
        {
            let mut state = self.state.borrow_mut();
            if state
                .loading_search
                .as_ref()
                .is_some_and(|current| current == &key)
            {
                return;
            }
            state.loading_search = Some(key.clone());
        }

        self.debug_state_label
            .set_text("State: Searching more candidates...");
        let tx = self.tx.clone();
        let generation = self.state.borrow().fetch_generation;
        thread::spawn(move || match fetch_search_candidates(&track) {
            Ok(candidates) => {
                let _ = tx.send(WorkerMsg::SearchFetched {
                    fetch_generation: generation,
                    track_key: key,
                    candidates,
                });
            }
            Err(err) => {
                let _ = tx.send(WorkerMsg::Failed {
                    fetch_generation: generation,
                    track_key: key,
                    source: "search",
                    error: err.to_string(),
                });
            }
        });
    }

    fn drain_worker_messages(self: &Rc<Self>) {
        let mut messages = Vec::new();
        {
            let rx = self.rx.borrow();
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }

        for msg in messages {
            match msg {
                WorkerMsg::BestFetched {
                    fetch_generation,
                    track_key,
                    track,
                    candidate,
                } => {
                    let should_apply = {
                        let state = self.state.borrow();
                        state.current_track_key.as_ref() == Some(&track_key)
                            && state.fetch_generation == fetch_generation
                    };
                    if !should_apply {
                        continue;
                    }
                    self.apply_candidates(track, track_key, vec![candidate], None, false, true);
                }
                WorkerMsg::SearchFetched {
                    fetch_generation,
                    track_key,
                    candidates,
                } => {
                    let track = {
                        let state = self.state.borrow();
                        if state.current_track_key.as_ref() != Some(&track_key)
                            || state.fetch_generation != fetch_generation
                        {
                            None
                        } else {
                            state.current_track.clone()
                        }
                    };
                    if let Some(track) = track {
                        self.apply_candidates(track, track_key, candidates, None, false, false);
                    }
                }
                WorkerMsg::Failed {
                    fetch_generation,
                    track_key,
                    source,
                    error,
                } => {
                    let should_apply = {
                        let state = self.state.borrow();
                        state.current_track_key.as_ref() == Some(&track_key)
                            && state.fetch_generation == fetch_generation
                    };
                    if !should_apply {
                        continue;
                    }
                    let mut state = self.state.borrow_mut();
                    if source == "search" {
                        state.loading_search = None;
                    } else {
                        state.loading_best = None;
                    }
                    self.debug_state_label
                        .set_text(&format!("State: {source} lyrics fetch failed: {error}"));
                }
            }
        }
    }

    fn apply_candidates(
        self: &Rc<Self>,
        track: TrackInfo,
        track_key: String,
        candidates: Vec<LyricsCandidate>,
        selected_candidate_id: Option<i64>,
        from_cache: bool,
        auto_apply: bool,
    ) {
        let mut state = self.state.borrow_mut();
        state.current_track = Some(track.clone());
        state.current_track_key = Some(track_key.clone());
        if auto_apply {
            state.loading_best = None;
        } else {
            state.loading_search = None;
        }
        state.current_candidates = candidates.clone();

        let selected = pick_selected_candidate(&candidates, selected_candidate_id)
            .or_else(|| candidates.first().cloned());
        if auto_apply {
            state.selected_candidate_id = selected.as_ref().map(|candidate| candidate.id);
            state.selected_lyrics = selected.as_ref().map(parse_candidate);
            state.current_line_index = 0;
        }
        drop(state);

        eprintln!(
            "apply_candidates: track={} candidates={} selected={} auto_apply={} from_cache={}",
            track.cache_key(),
            candidates.len(),
            selected.as_ref().map(|c| c.id.to_string()).unwrap_or_else(|| "none".to_string()),
            auto_apply,
            from_cache
        );

        if auto_apply {
            let cache_key = self.active_lyrics_key(&track);
            if let Err(err) = self.store.save(
                &cache_key,
                &track,
                selected.as_ref().map(|candidate| candidate.id),
                &candidates,
            ) {
                self.debug_state_label
                    .set_text(&format!("State: Cache save failed: {err:#}"));
            } else if from_cache {
                self.debug_state_label.set_text("State: Loaded from cache");
            } else {
                self.debug_state_label.set_text("State: Loaded lyrics");
            }
            self.state.borrow_mut().current_track_key = Some(cache_key);
            self.render_lyrics();
        } else if from_cache {
            self.debug_state_label
                .set_text("State: Loaded candidate list from cache");
        } else {
            self.debug_state_label.set_text("State: Search results ready");
        }

        self.render_candidate_rows();
        self.refresh_debug();
    }

    fn open_change_popover(self: &Rc<Self>) {
        let current_track = { self.state.borrow().current_track.clone() };
        if let Some(track) = current_track {
            let source = self.config.borrow().lyrics_source;
            if source == LyricsSource::SpotifyOfficial {
                self.render_candidate_rows();
            } else {
                let key = self.active_lyrics_key(&track);
                let should_fetch = {
                    let state = self.state.borrow();
                    state.current_candidates.len() <= 1
                        && state.loading_search.as_ref() != Some(&key)
                };
                if should_fetch {
                    self.spawn_search_fetch(track, key);
                } else {
                    self.render_candidate_rows();
                }
            }
        }
        self.change_popover.popup();
    }

    fn reload_current_track(self: &Rc<Self>) {
        let current_track = { self.state.borrow().current_track.clone() };
        if let Some(track) = current_track {
            let key = self.active_lyrics_key(&track);
            self.reset_for_new_track(track.clone(), key.clone());
            self.set_track_header(&track);
            self.load_cached_or_fetch(track, key);
            self.render_lyrics();
            self.refresh_debug();
        }
    }

    fn render_candidate_rows(self: &Rc<Self>) {
        while let Some(child) = self.candidate_list.first_child() {
            self.candidate_list.remove(&child);
        }

        let candidates = self.state.borrow().current_candidates.clone();
        if candidates.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            row.set_activatable(false);
            row.set_child(Some(&gtk::Label::new(Some("No candidates loaded yet"))));
            self.candidate_list.append(&row);
            return;
        }

        for (index, candidate) in candidates.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            let row_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let title = gtk::Label::new(Some(&candidate.display_title()));
            title.set_xalign(0.0);
            title.set_wrap(true);
            let meta = gtk::Label::new(Some(&format!(
                "id {} • {:.0}s",
                candidate.id, candidate.duration
            )));
            meta.set_xalign(0.0);
            meta.add_css_class("dim-label");

            let button = gtk::Button::with_label("Use this");
            let this = Rc::clone(self);
            button.connect_clicked(move |_| {
                this.select_candidate(index);
            });

            row_box.append(&title);
            row_box.append(&meta);
            row_box.append(&button);
            row.set_child(Some(&row_box));
            self.candidate_list.append(&row);
        }
    }

    fn select_candidate(self: &Rc<Self>, index: usize) {
        let (candidate, track, candidates) = {
            let state = self.state.borrow();
            if index >= state.current_candidates.len() {
                return;
            }
            (
                state.current_candidates[index].clone(),
                state.current_track.clone(),
                state.current_candidates.clone(),
            )
        };

        {
            let mut state = self.state.borrow_mut();
            state.selected_candidate_id = Some(candidate.id);
            state.selected_lyrics = Some(parse_candidate(&candidate));
            state.current_line_index = 0;
            state.loading_best = None;
            state.loading_search = None;
            state.fetch_generation = state.fetch_generation.saturating_add(1);
        }

        if let Some(track) = track {
            let cache_key = self.active_lyrics_key(&track);
            if let Err(err) = self
                .store
                .save(&cache_key, &track, Some(candidate.id), &candidates)
            {
                self.debug_state_label
                    .set_text(&format!("State: Cache save failed: {err:#}"));
            } else {
                self.debug_state_label
                    .set_text(&format!("State: Selected candidate {}", candidate.id));
            }
            self.state.borrow_mut().current_track_key = Some(cache_key);
        }

        self.render_lyrics();
        self.refresh_debug();
    }

    fn update_current_line(self: &Rc<Self>, track: &TrackInfo) {
        let effective_position = self.effective_position_ms(track);
        let (lyrics, current_index) = {
            let state = self.state.borrow();
            (state.selected_lyrics.clone(), state.current_line_index)
        };

        let Some(lyrics) = lyrics else {
            return;
        };

        if !track.playback_status.eq_ignore_ascii_case("Playing")
            && !track.playback_status.eq_ignore_ascii_case("Paused")
        {
            return;
        }

        let Some((index, _line)) = lyrics
            .lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.time_ms <= effective_position)
        else {
            return;
        };

        if index != current_index {
            self.state.borrow_mut().current_line_index = index;
            self.refresh_current_line_markup();
        }
    }

    fn render_lyrics(self: &Rc<Self>) {
        self.suppress_scroll_signal.set(true);
        while let Some(child) = self.lyrics_box.first_child() {
            self.lyrics_box.remove(&child);
        }
        self.lyrics_line_widgets.borrow_mut().clear();

        let state = self.state.borrow();
        let Some(lyrics) = state.selected_lyrics.clone() else {
            let label = gtk::Label::new(Some("No lyrics loaded yet."));
            label.set_wrap(true);
            label.set_xalign(0.0);
            self.lyrics_box.append(&label);
            self.suppress_scroll_signal.set(false);
            return;
        };

        if lyrics.lines.is_empty() {
            eprintln!(
                "render_lyrics plain: lines={} plain_lines={}",
                lyrics.lines.len(),
                lyrics.plain_lines.len()
            );
            if lyrics.plain_lines.is_empty() {
                let label = gtk::Label::new(Some("No lyrics content"));
                label.set_wrap(true);
                label.set_xalign(0.0);
                self.lyrics_box.append(&label);
            } else {
                for line in &lyrics.plain_lines {
                    let label = gtk::Label::new(Some(line));
                    label.set_wrap(true);
                    label.set_selectable(true);
                    label.set_xalign(0.0);
                    self.lyrics_box.append(&label);
                    self.lyrics_line_widgets.borrow_mut().push(label);
                }
            }
            drop(state);
            self.refresh_current_line_markup();
            self.suppress_scroll_signal.set(false);
            return;
        }
        eprintln!("render_lyrics synced: lines={}", lyrics.lines.len());
        for _line in &lyrics.lines {
            let label = gtk::Label::new(None);
            label.set_wrap(true);
            label.set_selectable(true);
            label.set_xalign(0.0);
            self.lyrics_box.append(&label);
            self.lyrics_line_widgets.borrow_mut().push(label);
        }

        drop(state);
        self.refresh_current_line_markup();
        self.suppress_scroll_signal.set(false);
    }

    fn clear_track_view(&self, message: &str) {
        self.track_label.set_markup("<b>No music playing</b>");
        {
            let mut state = self.state.borrow_mut();
            state.current_track_key = None;
            state.current_track = None;
            state.current_candidates.clear();
            state.selected_candidate_id = None;
            state.selected_lyrics = None;
            state.current_line_index = 0;
            state.loading_best = None;
            state.loading_search = None;
        }
        while let Some(child) = self.lyrics_box.first_child() {
            self.lyrics_box.remove(&child);
        }
        self.lyrics_line_widgets.borrow_mut().clear();
        let label = gtk::Label::new(Some(message));
        label.set_wrap(true);
        label.set_xalign(0.0);
        self.lyrics_box.append(&label);
        while let Some(child) = self.candidate_list.first_child() {
            self.candidate_list.remove(&child);
        }
        self.sync_button.set_visible(false);
        self.refresh_debug();
    }

    fn set_track_header(&self, track: &TrackInfo) {
        self.track_label
            .set_markup(&format!("<b>{}</b>", escape_markup(&format!("{} - {}", track.artist, track.title))));
    }

    fn apply_topmost(&self) {
        let topmost = self.config.borrow().topmost;
        let Some(surface) = self.window.surface() else {
            return;
        };

        let Some(x11_surface) = surface.downcast_ref::<gdk4_x11::X11Surface>() else {
            return;
        };

        let Ok((conn, screen_num)) = x11rb::connect(None) else {
            return;
        };

        let Ok(net_wm_state_cookie) = conn.intern_atom(false, b"_NET_WM_STATE") else {
            return;
        };
        let Ok(net_wm_state_above_cookie) =
            conn.intern_atom(false, b"_NET_WM_STATE_ABOVE")
        else {
            return;
        };
        let Ok(net_wm_state_sticky_cookie) = conn.intern_atom(false, b"_NET_WM_STATE_STICKY")
        else {
            return;
        };

        let Ok(net_wm_state) = net_wm_state_cookie.reply() else {
            return;
        };
        let Ok(net_wm_state_above) = net_wm_state_above_cookie.reply() else {
            return;
        };
        let Ok(net_wm_state_sticky) = net_wm_state_sticky_cookie.reply() else {
            return;
        };

        let root = conn.setup().roots[screen_num].root;
        let action = if topmost { 1 } else { 0 };
        let Ok(window_id) = u32::try_from(x11_surface.xid()) else {
            return;
        };

        let state_atoms = if topmost {
            vec![net_wm_state_above.atom, net_wm_state_sticky.atom]
        } else {
            Vec::new()
        };
        let _ = X11WrapperConnectionExt::change_property32(
            &conn,
            x11rb::protocol::xproto::PropMode::REPLACE,
            window_id,
            net_wm_state.atom,
            x11rb::protocol::xproto::AtomEnum::ATOM,
            &state_atoms,
        );

        let event = ClientMessageEvent::new(
            32,
            window_id,
            net_wm_state.atom,
            ClientMessageData::from([
                action,
                net_wm_state_above.atom,
                net_wm_state_sticky.atom,
                1,
                0,
            ]),
        );

        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = conn.flush();
    }

    fn schedule_restore_geometry(self: &Rc<Self>) {
        if self.restore_applied.get() {
            return;
        }
        self.restore_attempts.set(0);
        let cfg = self.config.borrow().clone();
        eprintln!(
            "schedule_restore_geometry: want {}x{}+{}+{}",
            cfg.window_width, cfg.window_height, cfg.window_x, cfg.window_y
        );

        let this = Rc::clone(self);
        glib::idle_add_local_once(move || {
            this.try_restore_window_geometry();
        });

        let this = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            this.try_restore_window_geometry();
        });
    }

    fn try_restore_window_geometry(self: &Rc<Self>) {
        if self.restore_applied.get() {
            return;
        }
        let cfg = self.config.borrow().clone();
        let attempt = self.restore_attempts.get() + 1;
        eprintln!(
            "try_restore_window_geometry attempt {}: target {}x{}+{}+{}",
            attempt, cfg.window_width, cfg.window_height, cfg.window_x, cfg.window_y
        );
        self.window.unmaximize();
        self.window.unfullscreen();
        let Some(surface) = self.window.surface() else {
            eprintln!("try_restore_window_geometry: no surface yet");
            self.retry_restore_geometry();
            return;
        };

        let Some(x11_surface) = surface.downcast_ref::<gdk4_x11::X11Surface>() else {
            eprintln!("try_restore_window_geometry: non-x11 surface, restore skipped");
            return;
        };

        let Ok((conn, _screen_num)) = x11rb::connect(None) else {
            return;
        };

        let Ok(window_id) = u32::try_from(x11_surface.xid()) else {
            return;
        };
        eprintln!(
            "try_restore_window_geometry: backend=x11 xid={}",
            window_id
        );

        let values = x11rb::protocol::xproto::ConfigureWindowAux::new()
            .x(cfg.window_x)
            .y(cfg.window_y)
            .width(cfg.window_width as u32)
            .height(cfg.window_height as u32);
        if conn.configure_window(window_id, &values).is_ok() && conn.flush().is_ok() {
            if let Some((x, y, w, h)) = self.current_window_geometry() {
                let matched = (x - cfg.window_x).abs() <= 8
                    && (y - cfg.window_y).abs() <= 8
                    && (w - cfg.window_width).abs() <= 16
                    && (h - cfg.window_height).abs() <= 16;
                eprintln!(
                    "try_restore_window_geometry: actual {}x{}+{}+{} matched={}",
                    w, h, x, y, matched
                );
                self.remember_window_geometry((x, y, w, h));
                if matched {
                    self.restore_applied.set(true);
                } else {
                    self.retry_restore_geometry();
                }
            } else {
                eprintln!("try_restore_window_geometry: geometry readback failed");
                self.retry_restore_geometry();
            }
            if self.restore_applied.get() {
                self.schedule_topmost_refresh();
                eprintln!("try_restore_window_geometry: restore applied");
            }
        } else {
            eprintln!("try_restore_window_geometry: configure_window failed");
            self.retry_restore_geometry();
        }
    }

    fn retry_restore_geometry(self: &Rc<Self>) {
        let attempts = self.restore_attempts.get();
        if attempts >= 5 {
            eprintln!("retry_restore_geometry: giving up after {} attempts", attempts);
            return;
        }
        self.restore_attempts.set(attempts + 1);
        eprintln!(
            "retry_restore_geometry: scheduling retry {}",
            attempts + 1
        );
        let this = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            this.try_restore_window_geometry();
        });
    }

    fn save_window_geometry(&self) {
        let geometry = self
            .current_window_geometry()
            .or_else(|| *self.last_window_geometry.borrow());
        let Some((x, y, width, height)) = geometry else {
            eprintln!("save_window_geometry: unable to read current geometry");
            return;
        };
        eprintln!(
            "save_window_geometry: current {}x{}+{}+{}",
            width, height, x, y
        );
        self.remember_window_geometry((x, y, width, height));

        let mut config = self.config.borrow_mut();
        config.window_x = x;
        config.window_y = y;
        config.window_width = width.max(320);
        config.window_height = height.max(240);
        drop(config);
        self.persist_settings();
    }

    fn current_window_geometry(&self) -> Option<(i32, i32, i32, i32)> {
        let surface = self.window.surface()?;
        let x11_surface = surface.downcast_ref::<gdk4_x11::X11Surface>()?;
        let (conn, _) = x11rb::connect(None).ok()?;
        let window_id = u32::try_from(x11_surface.xid()).ok()?;
        let geom = conn.get_geometry(window_id).ok()?.reply().ok()?;
        Some((
            geom.x.into(),
            geom.y.into(),
            geom.width.into(),
            geom.height.into(),
        ))
    }

    fn remember_window_geometry(&self, geometry: (i32, i32, i32, i32)) {
        *self.last_window_geometry.borrow_mut() = Some(geometry);
    }

    fn apply_visual_settings(&self) {
        let cfg = self.config.borrow().clone();
        self.window.set_opacity(cfg.background_opacity.clamp(0.2, 1.0));
    }

    fn refresh_current_line_markup(&self) {
        let state = self.state.borrow();
        let Some(lyrics) = state.selected_lyrics.as_ref() else {
            return;
        };
        let widgets = self.lyrics_line_widgets.borrow();
        if lyrics.lines.is_empty() {
            if widgets.len() != lyrics.plain_lines.len() {
                return;
            }
            for (label, line) in widgets.iter().zip(lyrics.plain_lines.iter()) {
                label.set_markup(&format!(
                    "<span foreground=\"#c8c8c8\">{}</span>",
                    escape_markup(line)
                ));
            }
            self.sync_button.set_visible(false);
            return;
        }
        if widgets.len() != lyrics.lines.len() {
            return;
        }

        let current = state
            .current_line_index
            .min(lyrics.lines.len().saturating_sub(1));
        let effective_position = state
            .current_track
            .as_ref()
            .map(|track| self.effective_position_ms(track))
            .unwrap_or_default();
        let char_follow = self.config.borrow().char_follow;

        for (index, (label, line)) in widgets.iter().zip(lyrics.lines.iter()).enumerate() {
            let markup = if index == current {
                if char_follow {
                    let next_time = lyrics
                        .lines
                        .get(index + 1)
                        .map(|next| next.time_ms)
                        .unwrap_or(line.time_ms.saturating_add(1500));
                    let progress = self.line_progress(effective_position, line.time_ms, next_time);
                    self.char_follow_markup(&line.text, progress)
                } else {
                    format!(
                        "<span size=\"x-large\" weight=\"bold\" foreground=\"#ffd36b\">{}</span>",
                        escape_markup(&line.text)
                    )
                }
            } else {
                format!(
                    "<span foreground=\"#c8c8c8\">{}</span>",
                    escape_markup(&line.text)
                )
            };
            label.set_markup(&markup);
        }
        self.scroll_to_current_line();
    }

    fn scroll_to_current_line(&self) {
        let state = self.state.borrow();
        if state.user_scrolled {
            self.sync_button.set_visible(true);
            return;
        }
        let Some(lyrics) = state.selected_lyrics.as_ref() else {
            return;
        };
        if lyrics.lines.is_empty() {
            self.sync_button.set_visible(false);
            return;
        }
        let widgets = self.lyrics_line_widgets.borrow();
        let current = state
            .current_line_index
            .min(lyrics.lines.len().saturating_sub(1));
        let Some(label) = widgets.get(current) else {
            return;
        };
        let Some((_, y)) = label.translate_coordinates(&self.lyrics_box, 0.0, 0.0) else {
            return;
        };
        let label_h = label.allocated_height();
        let viewport_h = self.lyrics_area.allocated_height().max(1);
        let mut target = y + (label_h as f64) / 2.0 - (viewport_h as f64) / 2.0;
        if target < 0.0 {
            target = 0.0;
        }
        let max_value = (self.lyrics_vadjustment.upper() - self.lyrics_vadjustment.page_size())
            .max(0.0);
        let target = (target as f64).min(max_value).max(0.0);
        self.suppress_scroll_signal.set(true);
        self.lyrics_vadjustment.set_value(target);
        self.suppress_scroll_signal.set(false);
        self.sync_button.set_visible(false);
    }

    fn mark_manual_scroll(&self, value: f64) {
        let mut state = self.state.borrow_mut();
        if !state.user_scrolled {
            state.user_scrolled = true;
        }
        drop(state);
        self.sync_button.set_visible(true);
        self.debug_timing_label
            .set_text(&format!("Timing: manual scroll at {:.0}", value));
    }

    fn resume_following(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.user_scrolled = false;
        }
        self.sync_button.set_visible(false);
        self.refresh_current_line_markup();
    }

    fn persist_settings(&self) {
        let settings = AppSettings::from(self.config.borrow().clone());
        if let Err(err) = self.store.save_settings(&settings) {
            eprintln!("persist_settings: failed: {err:#}");
            self.debug_state_label
                .set_text(&format!("State: settings save failed: {err:#}"));
        } else {
            eprintln!("persist_settings: saved");
        }
    }

    fn schedule_topmost_refresh(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::idle_add_local_once(move || {
            this.apply_topmost();
        });
        let this = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            this.apply_topmost();
        });
    }

    fn sync_settings_ui(&self) {
        self.suppress_setting_updates.set(true);
        let config = self.config.borrow().clone();
        self.topmost_switch.set_active(config.topmost);
        self.opacity_spin.set_value(config.background_opacity);
        self.timing_offset_spin
            .set_value(config.timing_offset_ms as f64);
        self.char_follow_switch.set_active(config.char_follow);
        self.lyrics_source_combo
            .set_active_id(Some(config.lyrics_source.as_str()));
        self.sp_dc_entry.set_text(&config.sp_dc);
        self.suppress_setting_updates.set(false);
        self.apply_visual_settings();
    }

    fn refresh_debug(&self) {
        let state = self.state.borrow();
        let cfg = self.config.borrow().clone();
        let track = state
            .current_track
            .as_ref()
            .map(|t| format!("{} - {}", t.artist, t.title))
            .unwrap_or_else(|| "none".to_string());
        let playback = state
            .current_track
            .as_ref()
            .map(|t| {
                format!(
                    "{} • {} • {} ms • offset {} ms",
                    t.album, t.playback_status, t.position_ms, cfg.timing_offset_ms
                )
            })
            .unwrap_or_else(|| "none".to_string());
        let candidate = state
            .selected_candidate_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string());
        let loading = match (
            state.loading_best.as_ref(),
            state.loading_search.as_ref(),
        ) {
            (Some(best), Some(search)) if best == search => "best/search loading same track".to_string(),
            (Some(_), Some(_)) => "best + search loading".to_string(),
            (Some(_), None) => match cfg.lyrics_source {
                LyricsSource::SpotifyOfficial => "spotify official loading".to_string(),
                LyricsSource::Lrclib => "best loading".to_string(),
            },
            (None, Some(_)) => "search loading".to_string(),
            (None, None) => "idle".to_string(),
        };

        self.debug_track_label.set_text(&format!("Track: {track}"));
        self.debug_playback_label
            .set_text(&format!("Playback: {playback}"));
        self.debug_state_label.set_text(&format!(
            "State: {loading} • topmost={} • opacity={:.2} • char={} • source={} • offset={}ms",
            cfg.topmost,
            cfg.background_opacity,
            cfg.char_follow,
            cfg.lyrics_source.as_str(),
            cfg.timing_offset_ms
        ));
        self.debug_candidate_label
            .set_text(&format!("Candidate: {candidate}"));
        self.debug_timing_label.set_text(
            "Timing: positive offset advances the highlight. Character-follow is heuristic and line-based.",
        );
    }
}

impl AppController {
    fn effective_position_ms(&self, track: &TrackInfo) -> u64 {
        let offset = self.config.borrow().timing_offset_ms as i64;
        let effective = track.position_ms as i64 + offset;
        effective.max(0) as u64
    }

    fn line_progress(&self, position: u64, line_time: u64, next_time: u64) -> f64 {
        if next_time <= line_time {
            return 1.0;
        }
        let elapsed = position.saturating_sub(line_time) as f64;
        let span = (next_time - line_time) as f64;
        (elapsed / span).clamp(0.0, 1.0)
    }

    fn char_follow_markup(&self, text: &str, progress: f64) -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return "<span foreground=\"#ffd36b\"></span>".to_string();
        }
        let split = ((chars.len() as f64) * progress).ceil() as usize;
        let split = split.min(chars.len());
        let active = escape_markup(&chars[..split].iter().collect::<String>());
        let rest = escape_markup(&chars[split..].iter().collect::<String>());
        format!(
            "<span size=\"x-large\" weight=\"bold\"><span foreground=\"#ffd36b\">{}</span><span foreground=\"#c8c8c8\">{}</span></span>",
            active, rest
        )
    }
}

fn pick_selected_candidate(
    candidates: &[LyricsCandidate],
    selected_candidate_id: Option<i64>,
) -> Option<LyricsCandidate> {
    if let Some(id) = selected_candidate_id {
        if let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) {
            return Some(candidate.clone());
        }
    }
    candidates.first().cloned()
}

fn escape_markup(input: &str) -> String {
    glib::markup_escape_text(input).to_string()
}

fn legacy_track_key(track: &TrackInfo) -> String {
    if let Some(track_id) = &track.track_id {
        return track_id.clone();
    }

    format!(
        "{}|{}|{}",
        normalize_legacy(&track.artist),
        normalize_legacy(&track.title),
        track.duration_ms / 1000
    )
}

fn normalize_legacy(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .replace('\u{3000}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn labeled_row<T: IsA<gtk::Widget>>(label: &str, widget: &T) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let title = gtk::Label::new(Some(label));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_wrap(true);
    widget.set_hexpand(false);
    widget.set_halign(gtk::Align::End);
    row.append(&title);
    row.append(widget);
    row
}
