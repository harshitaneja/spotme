use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub collaborative: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Track {
    pub name: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub duration_ms: u64,
    pub uri: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LrcLine {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Lyrics {
    pub plain: Option<String>,
    pub synced: Option<Vec<LrcLine>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpotifyTokenCache {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

impl Track {
    pub fn parse_track(
        track_obj: &serde_json::Value,
        fallback_album: Option<&str>,
        fallback_id: Option<&str>,
    ) -> Option<Track> {
        if track_obj.is_null() || !track_obj.is_object() {
            return None;
        }

        let name = track_obj["name"]
            .as_str()
            .unwrap_or("Unknown Track")
            .to_string();
        let uri = track_obj["uri"].as_str().unwrap_or("").to_string();
        if uri.is_empty() {
            return None;
        }

        let mut artists = Vec::new();
        if let Some(artists_arr) = track_obj["artists"].as_array() {
            for artist in artists_arr {
                if let Some(a_name) = artist["name"].as_str() {
                    artists.push(a_name.to_string());
                }
            }
        }
        let artist_str = if artists.is_empty() {
            "Unknown Artist".to_string()
        } else {
            artists.join(", ")
        };

        let album_obj = &track_obj["album"];
        let album = if !album_obj.is_null() && !album_obj["name"].is_null() {
            album_obj["name"].as_str().unwrap_or("Unknown Album")
        } else {
            fallback_album.unwrap_or("Unknown Album")
        }
        .to_string();

        let album_id = if !album_obj.is_null() && !album_obj["id"].is_null() {
            album_obj["id"].as_str().map(|s| s.to_string())
        } else {
            fallback_id.map(|s| s.to_string())
        };

        let duration_ms = track_obj["duration_ms"].as_u64().unwrap_or(0);

        Some(Track {
            name,
            artist: artist_str,
            album,
            album_id,
            duration_ms,
            uri,
        })
    }
}
