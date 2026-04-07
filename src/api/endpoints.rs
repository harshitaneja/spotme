use crate::api::models::*;
use crate::app_log;
use crate::config;
use crate::get_current_unix_time;
use anyhow::Result;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials as LibrespotCredentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::Player;
use serde_json::Value;
use tokio::sync::mpsc;

pub async fn get_or_refresh_token(client_id: &str, redirect_uri: &str) -> Result<String> {
    let cache_path = &config::paths().token_cache_file;

    if let Ok(content) = tokio::fs::read_to_string(cache_path).await {
        if let Ok(cache) = serde_json::from_str::<SpotifyTokenCache>(&content) {
            if get_current_unix_time() < cache.expires_at {
                return Ok(cache.access_token);
            }

            let client = crate::api::get_client();
            let res = client
                .post(format!("{}/api/token", crate::api::accounts_base_url()))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &cache.refresh_token),
                    ("client_id", client_id),
                ])
                .send()
                .await;

            if let Ok(response) = res {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    if let Some(access) = json["access_token"].as_str() {
                        let refresh = json["refresh_token"]
                            .as_str()
                            .unwrap_or(&cache.refresh_token);
                        let expires_in = json["expires_in"].as_u64().unwrap_or(3600);

                        let new_cache = SpotifyTokenCache {
                            access_token: access.to_string(),
                            refresh_token: refresh.to_string(),
                            expires_at: get_current_unix_time() + expires_in,
                        };

                        if let Ok(cache_str) = serde_json::to_string(&new_cache) {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = tokio::fs::write(cache_path, &cache_str).await;
                                if let Ok(meta) = std::fs::metadata(cache_path) {
                                    let mut perms = meta.permissions();
                                    perms.set_mode(0o600);
                                    let _ = std::fs::set_permissions(cache_path, perms);
                                }
                            }
                            #[cfg(not(unix))]
                            {
                                let _ = tokio::fs::write(cache_path, &cache_str).await;
                            }
                        }
                        return Ok(access.to_string());
                    }
                }
            }
        }
    }

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngExt;
    use sha2::{Digest, Sha256};

    // Generate Code Verifier
    let mut rng = rand::rng();
    let mut verifier_bytes = [0u8; 32];
    rng.fill(&mut verifier_bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    // Generate Code Challenge
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    let scopes = "user-read-private user-read-email playlist-read-private playlist-read-collaborative playlist-modify-public playlist-modify-private user-modify-playback-state user-read-playback-state streaming";
    let enc_redirect = urlencoding::encode(redirect_uri);
    let enc_scopes = urlencoding::encode(scopes);

    let auth_url = format!("{}/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={}&show_dialog=true", crate::api::accounts_base_url(),
        client_id, enc_redirect, enc_scopes, code_challenge
    );

    println!("Opening Spotify login in your browser...");
    println!(
        "If it doesn't open automatically, please click here: \n{}\n",
        auth_url
    );

    let _ = open::that(&auth_url);

    let url_parts: Vec<&str> = redirect_uri.split(':').collect();
    let port_part = url_parts
        .last()
        .unwrap_or(&"8480")
        .split('/')
        .next()
        .unwrap_or("8480");
    let port_u16 = port_part
        .parse::<u16>()
        .unwrap_or(crate::config::DEFAULT_PORT);

    let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port_u16)).await {
        Ok(l) => l,
        Err(e) => return Err(anyhow::anyhow!("Failed to bind port {}: {}", port_u16, e)),
    };
    println!("Waiting up to 120 seconds for browser authentication... (Press Ctrl+C to cancel)");

    let code = tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(crate::config::AUTH_TIMEOUT_SECS)) => {
            return Err(anyhow::anyhow!("Authentication timed out after {} seconds. Please run SpotMe again.", crate::config::AUTH_TIMEOUT_SECS));
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

                    if auth_code.is_empty() { return Err(anyhow::anyhow!("Could not extract code from callback request!")); }
                    auth_code
                }
                Err(e) => { return Err(anyhow::anyhow!("Listener failed to accept connection: {}", e)); }
            }
        }
    };

    let client = crate::api::get_client();
    let response = client
        .post(format!("{}/api/token", crate::api::accounts_base_url()))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", &code_verifier),
        ])
        .send()
        .await?;

    let json: serde_json::Value = response.json().await?;
    if let Some(access) = json["access_token"].as_str() {
        let refresh = json["refresh_token"].as_str().unwrap_or("");
        let expires_in = json["expires_in"].as_u64().unwrap_or(3600);

        let new_cache = SpotifyTokenCache {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: get_current_unix_time() + expires_in,
        };

        if let Ok(cache_str) = serde_json::to_string(&new_cache) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = tokio::fs::write(cache_path, &cache_str).await;
                if let Ok(meta) = std::fs::metadata(cache_path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o600);
                    let _ = std::fs::set_permissions(cache_path, perms);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::fs::write(cache_path, &cache_str).await;
            }
        }
        return Ok(access.to_string());
    }

    Err(anyhow::anyhow!(
        "Failed to parse token response: {:?}",
        json
    ))
}

pub async fn fetch_user_profile(token: &str) -> Result<(String, String)> {
    let client = crate::api::get_client();
    let res = client
        .get(format!("{}/v1/me", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;

    let json = res.json::<serde_json::Value>().await?;
    let display_name = json["display_name"]
        .as_str()
        .unwrap_or("Unknown")
        .to_string();
    let id = json["id"].as_str().unwrap_or("").to_string();
    Ok((display_name, id))
}

use crate::app::state::*;

// Background Task for Librespot Daemon
pub async fn start_librespot_daemon(
    token: String,
    mut receiver: mpsc::Receiver<LocalPlayerCommand>,
) -> Result<()> {
    let credentials = LibrespotCredentials::with_access_token(token);
    let session_config = SessionConfig::default();

    // Connect Session
    let session = Session::new(session_config, None);

    let backend = audio_backend::find(None).ok_or_else(|| anyhow::anyhow!("No audio backend found"))?;
    let player_config = PlayerConfig::default();

    let mixer_fn = mixer::find(Some("softvol")).ok_or_else(|| anyhow::anyhow!("No softvol mixer found"))?;
    let mixer_for_player = mixer_fn(MixerConfig::default())?;

    let player = Player::new(
        player_config,
        session.clone(),
        mixer_for_player.get_soft_volume(),
        move || backend(None, AudioFormat::default()),
    );

    let connect_config = ConnectConfig {
        name: "SpotMe Local Player".to_string(),
        ..Default::default()
    };

    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session,
        credentials,
        player,
        mixer_for_player,
    )
    .await?;

    tokio::spawn(spirc_task);

    while let Some(cmd) = receiver.recv().await {
        match cmd {
            LocalPlayerCommand::Play => {
                let _ = spirc.play();
            }
            LocalPlayerCommand::Pause => {
                let _ = spirc.pause();
            }
        }
    }

    Ok(())
}

// Playback API Commands
pub async fn play_track(token: &str, uri: &str, position_ms: u64) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();

    // Find our specific Local SpotMe daemon device to ensure music originates here
    let mut device_id = None;
    for _ in 0..5 {
        if let Ok(res) = client
            .get(format!("{}/v1/me/player/devices", crate::api::api_base_url()))
            .bearer_auth(token)
            .send()
            .await
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

    let mut url = format!("{}/v1/me/player/play", crate::api::api_base_url());
    if let Some(id) = device_id {
        url = format!("{}?device_id={}", url, id);
    }

    let body = serde_json::json!({ "uris": [uri], "position_ms": position_ms });
    let req_res = client.put(&url).bearer_auth(token).json(&body).send().await;

    match req_res {
        Ok(r) => {
            let _ = tokio::fs::write(
                &config::paths().log_file,
                format!("Play request sent. Status: {}, URL: {}\n", r.status(), url),
            )
            .await;
        }
        Err(e) => {
            let _ = tokio::fs::write(
                &config::paths().log_file,
                format!("Play request FAILED: {}\n", e),
            )
            .await;
        }
    }
    Ok(())
}

pub async fn pause_playback(token: &str) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    client
        .put(format!("{}/v1/me/player/pause", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

pub async fn resume_playback(token: &str) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    client
        .put(format!("{}/v1/me/player/play", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

pub async fn seek_playback(token: &str, position_ms: u64) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    let url = format!("{}/v1/me/player/seek?position_ms={}", crate::api::api_base_url(),
        position_ms
    );
    client.put(&url).bearer_auth(token).send().await?;
    Ok(())
}

pub async fn next_track(token: &str) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    client
        .post(format!("{}/v1/me/player/next", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

pub async fn previous_track(token: &str) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    client
        .post(format!("{}/v1/me/player/previous", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

// Track Fetch Hook
pub async fn fetch_playlists_api(token: &str) -> Vec<Playlist> {
    let client = crate::api::get_client();
    let mut url = format!("{}/v1/me/playlists?limit=50", crate::api::api_base_url());
    let mut out = Vec::new();

    while let Ok(res) = client.get(&url).bearer_auth(token).send().await {
        if let Ok(json) = res.json::<serde_json::Value>().await {
            if let Some(items) = json["items"].as_array() {
                for item in items {
                    if let (Some(name), Some(id)) = (item["name"].as_str(), item["id"].as_str()) {
                        let owner = item["owner"]["id"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string();
                        let collab = item["collaborative"].as_bool().unwrap_or(false);
                        out.push(Playlist {
                            name: name.to_string(),
                            id: id.to_string(),
                            owner_id: owner,
                            collaborative: collab,
                        });
                    }
                }
            }
            if let Some(n) = json["next"].as_str() {
                url = n.to_string();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    out
}

pub async fn fetch_tracks(token: String, playlist_id: String) -> Result<Vec<Track>, anyhow::Error> {
    let client = crate::api::get_client();
    let mut url = format!("{}/v1/playlists/{}/items?market=from_token", crate::api::api_base_url(),
        playlist_id
    );
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
                if let Some(t) = Track::parse_track(track_obj, None, None) {
                    tracks.push(t);
                }
            }
        } else {
            if tracks.is_empty() {
                return Err(anyhow::anyhow!(
                    "Failed to parse response payload array. Raw: {}",
                    raw_text
                ));
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
        return Err(anyhow::anyhow!(
            "Loaded items but found 0 playable tracks! Payload: {}",
            raw_text_fallback.chars().take(2000).collect::<String>()
        ));
    }

    Ok(tracks)
}

pub async fn search_spotify_api(token: &str, query: &str) -> Result<Vec<Track>, String> {
    let client = crate::api::get_client();
    let safe_query = urlencoding::encode(query.trim());

    // Spotify natively defaults to 20 limit. Leaving it omitted bypasses the 400 Bad Request parameter fault.
    let url = format!("{}/v1/search?q={}&type=track", crate::api::api_base_url(),
        safe_query
    );

    app_log("NETWORK INIT: GET /v1/search");
    let res = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| {
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

    let text_payload = res
        .text()
        .await
        .map_err(|e| format!("Text read Err: {}", e))?;
    app_log(&format!(
        "NETWORK SUCCESS: Payload Size {}",
        text_payload.len()
    ));

    let json: serde_json::Value = serde_json::from_str(&text_payload).map_err(|e| {
        let err_str = format!("JSON Err: {}", e);
        app_log(&err_str);
        err_str
    })?;

    let mut tracks = Vec::new();
    if let Some(items) = json["tracks"]["items"].as_array() {
        for item in items {
            if let Some(t) = Track::parse_track(item, None, None) {
                tracks.push(t);
            }
        }
    } else {
        return Err(format!("Bad payload: no items array. {}", json));
    }

    Ok(tracks)
}

pub async fn add_track_to_playlist_api(
    token: &str,
    playlist_id: &str,
    track_uri: &str,
) -> Result<(), anyhow::Error> {
    let client = crate::api::get_client();
    let payload = serde_json::json!({ "uris": [track_uri] });

    let url = format!("{}/v1/playlists/{}/items", crate::api::api_base_url(), playlist_id);
    app_log("ADD TRACK INIT: POST /v1/playlists/*/items");
    app_log("ADD TRACK PAYLOAD: [REDACTED]");

    let res = client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await?;

    let status = res.status();

    if status.is_success() {
        app_log(&format!("ADD TRACK SUCCESS {}", status));
        Ok(())
    } else {
        app_log(&format!("ADD TRACK FAULT {}", status));
        anyhow::bail!("Failed to add track")
    }
}

pub async fn fetch_player_queue(token: &str) -> Result<Vec<Track>, String> {
    let client = crate::api::get_client();
    let url = format!("{}/v1/me/player/queue", crate::api::api_base_url());
    app_log("NETWORK INIT: GET /v1/me/player/queue");
    let res = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Req Err: {}", e))?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!(
            "Bad Status {}: {}",
            status,
            res.text().await.unwrap_or_default()
        ));
    }
    let text_payload = res
        .text()
        .await
        .map_err(|e| format!("Text read Err: {}", e))?;
    let json: serde_json::Value =
        serde_json::from_str(&text_payload).map_err(|e| format!("JSON Err: {}", e))?;

    let mut tracks = Vec::new();

    if let Some(queue) = json["queue"].as_array() {
        for track_obj in queue {
            if let Some(t) = Track::parse_track(track_obj, None, None) {
                tracks.push(t);
            }
        }
    } else {
        return Err(format!("Bad payload: no queue array. {}", json));
    }
    Ok(tracks)
}

pub async fn fetch_album_tracks(token: &str, album_id: &str) -> Result<Vec<Track>, String> {
    let client = crate::api::get_client();
    let url = format!("{}/v1/albums/{}", crate::api::api_base_url(), album_id);
    let res = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Req Err: {}", e))?;
    if !res.status().is_success() {
        return Err(format!("Bad Status {}", res.status()));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| format!("JSON Err: {}", e))?;

    let mut tracks = Vec::new();
    let album_name = json["name"].as_str().unwrap_or("").to_string();
    let album_id_opt = Some(album_id.to_string());

    if let Some(items) = json["tracks"]["items"].as_array() {
        for track_obj in items {
            if let Some(t) =
                Track::parse_track(track_obj, Some(&album_name), album_id_opt.as_deref())
            {
                tracks.push(t);
            }
        }
    }
    Ok(tracks)
}

pub async fn fetch_featured_playlists_api(token: &str) -> Vec<Playlist> {
    let client = crate::api::get_client();
    let url = format!("{}/v1/browse/featured-playlists?limit=50", crate::api::api_base_url());
    if let Ok(res) = client.get(url).bearer_auth(token).send().await {
        if res.status().is_success() {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                let mut lists = Vec::new();
                if let Some(items) = json["playlists"]["items"].as_array() {
                    for item in items {
                        if item.is_null() {
                            continue;
                        }
                        let id = item["id"].as_str().unwrap_or("").to_string();
                        let name = item["name"]
                            .as_str()
                            .unwrap_or("Featured Playlist")
                            .to_string();
                        let owner_id = item["owner"]["id"]
                            .as_str()
                            .unwrap_or("spotify")
                            .to_string();
                        lists.push(Playlist {
                            id,
                            name,
                            owner_id,
                            collaborative: false,
                        });
                    }
                }
                return lists;
            }
        }
    }
    Vec::new()
}

pub async fn fetch_lyrics_api(
    track_name: &str,
    artist_name: &str,
) -> Result<Lyrics, anyhow::Error> {
    app_log(&format!(
        "FETCH LYRICS INIT: {} - {}",
        track_name, artist_name
    ));

    let clean_track = track_name
        .split(" - ")
        .next()
        .unwrap_or(track_name)
        .to_string();
    let clean_artist = artist_name
        .split(',')
        .next()
        .unwrap_or(artist_name)
        .to_string();

    let client = crate::api::get_client();
    let url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}",
        urlencoding::encode(&clean_track),
        urlencoding::encode(&clean_artist)
    );
    let res = client
        .get(&url)
        .header("User-Agent", "SpotMe/0.1.0")
        .send()
        .await?;

    if !res.status().is_success() {
        app_log(&format!(
            "FETCH LYRICS FAULT {}: {}",
            res.status(),
            res.text().await.unwrap_or_default()
        ));
        return Err(anyhow::anyhow!("Lyrics not found"));
    }

    let text = res.text().await?;
    app_log(&format!("FETCH LYRICS SUCCESS: {}", text.len()));
    let json: serde_json::Value = serde_json::from_str(&text)?;

    let plain = json["plainLyrics"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let synced = json["syncedLyrics"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| {
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
                            } else {
                                0
                            };

                            let total_ms = (mins * 60 * 1000) + (secs * 1000) + ms;
                            lines.push(LrcLine {
                                time_ms: total_ms,
                                text,
                            });
                        }
                    }
                }
            }
            lines
        });

    Ok(Lyrics { plain, synced })
}

pub async fn set_volume(
    token: &str,
    percent: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = crate::api::get_client();
    client
        .put(format!("{}/v1/me/player/volume?volume_percent={}", crate::api::api_base_url(),
            percent
        ))
        .bearer_auth(token)
        .send()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[tokio::test]
    async fn test_api_endpoints_mocked() {
        let mut server = Server::new_async().await;
        
        let mock_profile = server.mock("GET", "/v1/me")
            .match_header("authorization", "Bearer test_token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"display_name": "Mock User", "id": "mock_id"}"#)
            .create_async()
            .await;

        let mock_pause = server.mock("PUT", "/v1/me/player/pause")
            .match_header("authorization", "Bearer test_token")
            .with_status(204)
            .create_async()
            .await;

        std::env::set_var("SPOTIFY_API_BASE_URL", server.url());

        let profile_res = fetch_user_profile("test_token").await.unwrap();
        assert_eq!(profile_res.0, "Mock User");
        assert_eq!(profile_res.1, "mock_id");

        let pause_res = pause_playback("test_token").await;
        assert!(pause_res.is_ok());

        mock_profile.assert_async().await;
        mock_pause.assert_async().await;
    }
}
