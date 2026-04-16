mod api;
mod app;
mod config;
use crate::api::endpoints::*;
use crate::api::models::*;
use crate::app::state::*;
use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dotenvy::dotenv;
use ratatui::widgets::ListState;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    Terminal,
};

use ratatui_image::picker::Picker;
use std::{env, io, time::Duration};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "spotme",
    about = "A terminal-based Spotify client with album art, lyrics, and local playback",
    version,
    author
)]
struct Cli {
    /// Print config file path and exit
    #[arg(long)]
    config_path: bool,

    /// Print cache directory path and exit
    #[arg(long)]
    cache_path: bool,

    /// Reset config to defaults
    #[arg(long)]
    reset_config: bool,

    /// Set initial volume (0-100)
    #[arg(long, value_name = "0-100")]
    volume: Option<u8>,
}

// Models extracted to src/api/models.rs

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let rem_secs = secs % 60;
    format!("{}:{:02}", mins, rem_secs)
}

fn load_cache() -> AppCache {
    if let Ok(content) = std::fs::read_to_string(&config::paths().cache_file) {
        if let Ok(cache) = serde_json::from_str(&content) {
            return cache;
        }
    }
    AppCache::default()
}

fn save_cache(cache: &AppCache) {
    if let Ok(content) = serde_json::to_string(cache) {
        let cache_path = &config::paths().cache_file;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(cache_path)
            {
                use std::io::Write;
                let _ = file.write_all(content.as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::write(cache_path, content);
        }
    }
}

pub fn get_current_unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Initialize the background log writer thread. Must be called once at startup.
pub fn init_logger() {
    use std::sync::mpsc;
    static LOG_INIT: std::sync::Once = std::sync::Once::new();
    LOG_INIT.call_once(|| {
        let (tx, rx) = mpsc::channel::<String>();
        LOG_TX.set(tx).ok();
        std::thread::spawn(move || {
            use std::io::Write;
            for entry in rx {
                let log_path = &config::paths().log_file;
                if let Ok(meta) = std::fs::metadata(log_path) {
                    if meta.len() > 1_000_000 {
                        let _ = std::fs::rename(log_path, log_path.with_extension("log.old"));
                    }
                }
                #[cfg(unix)]
                let file_result = {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .mode(0o600)
                        .open(log_path)
                };
                #[cfg(not(unix))]
                let file_result = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path);
                if let Ok(mut file) = file_result {
                    let _ = writeln!(file, "{}", entry);
                }
            }
        });
    });
}

static LOG_TX: std::sync::OnceLock<std::sync::mpsc::Sender<String>> = std::sync::OnceLock::new();

/// Non-blocking log: sends the message to a dedicated writer thread via channel.
/// Safe to call from both sync and async contexts without blocking tokio executors.
pub fn app_log(msg: &str) {
    if let Some(tx) = LOG_TX.get() {
        let ts = get_current_unix_time();
        let _ = tx.send(format!("[{}] {}", ts, msg));
    }
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app_state: AppState) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMessage>();
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let mut background_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    if let Some(ref ps) = app_state.player_state {
        if let Some(ref url) = ps.album_art_url {
            let art_tx = tx.clone();
            let art_url = url.clone();
            tokio::spawn(async move {
                let client = crate::api::get_client();
                if let Ok(ares) = client.get(&art_url).send().await {
                    if let Ok(bytes) = ares.bytes().await {
                        let _ = art_tx.send(AppMessage::UpdateAlbumArt(art_url, bytes.to_vec()));
                    }
                }
            });
        }
    }

    // Start background poller for currently playing track
    let poll_token = app_state.access_token.clone();
    let poll_client_id = app_state.client_id.clone();
    let poll_tx = tx.clone();
    let mut poll_shutdown_rx = shutdown_tx.subscribe();
    let poller_handle = tokio::spawn(async move {
        let client = crate::api::get_client();
        let mut last_art_url: Option<String> = None;
        let mut current_token = poll_token;
        let mut poll_interval_ms: u64 = 1000;
        const BASE_INTERVAL_MS: u64 = 1000;
        const MAX_INTERVAL_MS: u64 = 30_000;
        loop {
            tokio::select! {
                _ = poll_shutdown_rx.changed() => break,
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(poll_interval_ms)) => {}
            }
            let res = client
                .get(format!("{}/v1/me/player", crate::api::api_base_url()))
                .bearer_auth(&current_token)
                .send()
                .await;
            if let Ok(r) = res {
                // Handle 429 Too Many Requests with Retry-After backoff
                if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let retry_after = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(5);
                    app_log(&format!("Rate limited, backing off {}s", retry_after));
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_after)).await;
                    poll_interval_ms = (poll_interval_ms * 2).min(MAX_INTERVAL_MS);
                    continue;
                }
                // Refresh token on 401 Unauthorized
                if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                    if let Some(new_token) =
                        crate::api::endpoints::try_refresh_token(&poll_client_id).await
                    {
                        current_token = new_token;
                    }
                    poll_interval_ms = (poll_interval_ms * 2).min(MAX_INTERVAL_MS);
                    continue;
                }
                // Successful response — reset interval to base
                if r.status().is_success() || r.status() == reqwest::StatusCode::NO_CONTENT {
                    poll_interval_ms = BASE_INTERVAL_MS;
                }
                if r.status() == reqwest::StatusCode::NO_CONTENT {
                    let _ = poll_tx.send(AppMessage::UpdatePlayerState(None));
                } else if let Ok(json) = r.json::<serde_json::Value>().await {
                    let track_obj = &json["item"];
                    if track_obj.is_object() {
                        let name = match track_obj["name"].as_str() {
                            Some(n) => n.to_string(),
                            None => {
                                app_log("PARSE WARN: missing track name in player response");
                                "Unknown".to_string()
                            }
                        };
                        let artist = match track_obj["artists"][0]["name"].as_str() {
                            Some(a) => a.to_string(),
                            None => {
                                app_log("PARSE WARN: missing artist in player response");
                                "Unknown".to_string()
                            }
                        };
                        let progress = match json["progress_ms"].as_u64() {
                            Some(p) => p,
                            None => {
                                app_log("PARSE WARN: missing progress_ms, defaulting to 0");
                                0
                            }
                        };
                        let duration = match track_obj["duration_ms"].as_u64() {
                            Some(d) => d,
                            None => {
                                app_log("PARSE WARN: missing duration_ms, defaulting to 0");
                                0
                            }
                        };
                        let is_playing = json["is_playing"].as_bool().unwrap_or(false);
                        let volume = match json["device"]["volume_percent"].as_u64() {
                            Some(v) => v.min(100) as u8,
                            None => {
                                app_log("PARSE WARN: missing volume_percent, defaulting to 100");
                                100
                            }
                        };
                        let art_url = track_obj["album"]["images"][0]["url"]
                            .as_str()
                            .map(|s| s.to_string());
                        let uri = track_obj["uri"].as_str().map(|s| s.to_string());

                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(Some(PlayerState {
                            track_uri: uri,
                            track_name: name,
                            artist,
                            progress_ms: progress,
                            duration_ms: duration,
                            is_playing,
                            volume_percent: volume,
                            album_art_url: art_url.clone(),
                            is_buffering: false,
                            is_fresh_cache: false,
                            lyrics: None,
                        })));

                        if let Some(url) = art_url {
                            if last_art_url.as_ref() != Some(&url) {
                                last_art_url = Some(url.clone());
                                let art_tx = poll_tx.clone();
                                let art_client = client.clone();
                                tokio::spawn(async move {
                                    if let Ok(ares) = art_client.get(&url).send().await {
                                        if let Ok(bytes) = ares.bytes().await {
                                            let _ = art_tx.send(AppMessage::UpdateAlbumArt(
                                                url,
                                                bytes.to_vec(),
                                            ));
                                        }
                                    }
                                });
                            }
                        }
                    } else {
                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(None));
                    }
                }
            } else {
                // Network error — back off
                poll_interval_ms = (poll_interval_ms * 2).min(MAX_INTERVAL_MS);
            }
        }
    });
    background_handles.push(poller_handle);

    loop {
        // Process async events incoming
        while let Ok(msg) = rx.try_recv() {
            match msg {
                AppMessage::PlaylistsRefreshed(lists) => {
                    app_state.app_cache.playlists = lists;
                    save_cache(&app_state.app_cache);

                    app_state.app_cache.playlists.sort_by(|a, b| {
                        let ta = app_state.app_cache.last_opened.get(&a.id).unwrap_or(&0);
                        let tb = app_state.app_cache.last_opened.get(&b.id).unwrap_or(&0);
                        tb.cmp(ta)
                    });

                    app_state.filtered_playlists = app_state
                        .app_cache
                        .playlists
                        .iter()
                        .filter(|p| {
                            app_state.show_others
                                || p.owner_id == app_state.user_id
                                || p.collaborative
                        })
                        .cloned()
                        .collect();

                    if !app_state.filtered_playlists.is_empty() {
                        app_state.playlist_state.select(Some(0));
                    }
                }
                AppMessage::TracksFetched {
                    playlist_id,
                    playlist_name,
                    tracks,
                } => {
                    app_state
                        .app_cache
                        .tracks
                        .insert(playlist_id.clone(), tracks.clone());
                    app_state
                        .app_cache
                        .last_opened
                        .insert(playlist_id.clone(), get_current_unix_time());
                    save_cache(&app_state.app_cache);

                    let mut list_state = ListState::default();
                    if !tracks.is_empty() {
                        list_state.select(Some(0));
                    }
                    app_state.current_view = View::Tracks {
                        playlist_id,
                        playlist_name,
                        tracks,
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::FetchError(err) => {
                    let mut list_state = ListState::default();
                    list_state.select(Some(0));
                    app_state.current_view = View::Tracks {
                        playlist_id: "error".to_string(),
                        playlist_name: "Error".to_string(),
                        tracks: vec![Track {
                            name: err,
                            artist: String::new(),
                            album: String::new(),
                            album_id: None,
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::UpdatePlayerState(pstate) => {
                    let now = get_current_unix_time();
                    if pstate.is_none() {
                        if app_state
                            .player_state
                            .as_ref()
                            .map(|p| p.is_buffering)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        if let Some(ref mut local_ps) = app_state.player_state {
                            local_ps.is_playing = false;
                        }
                        continue;
                    }

                    app_state.merge_incoming_player_state(pstate, now);

                    if let Some(ref mut ps) = app_state.player_state {
                        let mut cache_dirty = false;
                        let mut track_changed = false;

                        if let Some(cached) = &mut app_state.app_cache.last_player {
                            if cached.track_name != ps.track_name {
                                track_changed = true;
                            }

                            if track_changed
                                || (ps.progress_ms as i64 - cached.progress_ms as i64).abs() > 5000
                            {
                                cached.track_uri = ps.track_uri.clone();
                                cached.track_name = ps.track_name.clone();
                                cached.artist = ps.artist.clone();
                                cached.progress_ms = ps.progress_ms;
                                cached.duration_ms = ps.duration_ms;
                                cached.album_art_url = ps.album_art_url.clone();
                                cached.lyrics = ps.lyrics.clone();
                                cache_dirty = true;
                            }
                        } else {
                            app_state.app_cache.last_player = Some(CachedPlayerState {
                                track_uri: ps.track_uri.clone(),
                                track_name: ps.track_name.clone(),
                                artist: ps.artist.clone(),
                                progress_ms: ps.progress_ms,
                                duration_ms: ps.duration_ms,
                                album_art_url: ps.album_art_url.clone(),
                                lyrics: ps.lyrics.clone(),
                            });
                            track_changed = true;
                            cache_dirty = true;
                        }

                        let should_fetch = track_changed || ps.lyrics.is_none();

                        if cache_dirty {
                            save_cache(&app_state.app_cache);
                        }

                        if should_fetch {
                            ps.lyrics = Some(Lyrics::default());

                            if let Some(ref mut cached) = app_state.app_cache.last_player {
                                cached.lyrics = Some(Lyrics::default());
                            }

                            let t_name = ps.track_name.clone();
                            let t_artist = ps.artist.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                if let Ok(lyrics) = fetch_lyrics_api(&t_name, &t_artist).await {
                                    let _ = tx.send(AppMessage::LyricsLoaded(Ok(lyrics)));
                                } else {
                                    let _ =
                                        tx.send(AppMessage::LyricsLoaded(Ok(Lyrics::default())));
                                }
                            });
                        }
                    }
                }
                AppMessage::UpdateAlbumArt(url, bytes) => {
                    app_state.current_art_url = Some(url);
                    app_state.current_art_bytes = Some(bytes.clone());
                    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                        let protocol = app_state.picker.new_resize_protocol(dyn_img.clone());
                        app_state.current_art_protocol = Some(protocol);

                        let thumbnail =
                            dyn_img.resize_exact(1, 1, image::imageops::FilterType::Nearest);
                        let rgb = thumbnail.to_rgb8();
                        if let Some(pixel) = rgb.pixels().next() {
                            let p = pixel.0;
                            let dampen = 0.5; // Dampen brightness so text remains extremely legible
                            app_state.dominant_color = Some((
                                (p[0] as f32 * dampen) as u8,
                                (p[1] as f32 * dampen) as u8,
                                (p[2] as f32 * dampen) as u8,
                            ));
                        }
                    }
                }
                AppMessage::SearchResults(tracks) => {
                    let mut list_state = ListState::default();
                    if !tracks.is_empty() {
                        list_state.select(Some(0));
                    }
                    if let View::SearchGlobal {
                        query: ref mut _query,
                        tracks: ref mut t,
                        state: ref mut s,
                        ref mut is_typing,
                    } = app_state.current_view
                    {
                        *t = Some(tracks);
                        *s = list_state;
                        *is_typing = false;
                    } else {
                        app_state.current_view = View::SearchGlobal {
                            query: String::new(),
                            tracks: Some(tracks),
                            state: list_state,
                            is_typing: false,
                        };
                    }
                }
                AppMessage::SearchError(err) => {
                    if let View::SearchGlobal {
                        query: ref mut _query,
                        ref mut tracks,
                        state: ref mut _s,
                        ref mut is_typing,
                    } = app_state.current_view
                    {
                        *tracks = Some(vec![Track {
                            name: format!("Error: {}", err),
                            artist: String::new(),
                            album: String::new(),
                            album_id: None,
                            duration_ms: 0,
                            uri: "".to_string(),
                        }]);
                        *is_typing = false;
                    }
                }
                AppMessage::TrackAddedToPlaylist(playlist_id) => {
                    app_log("TRIGGERED TrackAddedToPlaylist UI Popup Return Constraint!");

                    // Discard stale offline cache for this playlist string targeting to force fresh syncs!
                    app_state.app_cache.tracks.remove(&playlist_id);

                    let mut prev = None;
                    if let View::SelectPlaylist {
                        ref mut previous, ..
                    } = app_state.current_view
                    {
                        prev = Some(std::mem::replace(previous, Box::new(View::Playlists)));
                    }
                    if let Some(p) = prev {
                        app_state.current_view = *p;
                    }
                }
                AppMessage::QueueFetched(Ok(tracks)) => {
                    let mut list_state = ListState::default();
                    if !tracks.is_empty() {
                        list_state.select(Some(0));
                    }
                    app_state.current_view = View::Tracks {
                        playlist_id: "queue".to_string(),
                        playlist_name: "Player Queue".to_string(),
                        tracks,
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::QueueFetched(Err(err)) => {
                    let mut list_state = ListState::default();
                    list_state.select(Some(0));
                    app_state.current_view = View::Tracks {
                        playlist_id: "queue_err".to_string(),
                        playlist_name: "Queue Error".to_string(),
                        tracks: vec![Track {
                            name: err,
                            artist: String::new(),
                            album: String::new(),
                            album_id: None,
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::FeaturedFetched(lists) => {
                    for pl in &lists {
                        if !app_state.app_cache.playlists.iter().any(|p| p.id == pl.id) {
                            app_state.app_cache.playlists.push(pl.clone());
                        }
                    }
                    app_state.filtered_playlists = lists;
                    app_state
                        .playlist_state
                        .select(if app_state.filtered_playlists.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
                    app_state.current_view = View::Playlists;
                }
                AppMessage::AlbumTracksFetched {
                    album_name,
                    tracks: Ok(tracks),
                } => {
                    let mut list_state = ListState::default();
                    if !tracks.is_empty() {
                        list_state.select(Some(0));
                    }
                    app_state.current_view = View::Tracks {
                        playlist_id: format!("album_{}", album_name),
                        playlist_name: album_name,
                        tracks,
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::AlbumTracksFetched {
                    album_name,
                    tracks: Err(err),
                } => {
                    let mut list_state = ListState::default();
                    list_state.select(Some(0));
                    app_state.current_view = View::Tracks {
                        playlist_id: format!("album_err_{}", album_name),
                        playlist_name: format!("Error: {}", album_name),
                        tracks: vec![Track {
                            name: err,
                            artist: String::new(),
                            album: String::new(),
                            album_id: None,
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::StatusError(msg) => {
                    app_state.status_message = Some((msg, get_current_unix_time()));
                }
                AppMessage::LyricsLoaded(result) => {
                    let parsed = result.ok();
                    if let Some(ref mut ps) = app_state.player_state {
                        ps.lyrics = parsed.clone();
                    }
                    if let Some(ref mut cached) = app_state.app_cache.last_player {
                        cached.lyrics = parsed;
                        save_cache(&app_state.app_cache);
                    }
                }
            }
        }

        // Advance spinners
        if let View::LoadingTracks {
            ref mut spinner_tick,
        } = app_state.current_view
        {
            *spinner_tick = spinner_tick.wrapping_add(1);
        }
        if app_state
            .player_state
            .as_ref()
            .map(|p| p.is_buffering)
            .unwrap_or(false)
        {
            app_state.player_spinner_tick = app_state.player_spinner_tick.wrapping_add(1);
        }

        // Clear stale status messages after 5 seconds
        if let Some((_, ts)) = &app_state.status_message {
            if get_current_unix_time().saturating_sub(*ts) >= 5 {
                app_state.status_message = None;
            }
        }

        // Draw UI
        terminal
            .draw(|f| crate::app::ui::ui(f, &mut app_state))
            .map_err(|_| anyhow::anyhow!("TUI draw error"))?;

        // GUI IO Polling logic
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if crate::app::events::handle_key_events(key, &mut app_state, &tx)? {
                    // Signal all background tasks to shut down
                    let _ = shutdown_tx.send(true);
                    // Give background tasks a moment to finish gracefully
                    for handle in background_handles {
                        let _ =
                            tokio::time::timeout(tokio::time::Duration::from_secs(2), handle).await;
                    }
                    return Ok(());
                }
            }
        }
    }
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle informational flags that exit early
    if cli.config_path {
        println!("{}", config::paths().config_file.display());
        return Ok(());
    }
    if cli.cache_path {
        println!("{}", config::paths().cache_file.parent().unwrap_or_else(|| std::path::Path::new(".")).display());
        return Ok(());
    }
    if cli.reset_config {
        let cfg = config::UserConfig::default();
        cfg.save();
        println!("Config reset to defaults at {}", config::paths().config_file.display());
        return Ok(());
    }

    let mut user_config = config::UserConfig::load();

    // CLI --volume overrides saved config
    if let Some(v) = cli.volume {
        user_config.volume = v.min(100);
    }

    init_logger();
    dotenv().ok();
    dotenvy::from_path(&crate::config::paths().env_file).ok();

    // Auth Flow utilizing PKCE (No secret needed)
    let client_id = env::var("SPOTIFY_CLIENT_ID")
        .unwrap_or_else(|_| "db41158aa95448d6914e73975652b52a".to_string());
    let redirect_uri = env::var("SPOTIFY_REDIRECT_URI")
        .unwrap_or_else(|_| "http://127.0.0.1:8480/callback".to_string());

    let access_token = get_or_refresh_token(&client_id, &redirect_uri).await?;
    let (display_name, raw_user_id) = fetch_user_profile(&access_token).await?;

    let (cmd_tx, cmd_rx) = mpsc::channel::<LocalPlayerCommand>(10);

    // Launch standalone librespot headless local player using our auth token
    if !access_token.is_empty() {
        let t = access_token.clone();
        tokio::spawn(async move {
            if let Err(e) = start_librespot_daemon(t, cmd_rx).await {
                app_log(&format!("Librespot error: {}", e));
            }
        });
    }

    // Give the local librespot daemon a second to register with Spotify clouds before we start asking for playlists (async)
    // Wait! If we have cache, we don't need to block!
    // tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let user_id = raw_user_id.replace("spotify:user:", "");

    let mut app_cache = load_cache();
    if app_cache.playlists.is_empty() && !access_token.is_empty() {
        // Block to let daemon register only on explicit first fetch
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
        app_cache.playlists = fetch_playlists_api(&access_token).await;
        save_cache(&app_cache);
    }

    app_cache.playlists.sort_by(|a, b| {
        let ta = app_cache.last_opened.get(&a.id).unwrap_or(&0);
        let tb = app_cache.last_opened.get(&b.id).unwrap_or(&0);
        tb.cmp(ta)
    });

    let show_others = false;
    let filtered_playlists: Vec<Playlist> = app_cache
        .playlists
        .iter()
        .filter(|p| show_others || p.owner_id == user_id || p.collaborative)
        .cloned()
        .collect();

    let mut playlist_state = ListState::default();
    if !filtered_playlists.is_empty() {
        playlist_state.select(Some(0));
    }

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    let initial_player_state = app_cache.last_player.as_ref().map(|cached| PlayerState {
        track_uri: cached.track_uri.clone(),
        track_name: cached.track_name.clone(),
        artist: cached.artist.clone(),
        progress_ms: cached.progress_ms,
        duration_ms: cached.duration_ms,
        is_playing: false,
        volume_percent: user_config.volume,
        album_art_url: cached.album_art_url.clone(),
        is_buffering: false,
        is_fresh_cache: true,
        lyrics: cached.lyrics.clone(),
    });

    let app_state = AppState {
        display_name,
        user_id,
        show_others,
        app_cache,
        filtered_playlists,
        playlist_state,
        current_view: View::Playlists,
        access_token,
        player_state: initial_player_state,
        current_art_url: None,
        current_art_bytes: None,
        current_art_protocol: None,
        player_spinner_tick: 0,
        picker,
        fullscreen_player: user_config.fullscreen_on_start,
        lyrics_mode: LyricsMode::Focused,
        lyrics_scroll_offset: 0,
        dominant_color: None,
        show_help: false,
        show_popup: false,
        local_cmd_tx: Some(cmd_tx),
        last_action_timestamp: 0,
        client_id,
        status_message: None,
        user_config,
    };

    // TUI setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, app_state).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(999), "0:00");
        assert_eq!(format_duration(1000), "0:01");
        assert_eq!(format_duration(59000), "0:59");
        assert_eq!(format_duration(60000), "1:00");
        assert_eq!(format_duration(61000), "1:01");
        assert_eq!(format_duration(3599000), "59:59");
        assert_eq!(format_duration(3600000), "60:00");
    }
}
