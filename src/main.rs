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
use rspotify::{prelude::*, AuthCodeSpotify, Config, Credentials, OAuth};
use serde_json::Value;
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

// Models
#[derive(Clone)]
struct Playlist {
    id: String,
    name: String,
}

#[derive(Clone)]
struct Track {
    name: String,
    artist: String,
    album: String,
    duration_ms: u64,
    uri: String,
}

#[derive(Clone)]
struct PlayerState {
    track_name: String,
    artist: String,
    progress_ms: u64,
    duration_ms: u64,
    is_playing: bool,
}

// GUI State
enum View {
    Playlists,
    LoadingTracks { spinner_tick: u8 },
    Tracks { playlist_name: String, tracks: Vec<Track>, state: ListState },
}

struct AppState {
    display_name: String,
    playlists: Vec<Playlist>,
    playlist_state: ListState,
    current_view: View,
    access_token: String,
    player_state: Option<PlayerState>,
}

// Async Message passing
enum AppMessage {
    TracksFetched { playlist_name: String, tracks: Vec<Track> },
    FetchError(String),
    UpdatePlayerState(Option<PlayerState>),
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let rem_secs = secs % 60;
    format!("{}:{:02}", mins, rem_secs)
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

    let creds = rspotify::Credentials::new(&client_id, &client_secret);
    let oauth = OAuth {
        redirect_uri,
        scopes: rspotify::scopes!(
            "user-read-private", 
            "user-read-email",
            "playlist-read-private", 
            "playlist-read-collaborative",
            "user-modify-playback-state",
            "user-read-playback-state",
            "streaming"
        ),
        ..Default::default()
    };
    
    let config = Config { token_cached: true, ..Default::default() };
    let spotify = AuthCodeSpotify::with_config(creds, oauth, config);

    let auth_url = spotify.get_authorize_url(false)?;
    spotify.prompt_for_token(&auth_url).await?;
    
    let user_info = spotify.current_user().await?;
    let display_name = user_info.display_name.unwrap_or_else(|| "Unknown".to_string());

    // Recover token
    let mut access_token = String::new();
    if let Ok(cache_content) = std::fs::read_to_string(".spotify_token_cache.json") {
        if let Ok(cache_json) = serde_json::from_str::<serde_json::Value>(&cache_content) {
            if let Some(token) = cache_json["access_token"].as_str() {
                access_token = token.to_string();
            }
        }
    }

    // Launch standalone librespot headless local player using our auth token
    if !access_token.is_empty() {
        let t = access_token.clone();
        tokio::spawn(async move {
            if let Err(e) = start_librespot_daemon(t).await {
                let _ = std::fs::write("/tmp/spotme.log", format!("Librespot error: {}", e));
            }
        });
    }

    // Give the local librespot daemon a second to register with Spotify clouds before we start asking for playlists
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    // Fetch Playlists using raw reqwest
    let mut playlists = Vec::new();
    let client = reqwest::Client::new();
    if !access_token.is_empty() {
        if let Ok(res) = client.get("https://api.spotify.com/v1/me/playlists")
            .bearer_auth(&access_token)
            .send()
            .await 
        {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                if let Some(items) = json["items"].as_array() {
                    for item in items {
                        if let (Some(name), Some(id)) = (item["name"].as_str(), item["id"].as_str()) {
                            playlists.push(Playlist { name: name.to_string(), id: id.to_string() });
                        }
                    }
                }
            }
        }
    }

    let mut playlist_state = ListState::default();
    if !playlists.is_empty() {
        playlist_state.select(Some(0)); // Start with first selected
    }

    let app_state = AppState {
        display_name,
        playlists,
        playlist_state,
        current_view: View::Playlists,
        access_token,
        player_state: None,
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
async fn fetch_tracks(token: String, playlist_id: String) -> Result<Vec<Track>, anyhow::Error> {
    let client = reqwest::Client::new();
    let url = format!("https://api.spotify.com/v1/playlists/{}/items?market=from_token", playlist_id);
    let res = client.get(&url).bearer_auth(token).send().await?;
    let raw_text = res.text().await?;
    let json: Value = serde_json::from_str(&raw_text)?;
    
    let mut tracks = Vec::new();
    if let Some(items) = json["items"].as_array() {
        if items.is_empty() {
            return Err(anyhow::anyhow!("API answered with 0 items."));
        }
        for item in items {
            let mut track_obj = &item["track"];
            if track_obj.is_null() {
                track_obj = &item["item"];
            }
            if track_obj.is_null() || !track_obj.is_object() {
                // Ignore missing tracks (e.g. region blocked)
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
        return Err(anyhow::anyhow!("Failed to parse response payload array. Raw: {}", raw_text));
    }
    
    if tracks.is_empty() {
        return Err(anyhow::anyhow!("Loaded items but found 0 playable tracks! Payload: {}", raw_text.chars().take(2000).collect::<String>()));
    }
    
    Ok(tracks)
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app_state: AppState) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppMessage>();

    // Start background poller for currently playing track
    let poll_token = app_state.access_token.clone();
    let poll_tx = tx.clone();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            let res = client.get("https://api.spotify.com/v1/me/player/currently-playing")
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
                        
                        let _ = poll_tx.send(AppMessage::UpdatePlayerState(Some(PlayerState { 
                            track_name: name, artist, progress_ms: progress, duration_ms: duration, is_playing 
                        })));
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
                AppMessage::TracksFetched { playlist_name, tracks } => {
                    let mut list_state = ListState::default();
                    if !tracks.is_empty() {
                        list_state.select(Some(0));
                    }
                    app_state.current_view = View::Tracks { playlist_name, tracks, state: list_state };
                }
                AppMessage::FetchError(err) => {
                    let mut list_state = ListState::default();
                    list_state.select(Some(0));
                    app_state.current_view = View::Tracks {
                        playlist_name: "Error".to_string(),
                        tracks: vec![Track {
                            name: err,
                            artist: String::new(),
                            album: String::new(),
                            duration_ms: 0,
                            uri: "".to_string(),
                        }],
                        state: list_state,
                    };
                }
                AppMessage::UpdatePlayerState(pstate) => {
                    app_state.player_state = pstate;
                }
            }
        }

        // Advance spinner if loading
        if let View::LoadingTracks { ref mut spinner_tick } = app_state.current_view {
            *spinner_tick = spinner_tick.wrapping_add(1);
        }

        // Draw UI
        terminal.draw(|f| ui(f, &mut app_state))?;

        // GUI IO Polling logic
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
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
                    _ => {}
                }

                // View-specific events
                match app_state.current_view {
                    View::Playlists => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Down | KeyCode::Char('j') => {
                                let i = match app_state.playlist_state.selected() {
                                    Some(i) => if i >= app_state.playlists.len().saturating_sub(1) { 0 } else { i + 1 },
                                    None => 0,
                                };
                                app_state.playlist_state.select(Some(i));
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                let i = match app_state.playlist_state.selected() {
                                    Some(i) => if i == 0 { app_state.playlists.len().saturating_sub(1) } else { i - 1 },
                                    None => 0,
                                };
                                app_state.playlist_state.select(Some(i));
                            }
                            KeyCode::Enter => {
                                if let Some(i) = app_state.playlist_state.selected() {
                                    let playlist = &app_state.playlists[i];
                                    app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                                    
                                    let tx = tx.clone();
                                    let p_id = playlist.id.clone();
                                    let p_name = playlist.name.clone();
                                    let token = app_state.access_token.clone();
                                    
                                    tokio::spawn(async move {
                                        match fetch_tracks(token, p_id).await {
                                            Ok(tracks) => { let _ = tx.send(AppMessage::TracksFetched{ playlist_name: p_name, tracks }); }
                                            Err(e) => { let _ = tx.send(AppMessage::FetchError(e.to_string())); }
                                        }
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    View::Tracks { ref mut state, ref tracks, .. } => {
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
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
                                // Request API to play this highlighted track!
                                if let Some(i) = state.selected() {
                                    let token = app_state.access_token.clone();
                                    let uri = tracks[i].uri.clone();
                                    if !uri.is_empty() {
                                        tokio::spawn(async move {
                                            let _ = play_track(&token, &uri).await;
                                        });
                                    }
                                }
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3), 
            Constraint::Min(1),
            Constraint::Length(3)  // Bottom player
        ].as_ref())
        .split(f.size());

    // Top banner
    let nav_hint = match state.current_view {
        View::Playlists => "(↑/↓ Navigate, Enter View, q Quit)",
        View::Tracks { .. } | View::LoadingTracks { .. } => "(↑/↓ Nav, Esc Back, Enter PLAY Track!, q Quit)",
    };
    
    let welcome_msg = format!("SpotMe Client - Welcome, {}! {}", state.display_name, nav_hint);
    let banner = Paragraph::new(welcome_msg)
        .block(Block::default().borders(Borders::ALL).title("User Info"))
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(banner, chunks[0]);

    // Active View
    match &mut state.current_view {
        View::Playlists => {
            let items: Vec<ListItem> = state.playlists
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
        View::Tracks { playlist_name, tracks, state: list_state } => {
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

            f.render_stateful_widget(tracks_list, chunks[1], list_state);
        }
    }

    // Bottom Player Box
    let player_block = Block::default().borders(Borders::ALL).title("Spotify Desktop Remote");
    if let Some(player) = &state.player_state {
        let status = if player.is_playing { "▶ PLAYING " } else { "⏸ PAUSED  " };
        let info = format!("{} \u{2014} {} [{}] \u{2014} {} / {}", 
            player.track_name, player.artist, status, 
            format_duration(player.progress_ms), format_duration(player.duration_ms));
        
        let mut progress_ratio = 0.0;
        if player.duration_ms > 0 {
            progress_ratio = (player.progress_ms as f64 / player.duration_ms as f64).clamp(0.0, 1.0);
        }
        
        let gauge = Gauge::default()
            .block(player_block)
            .gauge_style(Style::default().fg(Color::Green).bg(Color::Black))
            .ratio(progress_ratio)
            .label(Span::styled(info, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
            
        f.render_widget(gauge, chunks[2]);
    } else {
        let text = Paragraph::new(" Booting internal audio decoder. Setting up local Spotify Connect link... 🔊")
            .block(player_block)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(text, chunks[2]);
    }
}
