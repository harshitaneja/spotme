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
