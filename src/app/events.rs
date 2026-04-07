use crate::api::endpoints::*;
use crate::api::models::Track;
use crate::app::state::*;
use crate::{get_current_unix_time, save_cache};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::widgets::ListState;
use ratatui_image::picker::ProtocolType;
use tokio::sync::mpsc::UnboundedSender;

pub fn handle_key_events(
    key: KeyEvent,
    app_state: &mut AppState,
    tx: &UnboundedSender<AppMessage>,
) -> anyhow::Result<bool> {
    if app_state.show_help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
        ) {
            app_state.show_help = false;
        }
        return Ok(false); // Lock view events
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
                    app_state.lyrics_scroll_offset =
                        app_state.lyrics_scroll_offset.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app_state.lyrics_scroll_offset =
                        app_state.lyrics_scroll_offset.saturating_add(1);
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
                            tokio::spawn(async move {
                                let _ = pause_playback(&token).await;
                            });
                        }
                    } else {
                        if player.is_fresh_cache {
                            player.is_fresh_cache = false;
                            let prog = player.progress_ms;
                            if let Some(uri) = player.track_uri.clone() {
                                tokio::spawn(async move {
                                    let _ = play_track(&token, &uri, prog).await;
                                });
                            } else {
                                let t_name = player.track_name.clone();
                                let a_name = player.artist.clone();
                                tokio::spawn(async move {
                                    if let Ok(tracks) = search_spotify_api(
                                        &token,
                                        &format!("{} {}", t_name, a_name),
                                    )
                                    .await
                                    {
                                        if let Some(first) = tracks.first() {
                                            let _ = play_track(&token, &first.uri, prog).await;
                                        }
                                    }
                                });
                            }
                        } else if let Some(tx) = &app_state.local_cmd_tx {
                            let _ = tx.try_send(LocalPlayerCommand::Play);
                        } else {
                            tokio::spawn(async move {
                                let _ = resume_playback(&token).await;
                            });
                        }
                    }
                    player.is_playing = !is_playing;
                }
            }
            KeyCode::Left => {
                // Seek back 5s
                if let Some(player) = &app_state.player_state {
                    let token = app_state.access_token.clone();
                    let seek_ms = player
                        .progress_ms
                        .saturating_sub(crate::config::SEEK_SHORT_MS);
                    app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                    tokio::spawn(async move {
                        let _ = seek_playback(&token, seek_ms).await;
                    });
                }
            }
            KeyCode::Right => {
                // Seek forward 5s
                if let Some(player) = &app_state.player_state {
                    let token = app_state.access_token.clone();
                    let seek_ms = std::cmp::min(
                        player.progress_ms + crate::config::SEEK_SHORT_MS,
                        player.duration_ms,
                    );
                    app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                    tokio::spawn(async move {
                        let _ = seek_playback(&token, seek_ms).await;
                    });
                }
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                // Seek back 15s
                if let Some(player) = &app_state.player_state {
                    let token = app_state.access_token.clone();
                    let seek_ms = player
                        .progress_ms
                        .saturating_sub(crate::config::SEEK_LONG_MS);
                    app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                    tokio::spawn(async move {
                        let _ = seek_playback(&token, seek_ms).await;
                    });
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
                    let seek_ms = std::cmp::min(
                        player.progress_ms + crate::config::SEEK_LONG_MS,
                        player.duration_ms,
                    );
                    app_state.player_state.as_mut().unwrap().progress_ms = seek_ms;
                    tokio::spawn(async move {
                        let _ = seek_playback(&token, seek_ms).await;
                    });
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
                tokio::spawn(async move {
                    let _ = next_track(&token).await;
                });
            }
            KeyCode::Char('p') => {
                let token = app_state.access_token.clone();
                tokio::spawn(async move {
                    let _ = previous_track(&token).await;
                });
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                if let Some(player) = &app_state.player_state {
                    let token = app_state.access_token.clone();
                    let vol =
                        std::cmp::min(player.volume_percent + crate::config::VOLUME_STEP, 100);
                    app_state.player_state.as_mut().unwrap().volume_percent = vol;
                    tokio::spawn(async move {
                        let _ = set_volume(&token, vol).await;
                    });
                }
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                if let Some(player) = &app_state.player_state {
                    let token = app_state.access_token.clone();
                    let vol = player
                        .volume_percent
                        .saturating_sub(crate::config::VOLUME_STEP);
                    app_state.player_state.as_mut().unwrap().volume_percent = vol;
                    tokio::spawn(async move {
                        let _ = set_volume(&token, vol).await;
                    });
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
                        Ok(tracks) => {
                            let _ = tx.send(AppMessage::QueueFetched(Ok(tracks)));
                        }
                        Err(e) => {
                            let _ = tx.send(AppMessage::QueueFetched(Err(e)));
                        }
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
                KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
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
                        Some(i) => {
                            if i >= app_state.filtered_playlists.len().saturating_sub(1) {
                                0
                            } else {
                                i + 1
                            }
                        }
                        None => 0,
                    };
                    app_state.playlist_state.select(Some(i));
                }
                KeyCode::Char('s') => {
                    app_state.show_popup = true;
                    app_state.current_view = View::SearchGlobal {
                        query: String::new(),
                        tracks: None,
                        state: ListState::default(),
                        is_typing: true,
                    };
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = match app_state.playlist_state.selected() {
                        Some(i) => {
                            if i == 0 {
                                app_state.filtered_playlists.len().saturating_sub(1)
                            } else {
                                i - 1
                            }
                        }
                        None => 0,
                    };
                    app_state.playlist_state.select(Some(i));
                }
                KeyCode::Char('o') => {
                    app_state.show_others = !app_state.show_others;
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
                    app_state
                        .playlist_state
                        .select(if app_state.filtered_playlists.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
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
                            app_state
                                .app_cache
                                .last_opened
                                .insert(p_id.clone(), get_current_unix_time());
                            save_cache(&app_state.app_cache);

                            let mut state = ListState::default();
                            if !cached_tracks.is_empty() {
                                state.select(Some(0));
                            }
                            app_state.current_view = View::Tracks {
                                playlist_id: p_id,
                                playlist_name: p_name,
                                tracks: cached_tracks.clone(),
                                state,
                                search_query: String::new(),
                                is_searching: false,
                            };
                        } else {
                            app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                match fetch_tracks(token, p_id.clone()).await {
                                    Ok(tracks) => {
                                        let _ = tx.send(AppMessage::TracksFetched {
                                            playlist_id: p_id,
                                            playlist_name: p_name,
                                            tracks,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMessage::FetchError(e.to_string()));
                                    }
                                }
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        View::Tracks {
            playlist_id: ref active_pid,
            ref mut state,
            ref tracks,
            ref mut is_searching,
            ref mut search_query,
            ..
        } => {
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
                        let p_name = app_state
                            .filtered_playlists
                            .iter()
                            .find(|p| p.id == p_id)
                            .map(|p| p.name.clone())
                            .unwrap_or("Tracks".to_string());
                        app_state.current_view = View::LoadingTracks { spinner_tick: 0 };
                        tokio::spawn(async move {
                            match fetch_tracks(token, p_id.clone()).await {
                                Ok(tracks) => {
                                    let _ = tx.send(AppMessage::TracksFetched {
                                        playlist_id: p_id,
                                        playlist_name: p_name,
                                        tracks,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMessage::FetchError(e.to_string()));
                                }
                            }
                        });
                    }
                    KeyCode::Char('q') => return Ok(true),
                    KeyCode::Char('A') => {
                        if let Some(i) = state.selected() {
                            if i < tracks.len() {
                                if let Some(ref aid) = tracks[i].album_id {
                                    let token = app_state.access_token.clone();
                                    let tx = tx.clone();
                                    let a_name = tracks[i].album.clone();
                                    let a_id = aid.clone();
                                    app_state.current_view =
                                        View::LoadingTracks { spinner_tick: 0 };
                                    tokio::spawn(async move {
                                        match fetch_album_tracks(&token, &a_id).await {
                                            Ok(tracks) => {
                                                let _ = tx.send(AppMessage::AlbumTracksFetched {
                                                    album_name: a_name,
                                                    tracks: Ok(tracks),
                                                });
                                            }
                                            Err(e) => {
                                                let _ = tx.send(AppMessage::AlbumTracksFetched {
                                                    album_name: a_name,
                                                    tracks: Err(e),
                                                });
                                            }
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
                                    state: ListState::default().with_selected(
                                        if app_state.filtered_playlists.is_empty() {
                                            None
                                        } else {
                                            Some(0)
                                        },
                                    ),
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
                            Some(i) => {
                                if i >= tracks.len().saturating_sub(1) {
                                    0
                                } else {
                                    i + 1
                                }
                            }
                            None => 0,
                        };
                        state.select(Some(i));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let i = match state.selected() {
                            Some(i) => {
                                if i == 0 {
                                    tracks.len().saturating_sub(1)
                                } else {
                                    i - 1
                                }
                            }
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
                                    let current_vol = app_state
                                        .player_state
                                        .as_ref()
                                        .map(|p| p.volume_percent)
                                        .unwrap_or(50);
                                    app_state.player_state = Some(PlayerState {
                                        track_uri: Some(uri.clone()),
                                        track_name: track.name.clone(),
                                        artist: track.artist.clone(),
                                        progress_ms: 0,
                                        duration_ms: track.duration_ms,
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
        View::SearchGlobal {
            ref mut query,
            ref mut tracks,
            ref mut state,
            ref mut is_typing,
        } => {
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
                                    Ok(results) => {
                                        let _ = tx.send(AppMessage::SearchResults(results));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMessage::SearchError(e));
                                    }
                                }
                            });
                        } else {
                            *is_typing = false;
                        }
                    }
                    KeyCode::Esc => {
                        app_state.current_view = View::Playlists;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                    }
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
                                Some(i) => {
                                    if i >= t.len().saturating_sub(1) {
                                        0
                                    } else {
                                        i + 1
                                    }
                                }
                                None => 0,
                            };
                            state.select(Some(i));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(t) = tracks {
                            let i = match state.selected() {
                                Some(i) => {
                                    if i == 0 {
                                        t.len().saturating_sub(1)
                                    } else {
                                        i - 1
                                    }
                                }
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
                                        state: ListState::default().with_selected(
                                            if app_state.filtered_playlists.is_empty() {
                                                None
                                            } else {
                                                Some(0)
                                            },
                                        ),
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
                                        let current_vol = app_state
                                            .player_state
                                            .as_ref()
                                            .map(|p| p.volume_percent)
                                            .unwrap_or(50);
                                        app_state.player_state = Some(PlayerState {
                                            track_uri: Some(uri.clone()),
                                            track_name: track.name.clone(),
                                            artist: track.artist.clone(),
                                            progress_ms: 0,
                                            duration_ms: track.duration_ms,
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
                    }

                    KeyCode::Char('q') => return Ok(true),
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
        View::SelectPlaylist {
            ref track_uri,
            track_name: _,
            ref mut state,
            previous: _,
        } => match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('b') => {
                if let View::SelectPlaylist { previous, .. } =
                    std::mem::replace(&mut app_state.current_view, View::Playlists)
                {
                    app_state.current_view = *previous;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let i = match state.selected() {
                    Some(i) => {
                        if i >= app_state.filtered_playlists.len().saturating_sub(1) {
                            0
                        } else {
                            i + 1
                        }
                    }
                    None => 0,
                };
                state.select(Some(i));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = match state.selected() {
                    Some(i) => {
                        if i == 0 {
                            app_state.filtered_playlists.len().saturating_sub(1)
                        } else {
                            i - 1
                        }
                    }
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
            KeyCode::Char('q') => return Ok(true),
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
        },
        View::LoadingTracks { .. } => {
            if let KeyCode::Char('q') | KeyCode::Esc = key.code {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn jump_to_first_match(tracks: &[Track], state: &mut ListState, query: &str) {
    if query.is_empty() {
        return;
    }
    let q = query.to_lowercase();
    if let Some(pos) = tracks.iter().position(|t| {
        t.name.to_lowercase().contains(&q)
            || t.artist.to_lowercase().contains(&q)
            || t.album.to_lowercase().contains(&q)
    }) {
        state.select(Some(pos));
    }
}

fn jump_search_next(tracks: &[Track], state: &mut ListState, query: &str, forward: bool) {
    if query.is_empty() || tracks.is_empty() {
        return;
    }
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
        if t.name.to_lowercase().contains(&q)
            || t.artist.to_lowercase().contains(&q)
            || t.album.to_lowercase().contains(&q)
        {
            state.select(Some(i));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::tests::mock_app_state;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, KeyEventKind, KeyEventState};

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn test_handle_quit_event() {
        let mut state = mock_app_state();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        
        let key = make_key(KeyCode::Char('q'));
        let result = handle_key_events(key, &mut state, &tx).unwrap();
        assert_eq!(result, true); // True means exit loop
    }

    #[test]
    fn test_search_transition() {
        let mut state = mock_app_state();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        
        // Assert initial
        assert!(matches!(state.current_view, View::Playlists));
        
        let key = make_key(KeyCode::Char('s'));
        let result = handle_key_events(key, &mut state, &tx).unwrap();
        assert_eq!(result, false);

        match state.current_view {
            View::SearchGlobal { ref query, is_typing, .. } => {
                assert!(is_typing);
                assert!(query.is_empty());
            }
            _ => panic!("Expected SearchGlobal view"),
        }
        assert!(state.show_popup);
    }
}
