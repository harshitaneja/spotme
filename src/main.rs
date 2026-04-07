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
use std::{env, io, time::Duration};
use tokio::sync::mpsc;

use librespot_connect::{Spirc, ConnectConfig};
use librespot_core::authentication::Credentials as LibrespotCredentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::Player;

use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

struct GradientBackground {
    dominant: (u8, u8, u8),
}

impl ratatui::widgets::Widget for GradientBackground {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        for y in area.top()..area.bottom() {
            let factor = 1.0 - ((y - area.top()) as f32 / area.height as f32);
            let r = (self.dominant.0 as f32 * factor) as u8;
            let g = (self.dominant.1 as f32 * factor) as u8;
            let b = (self.dominant.2 as f32 * factor) as u8;
            
            for x in area.left()..area.right() {
                buf.get_mut(x, y).set_bg(ratatui::style::Color::Rgb(r, g, b));
            }
        }
    }
}

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
    album_id: Option<String>,
    duration_ms: u64,
    uri: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct LrcLine {
    time_ms: u64,
    text: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Lyrics {
    plain: Option<String>,
    synced: Option<Vec<LrcLine>>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct CachedPlayerState {
    #[serde(default)]
    track_uri: Option<String>,
    track_name: String,
    artist: String,
    #[serde(default)]
    progress_ms: u64,
    duration_ms: u64,
    album_art_url: Option<String>,
    lyrics: Option<Lyrics>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
struct AppCache {
    playlists: Vec<Playlist>,
    tracks: HashMap<String, Vec<Track>>,
    last_opened: HashMap<String, u64>,
    last_player: Option<CachedPlayerState>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct PlayerState {
    track_uri: Option<String>,
    track_name: String,
    artist: String,
    progress_ms: u64,
    duration_ms: u64,
    is_playing: bool,
    volume_percent: u8,
    album_art_url: Option<String>,
    is_buffering: bool,
    is_fresh_cache: bool,
    lyrics: Option<Lyrics>,
}

// GUI State
enum View {
    Playlists,
    LoadingTracks { spinner_tick: u8 },
    Tracks { playlist_id: String, playlist_name: String, tracks: Vec<Track>, state: ListState, search_query: String, is_searching: bool },
    SearchGlobal { query: String, tracks: Option<Vec<Track>>, state: ListState, is_typing: bool },
    SelectPlaylist { track_uri: String, track_name: String, state: ListState, previous: Box<View> },
}

#[derive(Clone, PartialEq)]
enum LyricsMode {
    Focused,
    Full,
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
    player_spinner_tick: u8,
    picker: Picker,
    fullscreen_player: bool,
    lyrics_mode: LyricsMode,
    lyrics_scroll_offset: usize,
    dominant_color: Option<(u8, u8, u8)>,
    show_help: bool,
    show_popup: bool,
    local_cmd_tx: Option<mpsc::Sender<LocalPlayerCommand>>,
    last_action_timestamp: u64,
}

// Async Message passing
enum AppMessage {
    TracksFetched { playlist_id: String, playlist_name: String, tracks: Vec<Track> },
    FetchError(String),
    UpdatePlayerState(Option<PlayerState>),
    UpdateAlbumArt(String, Vec<u8>),
    PlaylistsRefreshed(Vec<Playlist>),
    SearchResults(Vec<Track>),
    SearchError(String),
    TrackAddedToPlaylist(String),
    LyricsLoaded(Result<Lyrics, String>),
    QueueFetched(Result<Vec<Track>, String>),
    FeaturedFetched(Vec<Playlist>),
    AlbumTracksFetched { album_name: String, tracks: Result<Vec<Track>, String> },
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

pub fn app_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open("spotme.log") {
        let ts = get_current_unix_time();
        let _ = writeln!(file, "[{}] {}", ts, msg);
    }
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
    
    let scopes = "user-read-private user-read-email playlist-read-private playlist-read-collaborative playlist-modify-public playlist-modify-private user-modify-playback-state user-read-playback-state streaming";
    let enc_redirect = redirect_uri.replace(":", "%3A").replace("/", "%2F");
    let enc_scopes = scopes.replace(" ", "%20");
    
    let auth_url = format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&show_dialog=true",
        client_id, enc_redirect, enc_scopes
    );
    
    println!("Opening Spotify login in your browser...");
    println!("If it doesn't open automatically, please click here: \n{}\n", auth_url);
    
    let _ = open::that(&auth_url);
    
    let url_parts: Vec<&str> = redirect_uri.split(':').collect();
    let port_part = url_parts.last().unwrap_or(&"8480").split('/').next().unwrap_or("8480");
    let port_u16 = port_part.parse::<u16>().unwrap_or(8480);
    
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port_u16)).await?;
    
    println!("Waiting up to 120 seconds for browser authentication... (Press Ctrl+C to cancel)");
    
    let code = tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(120)) => {
            return Err(anyhow::anyhow!("Authentication timed out after 120 seconds. Please run SpotMe again."));
        }
        accept_res = listener.accept() => {
            match accept_res {
                Ok((mut socket, _)) => {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    
                    let mut auth_code = "".to_string();
                    for line in request.lines() {
                        if line.starts_with("GET ") && line.contains("code=") {
                            if let Some(idx) = line.find("code=") {
                                auth_code = line[idx + 5..].split('&').next().unwrap_or("").split(' ').next().unwrap_or("").to_string();
                            }
                            break;
                        }
                    }
                    
                    let response_html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1 style=\"font-family: sans-serif\">SpotMe Login Successful!</h1><p style=\"font-family: sans-serif\">You can safely close this tab and return to the terminal.</p><script>window.close();</script></body></html>";
                    let _ = socket.write_all(response_html.as_bytes()).await;
                    
                    if auth_code.is_empty() {
                        return Err(anyhow::anyhow!("Could not extract code from callback request!"));
                    }
                    auth_code
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Listener failed to accept connection: {}", e));
                }
            }
        }
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

// Local Player Commands
enum LocalPlayerCommand {
    Play,
    Pause,
}

// Background Task for Librespot Daemon
async fn start_librespot_daemon(token: String, mut receiver: mpsc::Receiver<LocalPlayerCommand>) -> Result<()> {
    let credentials = LibrespotCredentials::with_access_token(token);
    let session_config = SessionConfig::default();
    
    // Connect Session
    let session = Session::new(session_config, None);

    let backend = audio_backend::find(None).expect("No audio backend found");
    let player_config = PlayerConfig::default();
    
    let mixer_fn = mixer::find(Some("softvol")).expect("No softvol mixer found");
    let mixer_for_player = mixer_fn(MixerConfig::default())?;

    let player = Player::new(
        player_config,
        session.clone(),
        mixer_for_player.get_soft_volume(),
        move || {
            backend(None, AudioFormat::default())
        },
    );

    let mut connect_config = ConnectConfig::default();
    connect_config.name = "SpotMe Local Player".to_string();

    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session,
        credentials,
        player,
        mixer_for_player,
    ).await?;

    tokio::spawn(spirc_task);
    
    while let Some(cmd) = receiver.recv().await {
        match cmd {
            LocalPlayerCommand::Play => { let _ = spirc.play(); },
            LocalPlayerCommand::Pause => { let _ = spirc.pause(); },
        }
    }
    
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

    let (cmd_tx, cmd_rx) = mpsc::channel::<LocalPlayerCommand>(10);

    // Launch standalone librespot headless local player using our auth token
    if !access_token.is_empty() {
        let t = access_token.clone();
        tokio::spawn(async move {
            if let Err(e) = start_librespot_daemon(t, cmd_rx).await {
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

    let initial_player_state = app_cache.last_player.as_ref().map(|cached| PlayerState {
        track_uri: cached.track_uri.clone(),
        track_name: cached.track_name.clone(),
        artist: cached.artist.clone(),
        progress_ms: cached.progress_ms,
        duration_ms: cached.duration_ms,
        is_playing: false,
        volume_percent: 50,
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
        fullscreen_player: false,
        lyrics_mode: LyricsMode::Focused,
        lyrics_scroll_offset: 0,
        dominant_color: None,
        show_help: false,
        show_popup: false,
        local_cmd_tx: Some(cmd_tx),
        last_action_timestamp: 0,
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
async fn play_track(token: &str, uri: &str, position_ms: u64) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    
    // Find our specific Local SpotMe daemon device to ensure music originates here
    let mut device_id = None;
    for _ in 0..5 {
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
        if device_id.is_some() {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
    }

    let mut url = "https://api.spotify.com/v1/me/player/play".to_string();
    if let Some(id) = device_id {
        url = format!("{}?device_id={}", url, id);
    }

    let body = serde_json::json!({ "uris": [uri], "position_ms": position_ms });
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

async fn next_track(token: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    client.post("https://api.spotify.com/v1/me/player/next")
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

async fn previous_track(token: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    client.post("https://api.spotify.com/v1/me/player/previous")
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
                let album_id = track_obj["album"]["id"].as_str().map(|s| s.to_string());
                let duration_ms = track_obj["duration_ms"].as_u64().unwrap_or(0);
                
                tracks.push(Track { name, artist: artist_str, album, album_id, duration_ms, uri });
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

fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for b in input.as_bytes() {
        match *b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => encoded.push(*b as char),
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{:02X}", b)),
        }
    }
    encoded
}

async fn search_spotify_api(token: &str, query: &str) -> Result<Vec<Track>, String> {
    let client = reqwest::Client::new();
    let safe_query = url_encode(query.trim());
    
    // Spotify natively defaults to 20 limit. Leaving it omitted bypasses the 400 Bad Request parameter fault.
    let url = format!("https://api.spotify.com/v1/search?q={}&type=track", safe_query);
    
    app_log(&format!("NETWORK INIT: GET {}", url));
    let res = client.get(&url)
        .bearer_auth(token)
        .send()
        .await.map_err(|e| {
            let err_str = format!("Req Err: {}", e);
            app_log(&err_str);
            err_str
        })?;
        
    let status = res.status();
    if !status.is_success() {
        let txt = res.text().await.unwrap_or_default();
        let err_str = format!("Bad Status {}: {}", status, txt);
        app_log(&format!("NETWORK FAULT: {}", err_str));
        return Err(err_str);
    }
    
    let text_payload = res.text().await.map_err(|e| format!("Text read Err: {}", e))?;
    app_log(&format!("NETWORK SUCCESS: Payload Size {}", text_payload.len()));
    
    let json: serde_json::Value = serde_json::from_str(&text_payload).map_err(|e| {
        let err_str = format!("JSON Err: {}", e);
        app_log(&err_str);
        err_str
    })?;
    
    let mut tracks = Vec::new();
    if let Some(items) = json["tracks"]["items"].as_array() {
        for item in items {
            let name = item["name"].as_str().unwrap_or("").to_string();
            let uri = item["uri"].as_str().unwrap_or("").to_string();
            let duration_ms = item["duration_ms"].as_u64().unwrap_or(0);
            
            let mut artist_names: Vec<String> = Vec::new();
            if let Some(artists) = item["artists"].as_array() {
                for a in artists {
                    if let Some(n) = a["name"].as_str() {
                        artist_names.push(n.to_string());
                    }
                }
            }
            
            let album = item["album"]["name"].as_str().unwrap_or("").to_string();
            let album_id = item["album"]["id"].as_str().map(|s| s.to_string());
            tracks.push(Track { name, artist: artist_names.join(", "), album, album_id, duration_ms, uri });
        }
    } else {
        return Err(format!("Bad payload: no items array. {}", json));
    }
    
    Ok(tracks)
}

async fn add_track_to_playlist_api(token: &str, playlist_id: &str, track_uri: &str) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();
    let payload = serde_json::json!({ "uris": [track_uri] });
    
    let url = format!("https://api.spotify.com/v1/playlists/{}/items", playlist_id);
    app_log(&format!("ADD TRACK INIT: POST {}", url));
    app_log(&format!("ADD TRACK PAYLOAD: {}", payload));
    
    let res = client.post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await?;
        
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    
    if status.is_success() {
        app_log(&format!("ADD TRACK SUCCESS {}: {}", status, text));
        Ok(())
    } else {
        app_log(&format!("ADD TRACK FAULT {}: {}", status, text));
        anyhow::bail!("Failed to add track")
    }
}

async fn fetch_player_queue(token: &str) -> Result<Vec<Track>, String> {
    let client = reqwest::Client::new();
    let url = "https://api.spotify.com/v1/me/player/queue";
    app_log(&format!("NETWORK INIT: GET {}", url));
    let res = client.get(url).bearer_auth(token).send().await.map_err(|e| format!("Req Err: {}", e))?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("Bad Status {}: {}", status, res.text().await.unwrap_or_default()));
    }
    let text_payload = res.text().await.map_err(|e| format!("Text read Err: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&text_payload).map_err(|e| format!("JSON Err: {}", e))?;
    
    let mut tracks = Vec::new();
    
    if let Some(queue) = json["queue"].as_array() {
        for track_obj in queue {
            let name = track_obj["name"].as_str().unwrap_or("Unknown").to_string();
            let uri = track_obj["uri"].as_str().unwrap_or("").to_string();
            let album = track_obj["album"]["name"].as_str().unwrap_or("Unknown Album").to_string();
            let album_id = track_obj["album"]["id"].as_str().map(|s| s.to_string());
            let duration_ms = track_obj["duration_ms"].as_u64().unwrap_or(0);
            
            let mut artists = Vec::new();
            if let Some(artists_arr) = track_obj["artists"].as_array() {
                for artist in artists_arr {
                    if let Some(a_name) = artist["name"].as_str() {
                        artists.push(a_name.to_string());
                    }
                }
            }
            let artist_str = if artists.is_empty() { "Unknown Artist".to_string() } else { artists.join(", ") };
            
            tracks.push(Track { name, artist: artist_str, album, album_id, duration_ms, uri });
        }
    } else {
        return Err(format!("Bad payload: no queue array. {}", json));
    }
    Ok(tracks)
}

async fn fetch_album_tracks(token: &str, album_id: &str) -> Result<Vec<Track>, String> {
    let client = reqwest::Client::new();
    let url = format!("https://api.spotify.com/v1/albums/{}", album_id);
    let res = client.get(&url).bearer_auth(token).send().await.map_err(|e| format!("Req Err: {}", e))?;
    if !res.status().is_success() {
        return Err(format!("Bad Status {}", res.status()));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| format!("JSON Err: {}", e))?;
    
    let mut tracks = Vec::new();
    let album_name = json["name"].as_str().unwrap_or("").to_string();
    let album_id_opt = Some(album_id.to_string());
    
    if let Some(items) = json["tracks"]["items"].as_array() {
        for track_obj in items {
            let name = track_obj["name"].as_str().unwrap_or("Unknown").to_string();
            let uri = track_obj["uri"].as_str().unwrap_or("").to_string();
            let duration_ms = track_obj["duration_ms"].as_u64().unwrap_or(0);
            
            let mut artists = Vec::new();
            if let Some(artists_arr) = track_obj["artists"].as_array() {
                for artist in artists_arr {
                    if let Some(a_name) = artist["name"].as_str() {
                        artists.push(a_name.to_string());
                    }
                }
            }
            let artist_str = if artists.is_empty() { "Unknown Artist".to_string() } else { artists.join(", ") };
            
            tracks.push(Track { name, artist: artist_str, album: album_name.clone(), album_id: album_id_opt.clone(), duration_ms, uri });
        }
    }
    Ok(tracks)
}

async fn fetch_featured_playlists_api(token: &str) -> Vec<Playlist> {
    let client = reqwest::Client::new();
    let url = "https://api.spotify.com/v1/browse/featured-playlists?limit=50";
    if let Ok(res) = client.get(url).bearer_auth(token).send().await {
        if res.status().is_success() {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                let mut lists = Vec::new();
                if let Some(items) = json["playlists"]["items"].as_array() {
                    for item in items {
                        if item.is_null() { continue; }
                        let id = item["id"].as_str().unwrap_or("").to_string();
                        let name = item["name"].as_str().unwrap_or("Featured Playlist").to_string();
                        let owner_id = item["owner"]["id"].as_str().unwrap_or("spotify").to_string();
                        lists.push(Playlist { id, name, owner_id, collaborative: false });
                    }
                }
                return lists;
            }
        }
    }
    Vec::new()
}

async fn fetch_lyrics_api(track_name: &str, artist_name: &str) -> Result<Lyrics, anyhow::Error> {
    app_log(&format!("FETCH LYRICS INIT: {} - {}", track_name, artist_name));
    
    let clean_track = track_name.split(" - ").next().unwrap_or(track_name).to_string();
    let clean_artist = artist_name.split(',').next().unwrap_or(artist_name).to_string();
    
    let client = reqwest::Client::new();
    let url = format!("https://lrclib.net/api/get?track_name={}&artist_name={}", urlencoding::encode(&clean_track), urlencoding::encode(&clean_artist));
    let res = client.get(&url).header("User-Agent", "SpotMe/0.1.0").send().await?;
    
    if !res.status().is_success() {
        app_log(&format!("FETCH LYRICS FAULT {}: {}", res.status(), res.text().await.unwrap_or_default()));
        return Err(anyhow::anyhow!("Lyrics not found"));
    }
    
    let text = res.text().await?;
    app_log(&format!("FETCH LYRICS SUCCESS: {}", text.len()));
    let json: serde_json::Value = serde_json::from_str(&text)?;
    
    let plain = json["plainLyrics"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    
    let synced = json["syncedLyrics"].as_str().filter(|s| !s.is_empty()).map(|s| {
        let mut lines = Vec::new();
        for line in s.lines() {
            if line.starts_with('[') {
                if let Some(close_idx) = line.find(']') {
                    let ts = &line[1..close_idx];
                    let text = line[close_idx + 1..].trim().to_string();
                    
                    let parts: Vec<&str> = ts.split(':').collect();
                    if parts.len() == 2 {
                        let mins = parts[0].parse::<u64>().unwrap_or(0);
                        let secs_parts: Vec<&str> = parts[1].split('.').collect();
                        let secs = secs_parts[0].parse::<u64>().unwrap_or(0);
                        let ms = if secs_parts.len() == 2 {
                            let frac_str = format!("{:0<3}", secs_parts[1]);
                            frac_str[0..3].parse::<u64>().unwrap_or(0)
                        } else { 0 };
                        
                        let total_ms = (mins * 60 * 1000) + (secs * 1000) + ms;
                        lines.push(LrcLine { time_ms: total_ms, text });
                    }
                }
            }
        }
        lines
    });
    
    Ok(Lyrics { plain, synced })
}

async fn set_volume(token: &str, percent: u8) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    client.put(&format!("https://api.spotify.com/v1/me/player/volume?volume_percent={}", percent))
        .bearer_auth(token)
        .send().await?;
    Ok(())
}

fn jump_to_first_match(tracks: &[Track], state: &mut ListState, query: &str) {
    if query.is_empty() { return; }
    let q = query.to_lowercase();
    if let Some(pos) = tracks.iter().position(|t| t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.to_lowercase().contains(&q)) {
        state.select(Some(pos));
    }
}

fn jump_search_next(tracks: &[Track], state: &mut ListState, query: &str, forward: bool) {
    if query.is_empty() || tracks.is_empty() { return; }
    let q = query.to_lowercase();
    let current = state.selected().unwrap_or(0);
    let len = tracks.len();
    
    let iter: Vec<usize> = if forward {
        (current + 1..len).chain(0..current).collect()
    } else {
        (0..current).rev().chain((current + 1..len).rev()).collect()
    };
    
    for i in iter {
        let t = &tracks[i];
        if t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.to_lowercase().contains(&q) {
            state.select(Some(i));
            break;
        }
    }
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app_state: AppState) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMessage>();

    if let Some(ref ps) = app_state.player_state {
        if let Some(ref url) = ps.album_art_url {
            let art_tx = tx.clone();
            let art_url = url.clone();
            tokio::spawn(async move {
                let client = reqwest::Client::new();
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
                        let uri = track_obj["uri"].as_str().map(|s| s.to_string());
                        
                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(Some(PlayerState { 
                            track_uri: uri, track_name: name, artist, progress_ms: progress, duration_ms: duration, is_playing, volume_percent: volume, album_art_url: art_url.clone(), is_buffering: false, is_fresh_cache: false, lyrics: None
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
                            album_id: None,
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                        search_query: String::new(),
                        is_searching: false,
                    };
                }
                AppMessage::UpdatePlayerState(mut pstate) => {
                    let now = get_current_unix_time();
                    if pstate.is_none() {
                        if app_state.player_state.as_ref().map(|p| p.is_buffering).unwrap_or(false) {
                            continue;
                        }
                        if let Some(ref mut local_ps) = app_state.player_state {
                            local_ps.is_playing = false;
                        }
                        continue;
                    }
                    
                    let is_debounce_active = now.saturating_sub(app_state.last_action_timestamp) < 3;
                    
                    if is_debounce_active {
                        if let Some(ref local_ps) = app_state.player_state {
                            if let Some(ref incoming_ps) = pstate {
                                if incoming_ps.track_name != local_ps.track_name {
                                    continue; // Drop the lagging packet completely to prevent track name flashing!
                                }
                            }
                        }
                    }
                    
                    if is_debounce_active {
                        if let Some(ref local_ps) = app_state.player_state {
                            if let Some(ref mut incoming_ps) = pstate {
                                incoming_ps.is_playing = local_ps.is_playing;
                                incoming_ps.volume_percent = local_ps.volume_percent;
                                incoming_ps.progress_ms = local_ps.progress_ms;
                                incoming_ps.is_buffering = local_ps.is_buffering;
                            }
                        }
                    }
                    
                    if let Some(ref local_ps) = app_state.player_state {
                        if let Some(ref mut incoming_ps) = pstate {
                            if incoming_ps.track_name == local_ps.track_name {
                                incoming_ps.lyrics = local_ps.lyrics.clone();
                            }
                        }
                    }
                    
                    app_state.player_state = pstate;
                    
                    if let Some(ref mut ps) = app_state.player_state {
                        let mut cache_dirty = false;
                        let mut track_changed = false;
                        
                        if let Some(cached) = &mut app_state.app_cache.last_player {
                            if cached.track_name != ps.track_name {
                                track_changed = true;
                            }
                            
                            if track_changed || (ps.progress_ms as i64 - cached.progress_ms as i64).abs() > 5000 {
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
                                    let _ = tx.send(AppMessage::LyricsLoaded(Ok(Lyrics::default())));
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
                        
                        let thumbnail = dyn_img.resize_exact(1, 1, image::imageops::FilterType::Nearest);
                        let rgb = thumbnail.to_rgb8();
                        if let Some(pixel) = rgb.pixels().next() {
                            let p = pixel.0;
                            let dampen = 0.5; // Dampen brightness so text remains extremely legible
                            app_state.dominant_color = Some(((p[0] as f32 * dampen) as u8, (p[1] as f32 * dampen) as u8, (p[2] as f32 * dampen) as u8));
                        }
                    }
                }
                AppMessage::SearchResults(tracks) => {
                let mut list_state = ListState::default();
                if !tracks.is_empty() {
                    list_state.select(Some(0));
                }
                if let View::SearchGlobal { query: ref mut _query, tracks: ref mut t, state: ref mut s, ref mut is_typing } = app_state.current_view {
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
                if let View::SearchGlobal { query: ref mut _query, tracks: ref mut tracks, state: ref mut _s, ref mut is_typing } = app_state.current_view {
                    *tracks = Some(vec![Track { name: format!("Error: {}", err), artist: String::new(), album: String::new(), album_id: None, duration_ms: 0, uri: "".to_string() }]);
                    *is_typing = false;
                }
            }
            AppMessage::TrackAddedToPlaylist(playlist_id) => {
                app_log("TRIGGERED TrackAddedToPlaylist UI Popup Return Constraint!");
                
                // Discard stale offline cache for this playlist string targeting to force fresh syncs!
                app_state.app_cache.tracks.remove(&playlist_id);
                
                let mut prev = None;
                if let View::SelectPlaylist { ref mut previous, .. } = app_state.current_view {
                    prev = Some(std::mem::replace(previous, Box::new(View::Playlists)));
                }
                if let Some(p) = prev {
                    app_state.current_view = *p;
                }
            }
            AppMessage::QueueFetched(Ok(tracks)) => {
                let mut list_state = ListState::default();
                if !tracks.is_empty() { list_state.select(Some(0)); }
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
                    tracks: vec![Track { name: err, artist: String::new(), album: String::new(), album_id: None, duration_ms: 0, uri: "".to_string() }],
                    state: list_state,
                    search_query: String::new(),
                    is_searching: false,
                };
            }
            AppMessage::FeaturedFetched(lists) => {
                app_state.app_cache.playlists.extend(lists.clone());
                app_state.filtered_playlists = lists;
                app_state.playlist_state.select(if app_state.filtered_playlists.is_empty() { None } else { Some(0) });
                app_state.current_view = View::Playlists;
            }
            AppMessage::AlbumTracksFetched { album_name, tracks: Ok(tracks) } => {
                let mut list_state = ListState::default();
                if !tracks.is_empty() { list_state.select(Some(0)); }
                app_state.current_view = View::Tracks {
                    playlist_id: format!("album_{}", album_name),
                    playlist_name: album_name,
                    tracks,
                    state: list_state,
                    search_query: String::new(),
                    is_searching: false,
                };
            }
            AppMessage::AlbumTracksFetched { album_name, tracks: Err(err) } => {
                let mut list_state = ListState::default();
                list_state.select(Some(0));
                app_state.current_view = View::Tracks {
                    playlist_id: format!("album_err_{}", album_name),
                    playlist_name: format!("Error: {}", album_name),
                    tracks: vec![Track { name: err, artist: String::new(), album: String::new(), album_id: None, duration_ms: 0, uri: "".to_string() }],
                    state: list_state,
                    search_query: String::new(),
                    is_searching: false,
                };
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
        if let View::LoadingTracks { ref mut spinner_tick } = app_state.current_view {
            *spinner_tick = spinner_tick.wrapping_add(1);
        }
        if app_state.player_state.as_ref().map(|p| p.is_buffering).unwrap_or(false) {
            app_state.player_spinner_tick = app_state.player_spinner_tick.wrapping_add(1);
        }

        // Draw UI
        terminal.draw(|f| ui(f, &mut app_state)).map_err(|_| anyhow::anyhow!("TUI draw error"))?;

        // GUI IO Polling logic
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if app_state.show_help {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                        app_state.show_help = false;
                    }
                    continue; // Lock view events
                }
                
                let mut is_typing = false;
                if let View::Tracks { is_searching, .. } = &app_state.current_view {
                    is_typing = *is_searching;
                }
                if let View::SearchGlobal { is_typing: st, .. } = &app_state.current_view {
                    is_typing = *st;
                }
                if !is_typing {
                    if app_state.fullscreen_player && app_state.lyrics_mode == LyricsMode::Full {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                app_state.lyrics_scroll_offset = app_state.lyrics_scroll_offset.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                app_state.lyrics_scroll_offset = app_state.lyrics_scroll_offset.saturating_add(1);
                            }
                            _ => {}
                        }
                    }

                    // Global Playback Hotkeys!
                    match key.code {
                    KeyCode::Char(' ') => {
                        if let Some(player) = &mut app_state.player_state {
                            let token = app_state.access_token.clone();
                            let is_playing = player.is_playing;
                            
                            if is_playing { 
                                if let Some(tx) = &app_state.local_cmd_tx {
                                    let _ = tx.try_send(LocalPlayerCommand::Pause);
                                } else {
                                    tokio::spawn(async move { let _ = pause_playback(&token).await; });
                                }
                            } else { 
                                if player.is_fresh_cache {
                                    player.is_fresh_cache = false;
                                    let prog = player.progress_ms;
                                    if let Some(uri) = player.track_uri.clone() {
                                        tokio::spawn(async move { let _ = play_track(&token, &uri, prog).await; });
                                    } else {
                                        let t_name = player.track_name.clone();
                                        let a_name = player.artist.clone();
                                        tokio::spawn(async move {
                                            if let Ok(tracks) = search_spotify_api(&token, &format!("{} {}", t_name, a_name)).await {
                                                if let Some(first) = tracks.first() {
                                                    let _ = play_track(&token, &first.uri, prog).await;
                                                }
                                            }
                                        });
                                    }
                                } else if let Some(tx) = &app_state.local_cmd_tx {
                                    let _ = tx.try_send(LocalPlayerCommand::Play);
                                } else {
                                    tokio::spawn(async move { let _ = resume_playback(&token).await; });
                                }
                            }
                            player.is_playing = !is_playing;
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
                    KeyCode::Char('l') | KeyCode::Char('L') => { 
                        if app_state.fullscreen_player {
                            app_state.lyrics_mode = match app_state.lyrics_mode {
                                LyricsMode::Focused => LyricsMode::Full,
                                LyricsMode::Full => LyricsMode::Focused,
                            };
                        } else if let Some(player) = &app_state.player_state {
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
                    KeyCode::Char('?') => {
                        app_state.show_help = true;
                    }
                    KeyCode::Char('n') => {
                        let token = app_state.access_token.clone();
                        tokio::spawn(async move { let _ = next_track(&token).await; });
                    }
                    KeyCode::Char('p') => {
                        let token = app_state.access_token.clone();
                        tokio::spawn(async move { let _ = previous_track(&token).await; });
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
                    KeyCode::Char('v') | KeyCode::Char('f') => {
                        app_state.fullscreen_player = !app_state.fullscreen_player;
                    }
                    KeyCode::Tab => {
                        app_state.show_popup = !app_state.show_popup;
                    }
                    KeyCode::Char('w') | KeyCode::Char('e') => {
                        let token = app_state.access_token.clone();
                        let tx = tx.clone();
                        app_state.show_popup = true;
                        app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                        tokio::spawn(async move {
                            match fetch_player_queue(&token).await {
                                Ok(tracks) => { let _ = tx.send(AppMessage::QueueFetched(Ok(tracks))); }
                                Err(e) => { let _ = tx.send(AppMessage::QueueFetched(Err(e))); }
                            }
                        });
                    }
                    _ => {}
                }
                app_state.last_action_timestamp = get_current_unix_time();
                } // End !is_typing

                // View-specific events
                match app_state.current_view {
                    View::Playlists => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('c') => {
                                app_state.player_state = None;
                                app_state.current_art_protocol = None;
                                app_state.current_art_bytes = None;
                                app_state.current_art_url = None;
                                app_state.app_cache.last_player = None;
                                save_cache(&app_state.app_cache);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let i = match app_state.playlist_state.selected() {
                                    Some(i) => if i >= app_state.filtered_playlists.len().saturating_sub(1) { 0 } else { i + 1 },
                                    None => 0,
                                };
                                app_state.playlist_state.select(Some(i));
                            }
                            KeyCode::Char('s') => {
                                app_state.show_popup = true;
                                app_state.current_view = View::SearchGlobal { query: String::new(), tracks: None, state: ListState::default(), is_typing: true };
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
                            KeyCode::Char('b') => {
                                let token = app_state.access_token.clone();
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let lists = fetch_featured_playlists_api(&token).await;
                                    let _ = tx.send(AppMessage::FeaturedFetched(lists));
                                });
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
                        
                        if *is_searching {
                            match key.code {
                                KeyCode::Enter => {
                                    *is_searching = false;
                                }
                                KeyCode::Esc => {
                                    *is_searching = false;
                                    search_query.clear();
                                }
                                KeyCode::Backspace => {
                                    search_query.pop();
                                    jump_to_first_match(tracks, state, search_query);
                                }
                                KeyCode::Char(c) => {
                                    search_query.push(c);
                                    jump_to_first_match(tracks, state, search_query);
                                }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('/') => {
                                    app_state.show_popup = true;
                                    *is_searching = true;
                                    search_query.clear();
                                }
                                KeyCode::Char('n') => {
                                    jump_search_next(tracks, state, search_query, true);
                                }
                                KeyCode::Char('N') => {
                                    jump_search_next(tracks, state, search_query, false);
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
                                KeyCode::Char('A') => {
                                    if let Some(i) = state.selected() {
                                        if i < tracks.len() {
                                            if let Some(ref aid) = tracks[i].album_id {
                                                let token = app_state.access_token.clone();
                                                let tx = tx.clone();
                                                let a_name = tracks[i].album.clone();
                                                let a_id = aid.clone();
                                                app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                                                tokio::spawn(async move {
                                                    match fetch_album_tracks(&token, &a_id).await {
                                                        Ok(tracks) => { let _ = tx.send(AppMessage::AlbumTracksFetched { album_name: a_name, tracks: Ok(tracks) }); }
                                                        Err(e) => { let _ = tx.send(AppMessage::AlbumTracksFetched { album_name: a_name, tracks: Err(e) }); }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char('a') => {
                                    if let Some(i) = state.selected() {
                                        if i < tracks.len() {
                                            let track = tracks[i].clone();
                                            let mut dummy = View::Playlists;
                                            std::mem::swap(&mut app_state.current_view, &mut dummy);
                                            app_state.current_view = View::SelectPlaylist {
                                                track_uri: track.uri,
                                                track_name: track.name,
                                                state: ListState::default().with_selected(if app_state.filtered_playlists.is_empty() { None } else { Some(0) }),
                                                previous: Box::new(dummy),
                                            };
                                        }
                                    }
                                }
                                KeyCode::Char('c') => {
                                    app_state.player_state = None;
                                    app_state.current_art_protocol = None;
                                    app_state.current_art_bytes = None;
                                    app_state.current_art_url = None;
                                    app_state.app_cache.last_player = None;
                                    save_cache(&app_state.app_cache);
                                }
                                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('b') => {
                                    app_state.current_view = View::Playlists;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let i = match state.selected() {
                                        Some(i) => if i >= tracks.len().saturating_sub(1) { 0 } else { i + 1 },
                                        None => 0,
                                    };
                                    state.select(Some(i));
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let i = match state.selected() {
                                        Some(i) => if i == 0 { tracks.len().saturating_sub(1) } else { i - 1 },
                                        None => 0,
                                    };
                                    state.select(Some(i));
                                }
                                KeyCode::Enter => {
                                    if let Some(i) = state.selected() {
                                        if i < tracks.len() {
                                            app_state.show_popup = false;
                                            let token = app_state.access_token.clone();
                                            let track = tracks[i].clone();
                                            let uri = track.uri.clone();
                                            if !uri.is_empty() {
                                                // Optimistic instant feedback
                                                let current_vol = app_state.player_state.as_ref().map(|p| p.volume_percent).unwrap_or(50);
                                                app_state.player_state = Some(PlayerState {
                                                    track_uri: Some(uri.clone()),
                                                    track_name: track.name.clone(),
                                                    artist: track.artist.clone(),
                                                    progress_ms: 0,
                                                    duration_ms: track.duration_ms as u64,
                                                    is_playing: true,
                                                    volume_percent: current_vol,
                                                    album_art_url: None,
                                                    is_buffering: true,
                                                    is_fresh_cache: false,
                                                    lyrics: None,
                                                });
                                                app_state.last_action_timestamp = get_current_unix_time();

                                                tokio::spawn(async move {
                                                    let _ = play_track(&token, &uri, 0).await;
                                                });
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    View::SearchGlobal { ref mut query, ref mut tracks, ref mut state, ref mut is_typing } => {
                        if *is_typing {
                            match key.code {
                                KeyCode::Enter => {
                                    if !query.trim().is_empty() {
                                        *is_typing = false;
                                        let token = app_state.access_token.clone();
                                        let tx = tx.clone();
                                        let q = query.clone();
                                        tokio::spawn(async move {
                                            match search_spotify_api(&token, &q).await {
                                                Ok(results) => { let _ = tx.send(AppMessage::SearchResults(results)); }
                                                Err(e) => { let _ = tx.send(AppMessage::SearchError(e)); }
                                            }
                                        });
                                    } else {
                                        *is_typing = false;
                                    }
                                }
                                KeyCode::Esc => {
                                    app_state.current_view = View::Playlists;
                                }
                                KeyCode::Backspace => { query.pop(); }
                                KeyCode::Char(c) => { query.push(c); }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('b') => {
                                    app_state.current_view = View::Playlists;
                                }
                                KeyCode::Char('/') | KeyCode::Char('s') => {
                                    *is_typing = true;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if let Some(t) = tracks {
                                        let i = match state.selected() {
                                            Some(i) => if i >= t.len().saturating_sub(1) { 0 } else { i + 1 },
                                            None => 0,
                                        };
                                        state.select(Some(i));
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if let Some(t) = tracks {
                                        let i = match state.selected() {
                                            Some(i) => if i == 0 { t.len().saturating_sub(1) } else { i - 1 },
                                            None => 0,
                                        };
                                        state.select(Some(i));
                                    }
                                }
                                KeyCode::Char('a') => {
                                    if let Some(t) = tracks {
                                        if let Some(i) = state.selected() {
                                            if i < t.len() {
                                                let track = t[i].clone();
                                                let mut dummy = View::Playlists;
                                                std::mem::swap(&mut app_state.current_view, &mut dummy);
                                                app_state.current_view = View::SelectPlaylist {
                                                    track_uri: track.uri,
                                                    track_name: track.name,
                                                    state: ListState::default().with_selected(if app_state.filtered_playlists.is_empty() { None } else { Some(0) }),
                                                    previous: Box::new(dummy),
                                                };
                                            }
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some(t) = tracks {
                                        if let Some(i) = state.selected() {
                                            if i < t.len() {
                                                app_state.show_popup = false;
                                                let token = app_state.access_token.clone();
                                                let track = t[i].clone();
                                                let uri = track.uri.clone();
                                                if !uri.is_empty() {
                                                    let current_vol = app_state.player_state.as_ref().map(|p| p.volume_percent).unwrap_or(50);
                                                    app_state.player_state = Some(PlayerState {
                                                        track_uri: Some(uri.clone()),
                                                        track_name: track.name.clone(),
                                                        artist: track.artist.clone(),
                                                        progress_ms: 0,
                                                        duration_ms: track.duration_ms as u64,
                                                        is_playing: true,
                                                        volume_percent: current_vol,
                                                        album_art_url: None,
                                                        is_buffering: true,
                                                        is_fresh_cache: false,
                                                        lyrics: None,
                                                    });
                                                    app_state.last_action_timestamp = get_current_unix_time();
                                                    tokio::spawn(async move { let _ = play_track(&token, &uri, 0).await; });
                                                }
                                            }
                                        }
                                    }
                                }

                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Char('i') => {
                                    app_state.fullscreen_player = !app_state.fullscreen_player;
                                }
                                KeyCode::Char('c') => {
                                    app_state.player_state = None;
                                    app_state.current_art_protocol = None;
                                    app_state.current_art_bytes = None;
                                    app_state.current_art_url = None;
                                    app_state.app_cache.last_player = None;
                                    save_cache(&app_state.app_cache);
                                }
                                _ => {}
                            }
                        }
                    }
                    View::SelectPlaylist { ref track_uri, track_name: _, ref mut state, previous: _ } => {
                        match key.code {
                            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('b') => {
                                let mut dummy = View::Playlists;
                                if let View::SelectPlaylist { mut previous, .. } = app_state.current_view {
                                    std::mem::swap(&mut dummy, &mut previous);
                                    app_state.current_view = dummy;
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let i = match state.selected() {
                                    Some(i) => if i >= app_state.filtered_playlists.len().saturating_sub(1) { 0 } else { i + 1 },
                                    None => 0,
                                };
                                state.select(Some(i));
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                let i = match state.selected() {
                                    Some(i) => if i == 0 { app_state.filtered_playlists.len().saturating_sub(1) } else { i - 1 },
                                    None => 0,
                                };
                                state.select(Some(i));
                            }
                            KeyCode::Enter => {
                                if let Some(i) = state.selected() {
                                    if i < app_state.filtered_playlists.len() {
                                        let target_list = app_state.filtered_playlists[i].id.clone();
                                        let uri = track_uri.clone();
                                        let token = app_state.access_token.clone();
                                        let tx = tx.clone();
                                        tokio::spawn(async move {
                                            let _ = add_track_to_playlist_api(&token, &target_list, &uri).await;
                                            let _ = tx.send(AppMessage::TrackAddedToPlaylist(target_list));
                                        });
                                    }
                                }
                            }
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('i') => {
                                app_state.fullscreen_player = !app_state.fullscreen_player;
                            }
                            KeyCode::Char('c') => {
                                app_state.player_state = None;
                                app_state.current_art_protocol = None;
                                app_state.current_art_bytes = None;
                                app_state.current_art_url = None;
                                app_state.app_cache.last_player = None;
                                save_cache(&app_state.app_cache);
                            }
                            _ => {}
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
    if state.fullscreen_player {
        if let Some((r, g, b)) = state.dominant_color {
            f.render_widget(GradientBackground { dominant: (r, g, b) }, f.area());
        }
    }

    let is_vim_cmd = match state.current_view {
        View::Tracks { is_searching, ref search_query, .. } => is_searching || !search_query.is_empty(),
        View::SearchGlobal { is_typing, ref query, .. } => is_typing || !query.is_empty(),
        _ => false,
    };

    let (top, mid, cmd, bot) = if state.fullscreen_player {
        (0_u16, 0_u16, 0_u16, f.area().height.saturating_sub(4))
    } else {
        (3_u16, 1_u16, if is_vim_cmd { 1 } else { 0 }, if state.player_state.is_some() { 8 } else { 3 })
    };
    
    let constraints = if state.fullscreen_player {
        vec![Constraint::Length(0), Constraint::Length(0), Constraint::Min(1), Constraint::Length(0)]
    } else {
        vec![Constraint::Length(top), Constraint::Min(mid), Constraint::Length(bot), Constraint::Length(cmd)]
    };
    
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(constraints)
        .split(f.area());

    let mut render_view = !state.fullscreen_player;
    let mut actual_view_chunk = chunks[1];
    let mut actual_cmd_chunk = if chunks.len() > 3 { chunks[3] } else { chunks[1] };

    if state.fullscreen_player && state.show_popup {
        render_view = true;
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(10), Constraint::Percentage(80), Constraint::Percentage(10)])
            .split(f.area());
            
        let popup_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(15), Constraint::Percentage(70), Constraint::Percentage(15)])
            .split(popup_layout[1])[1];
            
        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(if is_vim_cmd { 1 } else { 0 })])
            .split(popup_area);
            
        actual_view_chunk = inner_chunks[0];
        actual_cmd_chunk = inner_chunks[1];
        
        // Strip natively via clear
        f.render_widget(ratatui::widgets::Clear, popup_area);
        // Paint a light translucent background natively mapped via Empty Block
        f.render_widget(Block::default().style(Style::default().bg(ratatui::style::Color::Black)), popup_area);
    }

    if !state.fullscreen_player {
        // Top banner
        let nav_hint = match state.current_view {
            View::Playlists => "(↑/↓ Nav, +/- Vol, s Search, o Others, b Featured, e Queue, r Refresh, Enter View, i Mode, q Quit)",
            View::Tracks { is_searching, .. } => {
                if is_searching { "(Type to search, Enter/Esc to exit search)" }
                else { "(↑/↓ Nav, +/- Vol, / Search, A Album, a Add, e Queue, Esc Edit, Enter PLAY, i Mode, q Quit)" }
            }
            View::SearchGlobal { is_typing, .. } => {
                if is_typing { "(Type to search... Enter to search, Esc Back)" }
                else { "(↑/↓ Nav, Enter PLAY, a Add Playlist, s Search, Esc Back)" }
            }
            View::SelectPlaylist { .. } => "(↑/↓ Nav, Enter Select, Esc Back)",
            View::LoadingTracks { .. } => "(Loading...)",
        };
        
        let welcome_msg = format!("SpotMe Client - Welcome, {}! {}", state.display_name, nav_hint);
        let banner = Paragraph::new(welcome_msg)
            .block(Block::default().borders(Borders::ALL).title("User Info"))
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(banner, chunks[0]);
    }

    if render_view {
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

                f.render_stateful_widget(playlist_list, actual_view_chunk, &mut state.playlist_state);
            }
            View::LoadingTracks { spinner_tick } => {
                let spinner = vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (*spinner_tick as usize) % spinner.len();
                
                let p = Paragraph::new(format!("{} Loading tracks...", spinner[idx]))
                    .block(Block::default().borders(Borders::ALL).title("Loading"))
                    .style(Style::default().fg(Color::Yellow));
                    
                f.render_widget(p, actual_view_chunk);
            }
            View::Tracks { playlist_id: _, playlist_name, tracks, state: list_state, search_query, is_searching } => {
                let items: Vec<ListItem> = tracks
                    .iter()
                    .map(|t| {
                        let metadata = format!("{} | {} ({})", t.artist, t.album, format_duration(t.duration_ms));
                        let line1 = Line::from(Span::styled(t.name.clone(), Style::default().add_modifier(Modifier::BOLD)));
                        let line2 = Line::from(Span::styled(metadata, Style::default().fg(Color::DarkGray)));
                        ListItem::new(vec![line1, line2])
                    })
                    .collect();

                let title = format!("Tracks in {}", playlist_name);

                let tracks_list = List::new(items)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().bg(Color::Magenta).fg(Color::Black))
                    .highlight_symbol(">> ");

                f.render_stateful_widget(tracks_list, actual_view_chunk, list_state);
                
                // Render vim command bar
                if *is_searching || !search_query.is_empty() {
                    let cursor = if *is_searching { "█" } else { "" };
                    let cmd_text = format!("/{}{}", search_query, cursor);
                    let p = Paragraph::new(cmd_text).style(Style::default().fg(Color::Yellow));
                    f.render_widget(p, actual_cmd_chunk);
                }
            }
            View::SearchGlobal { query, tracks, state: list_state, is_typing } => {
                let title = if *is_typing { "Global Search (Typing...)" } else { "Global Search" };
                let display_text = if let Some(t) = tracks {
                    if t.is_empty() {
                        vec![ListItem::new("No results found.")]
                    } else {
                        t.iter()
                        .map(|tr| {
                            let metadata = format!("{} | {} ({})", tr.artist, tr.album, format_duration(tr.duration_ms));
                            let line1 = Line::from(Span::styled(tr.name.clone(), Style::default().add_modifier(Modifier::BOLD)));
                            let line2 = Line::from(Span::styled(metadata, Style::default().fg(Color::DarkGray)));
                            ListItem::new(vec![line1, line2])
                        })
                        .collect()
                    }
                } else {
                    vec![ListItem::new("Enter a query to search Spotify network...")]
                };

                let tracks_list = List::new(display_text)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black).add_modifier(Modifier::BOLD))
                    .highlight_symbol(">> ");

                f.render_stateful_widget(tracks_list, actual_view_chunk, list_state);

                if *is_typing || !query.is_empty() {
                    let cursor = if *is_typing { "█" } else { "" };
                    let cmd_text = format!("Search: {}{}", query, cursor);
                    let p = Paragraph::new(cmd_text).style(Style::default().fg(Color::Yellow));
                    f.render_widget(p, actual_cmd_chunk);
                }
            }
            View::SelectPlaylist { track_name, state: list_state, .. } => {
                let items: Vec<ListItem> = state.filtered_playlists
                    .iter()
                    .map(|p| ListItem::new(p.name.clone()))
                    .collect();

                let title = format!("Add '{}' to Playlist", track_name);
                let playlist_list = List::new(items)
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::White))
                    .highlight_style(Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD))
                    .highlight_symbol(">> ");

                f.render_stateful_widget(playlist_list, actual_view_chunk, list_state);
            }
        }
    }

    // Bottom Player Box
    let player_block = Block::default().borders(Borders::ALL);
    let pdx = 2; // Fixed index now that cmd is at the end
    let inner_area = player_block.inner(chunks[pdx]);
    f.render_widget(player_block, chunks[pdx]);
    
    if let Some(player) = &state.player_state {
        let has_lyrics = if let Some(lyrics) = &player.lyrics {
            lyrics.synced.is_some() || lyrics.plain.is_some()
        } else {
            false
        };

        let (player_area, lyrics_area) = if state.fullscreen_player && has_lyrics {
            let v_split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(inner_area);
            (v_split[0], Some(v_split[1]))
        } else {
            (inner_area, None)
        };

        let h_split_constraints = if state.fullscreen_player {
            vec![Constraint::Percentage(50), Constraint::Percentage(50)]
        } else {
            vec![Constraint::Length(16), Constraint::Min(0)]
        };
        
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(h_split_constraints)
            .split(player_area);
        let sub_chunks = split.to_vec();
        
        if let Some(protocol) = state.current_art_protocol.as_mut() {
            let img_widget = StatefulImage::default();
            f.render_stateful_widget(img_widget, sub_chunks[0], protocol);
        } else {
            let placeholder = Paragraph::new("\n\n ░░░░░░\n NO ART\n ░░░░░░")
                .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD))
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(placeholder, sub_chunks[0]);
        }
        
        let target_area = sub_chunks[1];
        
        let v_split_constraints = if state.fullscreen_player {
            vec![
                Constraint::Percentage(35),
                Constraint::Length(1), // Track Name
                Constraint::Length(1), // Artist
                Constraint::Length(2), // Fixed padding
                Constraint::Length(1), // Gauge
                Constraint::Length(1), // Status
                Constraint::Percentage(35),
            ]
        } else {
            vec![
                Constraint::Min(1),    // Top pad
                Constraint::Length(1), // Track Name
                Constraint::Length(1), // Artist
                Constraint::Length(1), // Fixed padding
                Constraint::Length(1), // Gauge
                Constraint::Length(1), // Status
            ]
        };
        
        let detail_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(v_split_constraints)
            .split(target_area);
            
        let align = if state.fullscreen_player { ratatui::layout::Alignment::Center } else { ratatui::layout::Alignment::Left };

        let track_name = Paragraph::new(Line::from(vec![
            Span::styled(player.track_name.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
        ])).alignment(align);
        
        let artist_name = Paragraph::new(Line::from(vec![
            Span::styled(player.artist.to_uppercase(), Style::default().fg(Color::DarkGray))
        ])).alignment(align);
        
        let status = if player.is_buffering { 
            let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let idx = (state.player_spinner_tick as usize) % spinners.len();
            spinners[idx].to_string()
        } else if player.is_playing { 
            "⏵".to_string() 
        } else { 
            "⏸".to_string() 
        };
        
        let total_vol_blocks = 8;
        let filled_vol = ((player.volume_percent as u32 * total_vol_blocks) / 100) as usize;
        let filled_vol = filled_vol.min(total_vol_blocks as usize);
        let empty_vol = (total_vol_blocks as usize).saturating_sub(filled_vol);
        let vol_bar = format!("{}{}", "▰".repeat(filled_vol), "▱".repeat(empty_vol));
        
        let status_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(20)])
            .split(detail_chunks[5]);
            
        let mut status_str = format!("{}   {} / {}", 
            status, format_duration(player.progress_ms), format_duration(player.duration_ms));
            
        if state.fullscreen_player && !has_lyrics {
            if player.lyrics.is_none() {
                let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (state.player_spinner_tick as usize) % spinners.len();
                status_str.push_str(&format!("  |  Fetching lyrics... {}", spinners[idx]));
            } else {
                status_str.push_str("  |  No lyrics found");
            }
        }
            
        let left_status = Paragraph::new(status_str).style(Style::default().fg(Color::Gray));
            
        let right_status = Paragraph::new(format!("VOL {} {:3}%", vol_bar, player.volume_percent))
            .style(Style::default().fg(Color::Gray))
            .alignment(ratatui::layout::Alignment::Right);
            
        let mut progress_ratio = 0.0;
        if player.duration_ms > 0 {
            progress_ratio = (player.progress_ms as f64 / player.duration_ms as f64).clamp(0.0, 1.0);
        }
        
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
            .ratio(progress_ratio);
            
        f.render_widget(track_name, detail_chunks[1]);
        f.render_widget(artist_name, detail_chunks[2]);
        f.render_widget(gauge, detail_chunks[4]);
        f.render_widget(left_status, status_split[0]);
        f.render_widget(right_status, status_split[1]);
        
        if state.fullscreen_player && has_lyrics {
            let lyrics_chunk = lyrics_area.unwrap();
            let lyrics_block = Block::default()
                .borders(Borders::NONE)
                .padding(ratatui::widgets::Padding::new(0, 0, 1, 0));
                
            let inner_lyrics_area = lyrics_block.inner(lyrics_chunk);
            f.render_widget(lyrics_block, lyrics_chunk);
            
            if let Some(lyrics) = &player.lyrics {
                if let Some(synced) = &lyrics.synced {
                    let mut active_idx = 0;
                    for (i, line) in synced.iter().enumerate() {
                        if player.progress_ms >= line.time_ms {
                            active_idx = i;
                        } else {
                            break;
                        }
                    }
                    
                    let mut lyric_spans = Vec::new();
                    
                    if state.lyrics_mode == LyricsMode::Focused {
                        let pad_top = inner_lyrics_area.height.saturating_sub(5) / 2;
                        for _ in 0..pad_top {
                            lyric_spans.push(Line::from(vec![Span::raw(" ")]));
                        }
                        
                        let prev_idx2 = active_idx.saturating_sub(2);
                        if prev_idx2 < active_idx && active_idx >= 2 {
                            let text = &synced[prev_idx2].text;
                            lyric_spans.push(Line::from(vec![Span::styled(if text.is_empty() { " " } else { text } .to_string(), Style::default().fg(Color::DarkGray))]));
                        }
                        
                        let prev_idx = active_idx.saturating_sub(1);
                        if prev_idx < active_idx && active_idx >= 1 {
                            let text = &synced[prev_idx].text;
                            lyric_spans.push(Line::from(vec![Span::styled(if text.is_empty() { " " } else { text } .to_string(), Style::default().fg(Color::Gray))]));
                        }
                        
                        let text = &synced[active_idx].text;
                        lyric_spans.push(Line::from(vec![Span::styled(if text.is_empty() { " " } else { text } .to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]));
                        
                        let next_idx = active_idx.saturating_add(1);
                        if next_idx < synced.len() {
                            let text = &synced[next_idx].text;
                            lyric_spans.push(Line::from(vec![Span::styled(if text.is_empty() { " " } else { text } .to_string(), Style::default().fg(Color::Gray))]));
                        }
                        
                        let next_idx2 = active_idx.saturating_add(2);
                        if next_idx2 < synced.len() {
                            let text = &synced[next_idx2].text;
                            lyric_spans.push(Line::from(vec![Span::styled(if text.is_empty() { " " } else { text } .to_string(), Style::default().fg(Color::DarkGray))]));
                        }
                    } else {
                        let visible_lines = inner_lyrics_area.height as usize;
                        let max_scroll = synced.len().saturating_sub(visible_lines);
                        let start_idx = state.lyrics_scroll_offset.min(max_scroll);
                        
                        for i in start_idx..(start_idx + visible_lines).min(synced.len()) {
                            let text = &synced[i].text;
                            if text.is_empty() {
                                lyric_spans.push(Line::from(vec![Span::raw(" ")]));
                                continue;
                            }
                            if i == active_idx {
                                lyric_spans.push(Line::from(vec![Span::styled(text.clone(), Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD).add_modifier(Modifier::UNDERLINED))]));
                            } else {
                                lyric_spans.push(Line::from(vec![Span::styled(text.clone(), Style::default().fg(Color::Gray))]));
                            }
                        }
                    }
                    
                    let p = Paragraph::new(lyric_spans).alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(p, inner_lyrics_area);
                } else if let Some(plain) = &lyrics.plain {
                    let text_lines: Vec<&str> = plain.lines().collect();
                    let visible_lines = inner_lyrics_area.height as usize;
                    let max_scroll = text_lines.len().saturating_sub(visible_lines);
                    let start_idx = state.lyrics_scroll_offset.min(max_scroll);
                    
                    let mut lyric_spans = Vec::new();
                    for i in start_idx..(start_idx + visible_lines).min(text_lines.len()) {
                        lyric_spans.push(Line::from(vec![Span::styled(text_lines[i].to_string(), Style::default().fg(Color::Gray))]));
                    }
                    
                    let p = Paragraph::new(lyric_spans)
                        .alignment(ratatui::layout::Alignment::Center)
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    f.render_widget(p, inner_lyrics_area);
                } else {
                    let p = Paragraph::new("Lyrics not physically found on LRCLIB.")
                        .style(Style::default().fg(Color::DarkGray))
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(p, inner_lyrics_area);
                }
            } else {
                let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = (state.player_spinner_tick as usize) % spinners.len();
                let txt = format!("Fetching lyrics... {}", spinners[idx]);
                let p = Paragraph::new(txt)
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(ratatui::layout::Alignment::Center);
                f.render_widget(p, inner_lyrics_area);
            }
        }
    } else {
        let text = Paragraph::new("\n  No track currently playing. Select a track and press Enter to begin playback.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, inner_area);
    }

    if state.show_help {
        let help_text = vec![
            Line::from(vec![Span::styled(" SpotMe Shortcuts ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))]),
            Line::from(""),
            Line::from(vec![Span::styled("Global / Playback", Style::default().add_modifier(Modifier::UNDERLINED))]),
            Line::from("  Space      : Play/Pause"),
            Line::from("  n / p      : Next / Previous Track"),
            Line::from("  ← / →      : Seek -5s / +5s"),
            Line::from("  h / l      : Seek -15s / +15s (Or toggle Fullscreen Lyrics via 'l')"),
            Line::from("  + / -      : Volume Up / Down"),
            Line::from("  ?          : Toggle this Help Menu"),
            Line::from("  i          : Cycle Image Renderer Protocol"),
            Line::from(""),
            Line::from(vec![Span::styled("Navigation", Style::default().add_modifier(Modifier::UNDERLINED))]),
            Line::from("  ↑/↓ & j/k  : Navigate Lists / Scroll Fullscreen Lyrics"),
            Line::from("  Enter      : Select / Play"),
            Line::from("  Esc        : Go Back / Cancel Prompts"),
            Line::from("  s          : Search Global"),
            Line::from("  o          : Switch to Playlist Views"),
            Line::from("  f          : Fullscreen Player Toggle"),
            Line::from(""),
            Line::from(vec![Span::styled("Press Esc or ? to close.", Style::default().fg(Color::DarkGray))]),
        ];

        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(15), Constraint::Percentage(70), Constraint::Percentage(15)])
            .split(f.area());
            
        let popup_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(50), Constraint::Percentage(25)])
            .split(popup_layout[1])[1];

        // Explicitly clear background behind the popup so it doesn't mesh with the UI!
        f.render_widget(ratatui::widgets::Clear, popup_area);

        let p = Paragraph::new(help_text)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow)).style(Style::default().bg(Color::Reset)))
            .alignment(ratatui::layout::Alignment::Left);
        
        f.render_widget(p, popup_area);
    }
}
