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

/// Lightweight refresh that reads the cached refresh_token and exchanges it for a new access_token.
/// Returns the new access token on success, or None if refresh isn't possible.
pub async fn try_refresh_token(client_id: &str) -> Option<String> {
    let cache_path = &config::paths().token_cache_file;
    let content = tokio::fs::read_to_string(cache_path).await.ok()?;
    let cache: SpotifyTokenCache = serde_json::from_str(&content).ok()?;

    let client = crate::api::get_client();
    let res = client
        .post(format!("{}/api/token", crate::api::accounts_base_url()))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", cache.refresh_token.as_str()),
            ("client_id", client_id),
        ])
        .send()
        .await
        .ok()?;

    let json = res.json::<serde_json::Value>().await.ok()?;
    let access = json["access_token"].as_str()?;
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
            let mut opts = tokio::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            if let Ok(mut file) = opts.open(cache_path).await {
                use tokio::io::AsyncWriteExt;
                let _ = file.write_all(cache_str.as_bytes()).await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::fs::write(cache_path, &cache_str).await;
        }
    }

    Some(access.to_string())
}

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
                                let mut opts = tokio::fs::OpenOptions::new();
                                opts.write(true).create(true).truncate(true).mode(0o600);
                                if let Ok(mut file) = opts.open(cache_path).await {
                                    use tokio::io::AsyncWriteExt;
                                    let _ = file.write_all(cache_str.as_bytes()).await;
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

    // Generate CSRF state parameter
    let mut state_bytes = [0u8; 16];
    rng.fill(&mut state_bytes);
    let oauth_state = URL_SAFE_NO_PAD.encode(state_bytes);

    let scopes = "user-read-private user-read-email playlist-read-private playlist-read-collaborative playlist-modify-public playlist-modify-private user-modify-playback-state user-read-playback-state streaming";
    let enc_redirect = urlencoding::encode(redirect_uri);
    let enc_scopes = urlencoding::encode(scopes);

    let auth_url = format!("{}/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={}&state={}&show_dialog=true", crate::api::accounts_base_url(),
        client_id, enc_redirect, enc_scopes, code_challenge, oauth_state
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

                    let mut auth_code = String::new();
                    let mut returned_state = String::new();
                    for line in request.lines() {
                        if line.starts_with("GET ") {
                            if let Some(idx) = line.find("code=") {
                                auth_code = line[idx + 5..].split('&').next().unwrap_or("").split(' ').next().unwrap_or("").to_string();
                            }
                            if let Some(idx) = line.find("state=") {
                                returned_state = line[idx + 6..].split('&').next().unwrap_or("").split(' ').next().unwrap_or("").to_string();
                            }
                            break;
                        }
                    }

                    let response_html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1 style=\"font-family: sans-serif\">SpotMe Login Successful!</h1><p style=\"font-family: sans-serif\">You can safely close this tab and return to the terminal.</p><script>window.close();</script></body></html>";
                    let _ = socket.write_all(response_html.as_bytes()).await;

                    if returned_state != oauth_state {
                        return Err(anyhow::anyhow!("OAuth state mismatch — possible CSRF attack. Please try again."));
                    }
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
                let mut opts = tokio::fs::OpenOptions::new();
                opts.write(true).create(true).truncate(true).mode(0o600);
                if let Ok(mut file) = opts.open(cache_path).await {
                    use tokio::io::AsyncWriteExt;
                    let _ = file.write_all(cache_str.as_bytes()).await;
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

    let backend =
        audio_backend::find(None).ok_or_else(|| anyhow::anyhow!("No audio backend found"))?;
    let player_config = PlayerConfig::default();

    let mixer_fn =
        mixer::find(Some("softvol")).ok_or_else(|| anyhow::anyhow!("No softvol mixer found"))?;
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
            LocalPlayerCommand::SetVolume(percent) => {
                let vol = (percent as u16).min(100) * (u16::MAX / 100);
                let _ = spirc.set_volume(vol);
            }
        }
    }

    Ok(())
}

// Playback API Commands
pub async fn play_track(token: &str, uri: &str, position_ms: u64) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();

    // Find our specific Local SpotMe daemon device to ensure music originates here
    let mut device_id = None;
    for _ in 0..5 {
        if let Ok(res) = client
            .get(format!(
                "{}/v1/me/player/devices",
                crate::api::api_base_url()
            ))
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

    let r = req_res?;
    let status = r.status();
    crate::app_log(&format!("Play request sent. Status: {}", status));
    if !status.is_success() && status != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: status.as_u16(),
            message: "Playback start failed".to_string(),
        });
    }
    Ok(())
}

pub async fn pause_playback(token: &str) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let r = client
        .put(format!("{}/v1/me/player/pause", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Pause failed".to_string(),
        });
    }
    Ok(())
}

pub async fn resume_playback(token: &str) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let r = client
        .put(format!("{}/v1/me/player/play", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Resume failed".to_string(),
        });
    }
    Ok(())
}

pub async fn seek_playback(token: &str, position_ms: u64) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let url = format!(
        "{}/v1/me/player/seek?position_ms={}",
        crate::api::api_base_url(),
        position_ms
    );
    let r = client.put(&url).bearer_auth(token).send().await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Seek failed".to_string(),
        });
    }
    Ok(())
}

pub async fn next_track(token: &str) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let r = client
        .post(format!("{}/v1/me/player/next", crate::api::api_base_url()))
        .bearer_auth(token)
        .send()
        .await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Next track failed".to_string(),
        });
    }
    Ok(())
}

pub async fn previous_track(token: &str) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let r = client
        .post(format!(
            "{}/v1/me/player/previous",
            crate::api::api_base_url()
        ))
        .bearer_auth(token)
        .send()
        .await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Previous track failed".to_string(),
        });
    }
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

fn is_valid_spotify_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

pub async fn fetch_tracks(token: String, playlist_id: String) -> Result<Vec<Track>, anyhow::Error> {
    if !is_valid_spotify_id(&playlist_id) {
        return Err(anyhow::anyhow!("Invalid playlist ID"));
    }
    let client = crate::api::get_client();
    let mut url = format!(
        "{}/v1/playlists/{}/items?market=from_token",
        crate::api::api_base_url(),
        playlist_id
    );
    let mut tracks = Vec::new();

    loop {
        let res = client.get(&url).bearer_auth(&token).send().await?;
        let raw_text = res.text().await?;
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
                return Err(anyhow::anyhow!("Failed to parse response payload array."));
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
        return Err(anyhow::anyhow!("Loaded items but found 0 playable tracks!"));
    }

    Ok(tracks)
}

pub async fn search_spotify_api(token: &str, query: &str) -> Result<Vec<Track>, SpotifyApiError> {
    let client = crate::api::get_client();
    let safe_query = urlencoding::encode(query.trim());

    // Spotify natively defaults to 20 limit. Leaving it omitted bypasses the 400 Bad Request parameter fault.
    let url = format!(
        "{}/v1/search?q={}&type=track",
        crate::api::api_base_url(),
        safe_query
    );

    app_log("NETWORK INIT: GET /v1/search");
    let res = client.get(&url).bearer_auth(token).send().await?;

    let status = res.status();
    if !status.is_success() {
        app_log(&format!("NETWORK FAULT: Bad Status {}", status));
        return Err(SpotifyApiError::BadStatus {
            status: status.as_u16(),
            message: "Search request failed".to_string(),
        });
    }

    let text_payload = res
        .text()
        .await
        .map_err(|e| SpotifyApiError::ParseError(format!("Failed to read response body: {}", e)))?;
    app_log(&format!(
        "NETWORK SUCCESS: Payload Size {}",
        text_payload.len()
    ));

    let json: serde_json::Value = serde_json::from_str(&text_payload)
        .map_err(|e| SpotifyApiError::ParseError(format!("Invalid JSON: {}", e)))?;

    let mut tracks = Vec::new();
    if let Some(items) = json["tracks"]["items"].as_array() {
        for item in items {
            if let Some(t) = Track::parse_track(item, None, None) {
                tracks.push(t);
            }
        }
    } else {
        return Err(SpotifyApiError::ParseError(
            "No items array in search response".to_string(),
        ));
    }

    Ok(tracks)
}

pub async fn add_track_to_playlist_api(
    token: &str,
    playlist_id: &str,
    track_uri: &str,
) -> Result<(), anyhow::Error> {
    if !is_valid_spotify_id(playlist_id) {
        anyhow::bail!("Invalid playlist ID");
    }
    let client = crate::api::get_client();
    let payload = serde_json::json!({ "uris": [track_uri] });

    let url = format!(
        "{}/v1/playlists/{}/items",
        crate::api::api_base_url(),
        playlist_id
    );
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

pub async fn fetch_player_queue(token: &str) -> Result<Vec<Track>, SpotifyApiError> {
    let client = crate::api::get_client();
    let url = format!("{}/v1/me/player/queue", crate::api::api_base_url());
    app_log("NETWORK INIT: GET /v1/me/player/queue");
    let res = client.get(url).bearer_auth(token).send().await?;
    let status = res.status();
    if !status.is_success() {
        return Err(SpotifyApiError::BadStatus {
            status: status.as_u16(),
            message: "Queue fetch failed".to_string(),
        });
    }
    let text_payload = res
        .text()
        .await
        .map_err(|e| SpotifyApiError::ParseError(format!("Body read error: {}", e)))?;
    let json: serde_json::Value = serde_json::from_str(&text_payload)
        .map_err(|e| SpotifyApiError::ParseError(format!("Invalid JSON: {}", e)))?;

    let mut tracks = Vec::new();

    if let Some(queue) = json["queue"].as_array() {
        for track_obj in queue {
            if let Some(t) = Track::parse_track(track_obj, None, None) {
                tracks.push(t);
            }
        }
    } else {
        return Err(SpotifyApiError::ParseError(
            "No queue array in response".to_string(),
        ));
    }
    Ok(tracks)
}

pub async fn fetch_album_tracks(
    token: &str,
    album_id: &str,
) -> Result<Vec<Track>, SpotifyApiError> {
    if !is_valid_spotify_id(album_id) {
        return Err(SpotifyApiError::InvalidInput(
            "Invalid album ID".to_string(),
        ));
    }
    let client = crate::api::get_client();
    let url = format!("{}/v1/albums/{}", crate::api::api_base_url(), album_id);
    let res = client.get(&url).bearer_auth(token).send().await?;
    if !res.status().is_success() {
        return Err(SpotifyApiError::BadStatus {
            status: res.status().as_u16(),
            message: "Album fetch failed".to_string(),
        });
    }
    let json: serde_json::Value = res
        .json()
        .await
        .map_err(|e| SpotifyApiError::ParseError(format!("Invalid JSON: {}", e)))?;

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
    let url = format!(
        "{}/v1/browse/featured-playlists?limit=50",
        crate::api::api_base_url()
    );
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
        .timeout(std::time::Duration::from_secs(5))
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

                            if secs < 60 && mins < 600 {
                                let total_ms = (mins * 60 * 1000) + (secs * 1000) + ms;
                                lines.push(LrcLine {
                                    time_ms: total_ms,
                                    text,
                                });
                            }
                        }
                    }
                }
            }
            lines
        });

    Ok(Lyrics { plain, synced })
}

pub async fn set_volume(token: &str, percent: u8) -> Result<(), SpotifyApiError> {
    let client = crate::api::get_client();
    let r = client
        .put(format!(
            "{}/v1/me/player/volume?volume_percent={}",
            crate::api::api_base_url(),
            percent
        ))
        .bearer_auth(token)
        .send()
        .await?;
    if !r.status().is_success() && r.status() != reqwest::StatusCode::NO_CONTENT {
        return Err(SpotifyApiError::BadStatus {
            status: r.status().as_u16(),
            message: "Volume change failed".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[tokio::test]
    async fn test_api_endpoints_mocked() {
        let mut server = Server::new_async().await;

        let mock_profile = server
            .mock("GET", "/v1/me")
            .match_header("authorization", "Bearer test_token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"display_name": "Mock User", "id": "mock_id"}"#)
            .create_async()
            .await;

        let mock_pause = server
            .mock("PUT", "/v1/me/player/pause")
            .match_header("authorization", "Bearer test_token")
            .with_status(204)
            .create_async()
            .await;

        // SAFETY: This test is not run concurrently with other tests that read this env var.
        unsafe { std::env::set_var("SPOTIFY_API_BASE_URL", server.url()) };

        let profile_res = fetch_user_profile("test_token").await.unwrap();
        assert_eq!(profile_res.0, "Mock User");
        assert_eq!(profile_res.1, "mock_id");

        let pause_res = pause_playback("test_token").await;
        assert!(pause_res.is_ok());

        mock_profile.assert_async().await;
        mock_pause.assert_async().await;
    }
}
