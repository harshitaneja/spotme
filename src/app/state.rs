use crate::api::models::*;
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;

pub enum LocalPlayerCommand {
    Play,
    Pause,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct CachedPlayerState {
    #[serde(default)]
    pub track_uri: Option<String>,
    pub track_name: String,
    pub artist: String,
    #[serde(default)]
    pub progress_ms: u64,
    pub duration_ms: u64,
    pub album_art_url: Option<String>,
    pub lyrics: Option<Lyrics>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct AppCache {
    pub playlists: Vec<Playlist>,
    pub tracks: HashMap<String, Vec<Track>>,
    pub last_opened: HashMap<String, u64>,
    pub last_player: Option<CachedPlayerState>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct PlayerState {
    pub track_uri: Option<String>,
    pub track_name: String,
    pub artist: String,
    pub progress_ms: u64,
    pub duration_ms: u64,
    pub is_playing: bool,
    pub volume_percent: u8,
    pub album_art_url: Option<String>,
    pub is_buffering: bool,
    pub is_fresh_cache: bool,
    pub lyrics: Option<Lyrics>,
}

// GUI State
pub enum View {
    Playlists,
    LoadingTracks {
        spinner_tick: u8,
    },
    Tracks {
        playlist_id: String,
        playlist_name: String,
        tracks: Vec<Track>,
        state: ListState,
        search_query: String,
        is_searching: bool,
    },
    SearchGlobal {
        query: String,
        tracks: Option<Vec<Track>>,
        state: ListState,
        is_typing: bool,
    },
    SelectPlaylist {
        track_uri: String,
        track_name: String,
        state: ListState,
        previous: Box<View>,
    },
}

#[derive(Clone, PartialEq)]
pub enum LyricsMode {
    Focused,
    Full,
}

pub struct AppState {
    pub display_name: String,
    pub user_id: String,
    pub show_others: bool,
    pub app_cache: AppCache,
    pub filtered_playlists: Vec<Playlist>,
    pub playlist_state: ListState,
    pub current_view: View,
    pub access_token: String,
    pub player_state: Option<PlayerState>,

    pub current_art_url: Option<String>,
    pub current_art_bytes: Option<Vec<u8>>,
    pub current_art_protocol: Option<StatefulProtocol>,
    pub player_spinner_tick: u8,
    pub picker: Picker,
    pub fullscreen_player: bool,
    pub lyrics_mode: LyricsMode,
    pub lyrics_scroll_offset: usize,
    pub dominant_color: Option<(u8, u8, u8)>,
    pub show_help: bool,
    pub show_popup: bool,
    pub local_cmd_tx: Option<mpsc::Sender<LocalPlayerCommand>>,
    pub last_action_timestamp: u64,
}

impl AppState {
    pub fn merge_incoming_player_state(&mut self, mut pstate: Option<PlayerState>, now: u64) {
        let is_debounce_active = now.saturating_sub(self.last_action_timestamp) < 3;

        if is_debounce_active {
            if let Some(ref local_ps) = self.player_state {
                if let Some(ref incoming_ps) = pstate {
                    if incoming_ps.track_name != local_ps.track_name {
                        return; // Drop lagging packet
                    }
                }
            }
        }

        if is_debounce_active {
            if let Some(ref local_ps) = self.player_state {
                if let Some(ref mut incoming_ps) = pstate {
                    incoming_ps.is_playing = local_ps.is_playing;
                    incoming_ps.volume_percent = local_ps.volume_percent;
                    incoming_ps.progress_ms = local_ps.progress_ms;
                    incoming_ps.is_buffering = local_ps.is_buffering;
                }
            }
        }

        if let Some(ref local_ps) = self.player_state {
            if let Some(ref mut incoming_ps) = pstate {
                if incoming_ps.track_name == local_ps.track_name {
                    incoming_ps.lyrics = local_ps.lyrics.clone();
                }
            }
        }

        self.player_state = pstate;
    }
}

// Async Message passing
pub enum AppMessage {
    TracksFetched {
        playlist_id: String,
        playlist_name: String,
        tracks: Vec<Track>,
    },
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
    AlbumTracksFetched {
        album_name: String,
        tracks: Result<Vec<Track>, String>,
    },
}
