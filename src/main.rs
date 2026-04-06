use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dotenvy::dotenv;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Line},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Gauge},
    Frame, Terminal,
};
use serde_json::Value;
use std::io::Write;
use std::{env, io, time::Duration};
use tokio::sync::mpsc;

use librespot_connect::{Spirc, ConnectConfig};
use librespot_core::authentication::Credentials as LibrespotCredentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig, NoOpVolume};
use librespot_playback::player::Player;

use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize)]
struct SpotifyTokenCache {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

// Models
#[derive(Clone, Serialize, Deserialize)]
struct Playlist {
    id: String,
    name: String,
    owner_id: String,
    collaborative: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Track {
    name: String,
    artist: String,
    album: String,
    duration_ms: u64,
    uri: String,
}

#[derive(Default, Serialize, Deserialize, Clone)]
struct AppCache {
    playlists: Vec<Playlist>,
    tracks: HashMap<String, Vec<Track>>,
    last_opened: HashMap<String, u64>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct PlayerState {
    track_name: String,
    artist: String,
    progress_ms: u64,
    duration_ms: u64,
    is_playing: bool,
    volume_percent: u8,
    album_art_url: Option<String>,
}

// GUI State
enum View {
    Playlists,
    LoadingTracks { spinner_tick: u8 },
    Tracks { playlist_id: String, playlist_name: String, tracks: Vec<Track>, state: ListState, search_query: String, is_searching: bool },
}

struct AppState {
    display_name: String,
    user_id: String,
    show_others: bool,
    app_cache: AppCache,
    filtered_playlists: Vec<Playlist>,
    playlist_state: ListState,
    current_view: View,
    access_token: String,
    player_state: Option<PlayerState>,
    
    current_art_url: Option<String>,
    current_art_bytes: Option<Vec<u8>>,
    current_art_protocol: Option<StatefulProtocol>,
    picker: Picker,
}

// Async Message passing
enum AppMessage {
    TracksFetched { playlist_id: String, playlist_name: String, tracks: Vec<Track> },
    FetchError(String),
    UpdatePlayerState(Option<PlayerState>),
    UpdateAlbumArt(String, Vec<u8>),
    PlaylistsRefreshed(Vec<Playlist>),
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let rem_secs = secs % 60;
    format!("{}:{:02}", mins, rem_secs)
}

fn load_cache() -> AppCache {
    if let Ok(content) = std::fs::read_to_string(".spotme_cache.json") {
        if let Ok(cache) = serde_json::from_str(&content) {
            return cache;
        }
    }
    AppCache::default()
}

fn save_cache(cache: &AppCache) {
    if let Ok(content) = serde_json::to_string(cache) {
        let _ = std::fs::write(".spotme_cache.json", content);
    }
}

fn get_current_unix_time() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

async fn get_or_refresh_token(client_id: &str, client_secret: &str, redirect_uri: &str) -> Result<String> {
    let cache_path = ".spotify_token_cache.json";
    
    if let Ok(content) = std::fs::read_to_string(cache_path) {
        if let Ok(cache) = serde_json::from_str::<SpotifyTokenCache>(&content) {
            if get_current_unix_time() < cache.expires_at {
                return Ok(cache.access_token);
            }
            
            let client = reqwest::Client::new();
            let res = client.post("https://accounts.spotify.com/api/token")
                .basic_auth(client_id, Some(client_secret))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &cache.refresh_token),
                ])
                .send()
                .await;
                
            if let Ok(response) = res {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    if let Some(access) = json["access_token"].as_str() {
                        let refresh = json["refresh_token"].as_str().unwrap_or(&cache.refresh_token);
                        let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
                        
                        let new_cache = SpotifyTokenCache {
                            access_token: access.to_string(),
                            refresh_token: refresh.to_string(),
                            expires_at: get_current_unix_time() + expires_in,
                        };
                        
                        if let Ok(cache_str) = serde_json::to_string(&new_cache) {
                            let _ = std::fs::write(cache_path, cache_str);
                        }
                        return Ok(access.to_string());
                    }
                }
            }
        }
    }
    
    let scopes = "user-read-private user-read-email playlist-read-private playlist-read-collaborative user-modify-playback-state user-read-playback-state streaming";
    let enc_redirect = redirect_uri.replace(":", "%3A").replace("/", "%2F");
    let enc_scopes = scopes.replace(" ", "%20");
    
    let auth_url = format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}",
        client_id, enc_redirect, enc_scopes
    );
    
    println!("Please open this URL in your browser:");
    println!("{}", auth_url);
    print!("Paste the redirected URL here: ");
    std::io::stdout().flush()?;
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim();
    
    let code = if let Some(idx) = input.find("code=") {
        input[idx + 5..].split('&').next().unwrap_or("").to_string()
    } else {
        return Err(anyhow::anyhow!("Could not find code in URL"));
    };
    
    let client = reqwest::Client::new();
    let response = client.post("https://accounts.spotify.com/api/token")
        .basic_auth(client_id, Some(client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await?;
        
    let json = response.json::<serde_json::Value>().await?;
    if let Some(access) = json["access_token"].as_str() {
        let refresh = json["refresh_token"].as_str().unwrap_or("");
        let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
        
        let new_cache = SpotifyTokenCache {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: get_current_unix_time() + expires_in,
        };
        
        if let Ok(cache_str) = serde_json::to_string(&new_cache) {
            let _ = std::fs::write(cache_path, cache_str);
        }
        return Ok(access.to_string());
    }
    
    Err(anyhow::anyhow!("Failed to parse token response: {}", json))
}

async fn fetch_user_profile(token: &str) -> Result<(String, String)> {
    let client = reqwest::Client::new();
    let res = client.get("https://api.spotify.com/v1/me")
        .bearer_auth(token)
        .send()
        .await?;
        
    let json = res.json::<serde_json::Value>().await?;
    let display_name = json["display_name"].as_str().unwrap_or("Unknown").to_string();
    let id = json["id"].as_str().unwrap_or("").to_string();
    Ok((display_name, id))
}

// Background Task for Librespot Daemon
async fn start_librespot_daemon(token: String) -> Result<()> {
    let credentials = LibrespotCredentials::with_access_token(token);
    let session_config = SessionConfig::default();
    
    // Connect Session
    let session = Session::new(session_config, None);

    let backend = audio_backend::find(None).expect("No audio backend found");
    let player_config = PlayerConfig::default();
    
    let player = Player::new(
        player_config,
        session.clone(),
        Box::new(NoOpVolume),
        move || {
            backend(None, AudioFormat::default())
        },
    );

    let mixer = mixer::find(None).expect("No mixer found");
    let mut connect_config = ConnectConfig::default();
    connect_config.name = "SpotMe Local Player".to_string();

    let (_spirc, spirc_task) = Spirc::new(
        connect_config,
        session,
        credentials,
        player,
        mixer(MixerConfig::default())?,
    ).await?;

    // Block here to keep the Spotify Connect daemon active internally!
    spirc_task.await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    
    // Auth Flow
    let client_id = env::var("SPOTIFY_CLIENT_ID").expect("Missing SPOTIFY_CLIENT_ID");
    let client_secret = env::var("SPOTIFY_CLIENT_SECRET").expect("Missing SPOTIFY_CLIENT_SECRET");
    let redirect_uri = env::var("SPOTIFY_REDIRECT_URI").expect("Missing SPOTIFY_REDIRECT_URI");

    let access_token = get_or_refresh_token(&client_id, &client_secret, &redirect_uri).await?;
    let (display_name, raw_user_id) = fetch_user_profile(&access_token).await?;

    // Launch standalone librespot headless local player using our auth token
    if !access_token.is_empty() {
        let t = access_token.clone();
        tokio::spawn(async move {
            if let Err(e) = start_librespot_daemon(t).await {
                let _ = std::fs::write("/tmp/spotme.log", format!("Librespot error: {}", e));
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
    let filtered_playlists: Vec<Playlist> = app_cache.playlists.iter().filter(|p| {
        show_others || p.owner_id == user_id || p.collaborative
    }).cloned().collect();

    let mut playlist_state = ListState::default();
    if !filtered_playlists.is_empty() {
        playlist_state.select(Some(0));
    }

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    let app_state = AppState {
        display_name,
        user_id,
        show_others,
        app_cache,
        filtered_playlists,
        playlist_state,
        current_view: View::Playlists,
        access_token,
        player_state: None,
        current_art_url: None,
        current_art_bytes: None,
        current_art_protocol: None,
        picker,
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

// Playback API Commands
async fn play_track(token: &str, uri: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    
    // Find our specific Local SpotMe daemon device to ensure music originates here
    let mut device_id = None;
    if let Ok(res) = client.get("https://api.spotify.com/v1/me/player/devices")
        .bearer_auth(token)
        .send().await 
    {
        if let Ok(json) = res.json::<serde_json::Value>().await {
            if let Some(devices) = json["devices"].as_array() {
                for dev in devices {
                    if let Some(name) = dev["name"].as_str() {
                        if name == "SpotMe Local Player" {
                            device_id = dev["id"].as_str().map(|s| s.to_string());
                            break;
                        }
                    }
                }
            }
        }
    }

    let mut url = "https://api.spotify.com/v1/me/player/play".to_string();
    if let Some(id) = device_id {
        url = format!("{}?device_id={}", url, id);
    }

    let body = serde_json::json!({ "uris": [uri] });
    let req_res = client.put(&url)
        .bearer_auth(token)
        .json(&body)
        .send().await;
        
    match req_res {
        Ok(r) => { 
            let _ = std::fs::write("/tmp/spotme.log", format!("Play request sent. Status: {}, URL: {}", r.status(), url)); 
        }
        Err(e) => { 
            let _ = std::fs::write("/tmp/spotme.log", format!("Play request FAILED: {}", e)); 
        }
    }
    Ok(())
}

async fn pause_playback(token: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    client.put("https://api.spotify.com/v1/me/player/pause")
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

async fn resume_playback(token: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    client.put("https://api.spotify.com/v1/me/player/play")
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

async fn seek_playback(token: &str, position_ms: u64) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    let url = format!("https://api.spotify.com/v1/me/player/seek?position_ms={}", position_ms);
    client.put(&url)
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

// Track Fetch Hook
async fn fetch_playlists_api(token: &str) -> Vec<Playlist> {
    let client = reqwest::Client::new();
    let mut url = "https://api.spotify.com/v1/me/playlists?limit=50".to_string();
    let mut out = Vec::new();

    loop {
        if let Ok(res) = client.get(&url).bearer_auth(token).send().await {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                if let Some(items) = json["items"].as_array() {
                    for item in items {
                        if let (Some(name), Some(id)) = (item["name"].as_str(), item["id"].as_str()) {
                            let owner = item["owner"]["id"].as_str().unwrap_or("unknown").to_string();
                            let collab = item["collaborative"].as_bool().unwrap_or(false);
                            out.push(Playlist { name: name.to_string(), id: id.to_string(), owner_id: owner, collaborative: collab });
                        }
                    }
                }
                if let Some(n) = json["next"].as_str() {
                    url = n.to_string();
                } else {
                    break;
                }
            } else { break; }
        } else { break; }
    }
    out
}

async fn fetch_tracks(token: String, playlist_id: String) -> Result<Vec<Track>, anyhow::Error> {
    let client = reqwest::Client::new();
    let mut url = format!("https://api.spotify.com/v1/playlists/{}/items?market=from_token", playlist_id);
    let mut tracks = Vec::new();
    #[allow(unused_assignments)]
    let mut raw_text_fallback = String::new();

    loop {
        let res = client.get(&url).bearer_auth(&token).send().await?;
        let raw_text = res.text().await?;
        raw_text_fallback = raw_text.clone();
        let json: Value = serde_json::from_str(&raw_text)?;
        
        if let Some(items) = json["items"].as_array() {
            if items.is_empty() && tracks.is_empty() {
                return Err(anyhow::anyhow!("API answered with 0 items."));
            }
            for item in items {
                let mut track_obj = &item["track"];
                if track_obj.is_null() {
                    track_obj = &item["item"];
                }
                if track_obj.is_null() || !track_obj.is_object() {
                    continue; 
                }
                
                let name = track_obj["name"].as_str().unwrap_or("Unknown Track").to_string();
                let uri = track_obj["uri"].as_str().unwrap_or("").to_string();
                
                let mut artists = Vec::new();
                if let Some(artists_arr) = track_obj["artists"].as_array() {
                    for artist in artists_arr {
                        if let Some(a_name) = artist["name"].as_str() {
                            artists.push(a_name.to_string());
                        }
                    }
                }
                let artist_str = if artists.is_empty() { "Unknown Artist".to_string() } else { artists.join(", ") };
                
                let album = track_obj["album"]["name"].as_str().unwrap_or("Unknown Album").to_string();
                let duration_ms = track_obj["duration_ms"].as_u64().unwrap_or(0);
                
                tracks.push(Track { name, artist: artist_str, album, duration_ms, uri });
            }
        } else {
            if tracks.is_empty() {
                return Err(anyhow::anyhow!("Failed to parse response payload array. Raw: {}", raw_text));
            } else {
                break;
            }
        }
        
        if let Some(next_url) = json["next"].as_str() {
            url = next_url.to_string();
        } else {
            break;
        }
    }
    
    if tracks.is_empty() {
        return Err(anyhow::anyhow!("Loaded items but found 0 playable tracks! Payload: {}", raw_text_fallback.chars().take(2000).collect::<String>()));
    }
    
    Ok(tracks)
}

async fn set_volume(token: &str, percent: u8) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    client.put(&format!("https://api.spotify.com/v1/me/player/volume?volume_percent={}", percent))
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app_state: AppState) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMessage>();

    // Start background poller for currently playing track
    let poll_token = app_state.access_token.clone();
    let poll_tx = tx.clone();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut last_art_url: Option<String> = None;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            let res = client.get("https://api.spotify.com/v1/me/player")
                .bearer_auth(&poll_token)
                .send().await;
            if let Ok(r) = res {
                if r.status() == reqwest::StatusCode::NO_CONTENT {
                    let _ = poll_tx.send(AppMessage::UpdatePlayerState(None));
                } else if let Ok(json) = r.json::<serde_json::Value>().await {
                    let track_obj = &json["item"];
                    if track_obj.is_object() {
                        let name = track_obj["name"].as_str().unwrap_or("Unknown").to_string();
                        let artist = track_obj["artists"][0]["name"].as_str().unwrap_or("Unknown").to_string();
                        let progress = json["progress_ms"].as_u64().unwrap_or(0);
                        let duration = track_obj["duration_ms"].as_u64().unwrap_or(0);
                        let is_playing = json["is_playing"].as_bool().unwrap_or(false);
                        let volume = json["device"]["volume_percent"].as_u64().unwrap_or(100) as u8;
                        let art_url = track_obj["album"]["images"][0]["url"].as_str().map(|s| s.to_string());
                        
                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(Some(PlayerState { 
                            track_name: name, artist, progress_ms: progress, duration_ms: duration, is_playing, volume_percent: volume, album_art_url: art_url.clone()
                        })));
                        
                        if let Some(url) = art_url {
                            if last_art_url.as_ref() != Some(&url) {
                                last_art_url = Some(url.clone());
                                let art_tx = poll_tx.clone();
                                let art_client = client.clone();
                                tokio::spawn(async move {
                                    if let Ok(ares) = art_client.get(&url).send().await {
                                        if let Ok(bytes) = ares.bytes().await {
                                            let _ = art_tx.send(AppMessage::UpdateAlbumArt(url, bytes.to_vec()));
                                        }
                                    }
                                });
                            }
                        }
                    } else {
                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(None));
                    }
                }
            }
        }
    });

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
                
                app_state.filtered_playlists = app_state.app_cache.playlists.iter().filter(|p| {
                    app_state.show_others || p.owner_id == app_state.user_id || p.collaborative
                }).cloned().collect();
                
                if !app_state.filtered_playlists.is_empty() {
                    app_state.playlist_state.select(Some(0));
                }
            }
            AppMessage::TracksFetched { playlist_id, playlist_name, tracks } => {
                app_state.app_cache.tracks.insert(playlist_id.clone(), tracks.clone());
                app_state.app_cache.last_opened.insert(playlist_id.clone(), get_current_unix_time());
                save_cache(&app_state.app_cache);
                
                let mut list_state = ListState::default();
                if !tracks.is_empty() {
                    list_state.select(Some(0));
                }
                app_state.current_view = View::Tracks { playlist_id, playlist_name, tracks, state: list_state, search_query: String::new(), is_searching: false };
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
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::UpdatePlayerState(pstate) => {
                    app_state.player_state = pstate;
                }
                AppMessage::UpdateAlbumArt(url, bytes) => {
                    app_state.current_art_url = Some(url);
                    app_state.current_art_bytes = Some(bytes.clone());
                    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                        let protocol = app_state.picker.new_resize_protocol(dyn_img);
                        app_state.current_art_protocol = Some(protocol);
                    }
                }
            }
        }

        // Advance spinner if loading
        if let View::LoadingTracks { ref mut spinner_tick } = app_state.current_view {
            *spinner_tick = spinner_tick.wrapping_add(1);
        }

        // Draw UI
        terminal.draw(|f| ui(f, &mut app_state)).map_err(|_| anyhow::anyhow!("TUI draw error"))?;

        // GUI IO Polling logic
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let mut is_typing = false;
                if let View::Tracks { is_searching, .. } = &app_state.current_view {
                    is_typing = *is_searching;
                }

                if !is_typing {
                    // Global Playback Hotkeys!
                    match key.code {
                    KeyCode::Char(' ') => {
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let is_playing = player.is_playing;
                            tokio::spawn(async move {
                                if is_playing { let _ = pause_playback(&token).await; } 
                                else { let _ = resume_playback(&token).await; }
                            });
                            app_state.player_state.as_mut().unwrap().is_playing = !is_playing;
                        }
                    }
                    KeyCode::Left => { // Seek back 5s
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let seek_ms = player.progress_ms.saturating_sub(5000);
                            app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                            tokio::spawn(async move { let _ = seek_playback(&token, seek_ms).await; });
                        }
                    }
                    KeyCode::Right => { // Seek forward 5s
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let seek_ms = std::cmp::min(player.progress_ms + 5000, player.duration_ms);
                            app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                            tokio::spawn(async move { let _ = seek_playback(&token, seek_ms).await; });
                        }
                    }
                    KeyCode::Char('h') | KeyCode::Char('H') => { // Seek back 15s
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let seek_ms = player.progress_ms.saturating_sub(15000);
                            app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                            tokio::spawn(async move { let _ = seek_playback(&token, seek_ms).await; });
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Char('L') => { // Seek forward 15s
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let seek_ms = std::cmp::min(player.progress_ms + 15000, player.duration_ms);
                            app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                            tokio::spawn(async move { let _ = seek_playback(&token, seek_ms).await; });
                        }
                    }
                    KeyCode::Char('i') => {
                        let next = match app_state.picker.protocol_type() {
                            ProtocolType::Halfblocks => ProtocolType::Kitty,
                            ProtocolType::Kitty => ProtocolType::Iterm2,
                            ProtocolType::Iterm2 => ProtocolType::Sixel,
                            ProtocolType::Sixel => ProtocolType::Halfblocks,
                        };
                        app_state.picker.set_protocol_type(next);
                        if let Some(bytes) = &app_state.current_art_bytes {
                            if let Ok(dyn_img) = image::load_from_memory(bytes) {
                                let protocol = app_state.picker.new_resize_protocol(dyn_img);
                                app_state.current_art_protocol = Some(protocol);
                            }
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let vol = std::cmp::min(player.volume_percent + 5, 100);
                            app_state.player_state.as_mut().unwrap().volume_percent = vol;
                            tokio::spawn(async move { let _ = set_volume(&token, vol).await; });
                        }
                    }
                    KeyCode::Char('-') | KeyCode::Char('_') => {
                        if let Some(player) = &app_state.player_state {
                            let token = app_state.access_token.clone();
                            let vol = player.volume_percent.saturating_sub(5);
                            app_state.player_state.as_mut().unwrap().volume_percent = vol;
                            tokio::spawn(async move { let _ = set_volume(&token, vol).await; });
                        }
                    }
                    _ => {}
                }
                } // End !is_typing

                // View-specific events
                match app_state.current_view {
                    View::Playlists => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Down | KeyCode::Char('j') => {
                                let i = match app_state.playlist_state.selected() {
                                    Some(i) => if i >= app_state.filtered_playlists.len().saturating_sub(1) { 0 } else { i + 1 },
                                    None => 0,
                                };
                                app_state.playlist_state.select(Some(i));
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                let i = match app_state.playlist_state.selected() {
                                    Some(i) => if i == 0 { app_state.filtered_playlists.len().saturating_sub(1) } else { i - 1 },
                                    None => 0,
                                };
                                app_state.playlist_state.select(Some(i));
                            }
                            KeyCode::Char('o') => {
                                app_state.show_others = !app_state.show_others;
                                app_state.filtered_playlists = app_state.app_cache.playlists.iter().filter(|p| {
                                    app_state.show_others || p.owner_id == app_state.user_id || p.collaborative
                                }).cloned().collect();
                                app_state.playlist_state.select(if app_state.filtered_playlists.is_empty() { None } else { Some(0) });
                            }
                            KeyCode::Char('r') => {
                                let tx = tx.clone();
                                let token = app_state.access_token.clone();
                                tokio::spawn(async move {
                                    let lists = fetch_playlists_api(&token).await;
                                    let _ = tx.send(AppMessage::PlaylistsRefreshed(lists));
                                });
                            }
                            KeyCode::Enter => {
                                if let Some(i) = app_state.playlist_state.selected() {
                                    let playlist = &app_state.filtered_playlists[i];
                                    let p_id = playlist.id.clone();
                                    let p_name = playlist.name.clone();
                                    let token = app_state.access_token.clone();
                                    
                                    // Cache fast-path logic!
                                    if let Some(cached_tracks) = app_state.app_cache.tracks.get(&p_id) {
                                        app_state.app_cache.last_opened.insert(p_id.clone(), get_current_unix_time());
                                        save_cache(&app_state.app_cache);
                                        
                                        let mut state = ListState::default();
                                        if !cached_tracks.is_empty() { state.select(Some(0)); }
                                        app_state.current_view = View::Tracks { playlist_id: p_id, playlist_name: p_name, tracks: cached_tracks.clone(), state, search_query: String::new(), is_searching: false };
                                    } else {
                                        app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                                        let tx = tx.clone();
                                        tokio::spawn(async move {
                                            match fetch_tracks(token, p_id.clone()).await {
                                                Ok(tracks) => { let _ = tx.send(AppMessage::TracksFetched{ playlist_id: p_id, playlist_name: p_name, tracks }); }
                                                Err(e) => { let _ = tx.send(AppMessage::FetchError(e.to_string())); }
                                            }
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    View::Tracks { playlist_id: ref active_pid, ref mut state, ref tracks, ref mut is_searching, ref mut search_query, .. } => {
                        let inner_pid = active_pid.clone();
                        
                        let filtered_tracks: Vec<&Track> = tracks.iter().filter(|t| {
                            let q = search_query.to_lowercase();
                            q.is_empty() || t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.to_lowercase().contains(&q)
                        }).collect();
                        
                        if *is_searching {
                            match key.code {
                                KeyCode::Esc | KeyCode::Enter => {
                                    *is_searching = false;
                                    if key.code == KeyCode::Esc {
                                        search_query.clear();
                                    }
                                }
                                KeyCode::Backspace => {
                                    search_query.pop();
                                }
                                KeyCode::Char(c) => {
                                    search_query.push(c);
                                }
                                _ => {}
                            }
                            
                            let new_len = tracks.iter().filter(|t| {
                                let q = search_query.to_lowercase();
                                q.is_empty() || t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.to_lowercase().contains(&q)
                            }).count();
                            
                            if new_len == 0 { state.select(None); }
                            else if state.selected().is_none() || state.selected().unwrap() >= new_len { state.select(Some(0)); }
                        } else {
                            match key.code {
                                KeyCode::Char('/') => {
                                    *is_searching = true;
                                    search_query.clear();
                                }
                                KeyCode::Char('r') => {
                                    let token = app_state.access_token.clone();
                                    let tx = tx.clone();
                                    let p_id = inner_pid.clone();
                                    let p_name = app_state.filtered_playlists.iter().find(|p| p.id == p_id).map(|p| p.name.clone()).unwrap_or("Tracks".to_string());
                                    app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                                    tokio::spawn(async move {
                                        match fetch_tracks(token, p_id.clone()).await {
                                            Ok(tracks) => { let _ = tx.send(AppMessage::TracksFetched{ playlist_id: p_id, playlist_name: p_name, tracks }); }
                                            Err(e) => { let _ = tx.send(AppMessage::FetchError(e.to_string())); }
                                        }
                                    });
                                }
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('b') => {
                                    app_state.current_view = View::Playlists;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let i = match state.selected() {
                                        Some(i) => if i >= filtered_tracks.len().saturating_sub(1) { 0 } else { i + 1 },
                                        None => 0,
                                    };
                                    state.select(Some(i));
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let i = match state.selected() {
                                        Some(i) => if i == 0 { filtered_tracks.len().saturating_sub(1) } else { i - 1 },
                                        None => 0,
                                    };
                                    state.select(Some(i));
                                }
                                KeyCode::Enter => {
                                    if let Some(i) = state.selected() {
                                        if i < filtered_tracks.len() {
                                            let token = app_state.access_token.clone();
                                            let uri = filtered_tracks[i].uri.clone();
                                            if !uri.is_empty() {
                                                tokio::spawn(async move {
                                                    let _ = play_track(&token, &uri).await;
                                                });
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    View::LoadingTracks { .. } => {
                        if let KeyCode::Char('q') | KeyCode::Esc = key.code {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, state: &mut AppState) {
    let bottom_height = if state.current_art_protocol.is_some() { 14 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3), 
            Constraint::Min(1),
            Constraint::Length(bottom_height)
        ])
        .split(f.area());

    // Top banner
    let nav_hint = match state.current_view {
        View::Playlists => "(↑/↓ Nav, +/- Vol, o Others, r Refresh, Enter View, i Mode, q Quit)",
        View::Tracks { is_searching, .. } => {
            if is_searching { "(Type to search, Enter/Esc to exit search)" }
            else { "(↑/↓ Nav, +/- Vol, / Search, r Sync, Esc Back, Enter PLAY, i Mode, q Quit)" }
        }
        View::LoadingTracks { .. } => "(Loading...)",
    };
    
    let welcome_msg = format!("SpotMe Client - Welcome, {}! {}", state.display_name, nav_hint);
    let banner = Paragraph::new(welcome_msg)
        .block(Block::default().borders(Borders::ALL).title("User Info"))
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(banner, chunks[0]);

    // Active View
    match &mut state.current_view {
        View::Playlists => {
            let items: Vec<ListItem> = state.filtered_playlists
                .iter()
                .map(|p| ListItem::new(p.name.clone()))
                .collect();

            let playlist_list = List::new(items)
                .block(Block::default().title("Your Playlists").borders(Borders::ALL))
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD))
                .highlight_symbol(">> ");

            f.render_stateful_widget(playlist_list, chunks[1], &mut state.playlist_state);
        }
        View::LoadingTracks { spinner_tick } => {
            let spinner = vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let idx = (*spinner_tick as usize) % spinner.len();
            
            let p = Paragraph::new(format!("{} Loading tracks...", spinner[idx]))
                .block(Block::default().borders(Borders::ALL).title("Loading"))
                .style(Style::default().fg(Color::Yellow));
                
            f.render_widget(p, chunks[1]);
        }
        View::Tracks { playlist_id: _, playlist_name, tracks, state: list_state, search_query, is_searching } => {
            let filtered_tracks: Vec<&Track> = tracks.iter().filter(|t| {
                let q = search_query.to_lowercase();
                q.is_empty() || t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.to_lowercase().contains(&q)
            }).collect();
            
            let items: Vec<ListItem> = filtered_tracks
                .iter()
                .map(|t| {
                    let metadata = format!("{} | {} ({})", t.artist, t.album, format_duration(t.duration_ms));
                    let line1 = Line::from(Span::styled(t.name.clone(), Style::default().add_modifier(Modifier::BOLD)));
                    let line2 = Line::from(Span::styled(metadata, Style::default().fg(Color::DarkGray)));
                    ListItem::new(vec![line1, line2])
                })
                .collect();

            let mut title_style = Style::default();
            let title = if *is_searching {
                title_style = title_style.fg(Color::Yellow);
                format!("Tracks in {} [Search: {}█]", playlist_name, search_query)
            } else if !search_query.is_empty() {
                title_style = title_style.fg(Color::Yellow);
                format!("Tracks in {} [Search: {}]", playlist_name, search_query)
            } else {
                format!("Tracks in {}", playlist_name)
            };

            let tracks_list = List::new(items)
                .block(Block::default().title(Span::styled(title, title_style)).borders(Borders::ALL))
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().bg(Color::Magenta).fg(Color::Black))
                .highlight_symbol(">> ");

            f.render_stateful_widget(tracks_list, chunks[1], list_state);
        }
    }

    // Bottom Player Box
    let mode_str = match state.picker.protocol_type() {
        ProtocolType::Halfblocks => "HalfBlocks",
        ProtocolType::Kitty => "Kitty HD",
        ProtocolType::Iterm2 => "Iterm2 HD",
        ProtocolType::Sixel => "Sixel HD",
    };
    let player_title = format!("Spotify Desktop Remote [{}]", mode_str);
    let player_block = Block::default().borders(Borders::ALL).title(player_title);
    let inner_area = player_block.inner(chunks[2]);
    f.render_widget(player_block, chunks[2]);
    
    if let Some(player) = &state.player_state {
        let mut sub_chunks = vec![inner_area];
        if state.current_art_protocol.is_some() {
            let split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(0)])
                .split(inner_area);
            sub_chunks = split.to_vec();
            
            if let Some(protocol) = state.current_art_protocol.as_mut() {
                let img_widget = StatefulImage::default();
                f.render_stateful_widget(img_widget, sub_chunks[0], protocol);
            }
        }
        
        let target_area = if sub_chunks.len() > 1 { sub_chunks[1] } else { sub_chunks[0] };
        
        let status = if player.is_playing { "▶ PLAYING " } else { "⏸ PAUSED  " };
        let info = format!("{} \u{2014} {} [{}] Vol: {}% \u{2014} {} / {}", 
            player.track_name, player.artist, status, player.volume_percent,
            format_duration(player.progress_ms), format_duration(player.duration_ms));
        
        let mut progress_ratio = 0.0;
        if player.duration_ms > 0 {
            progress_ratio = (player.progress_ms as f64 / player.duration_ms as f64).clamp(0.0, 1.0);
        }
        
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Green).bg(Color::Black))
            .ratio(progress_ratio)
            .label(Span::styled(info, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
            
        f.render_widget(gauge, target_area);
    } else {
        let text = Paragraph::new(" Booting internal audio decoder. Setting up local Spotify Connect link... 🔊")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, inner_area);
    }
}
