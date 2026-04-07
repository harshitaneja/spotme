pub mod endpoints;
pub mod models;

use std::sync::OnceLock;

pub fn get_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

pub fn api_base_url() -> String {
    std::env::var("SPOTIFY_API_BASE_URL").unwrap_or_else(|_| "https://api.spotify.com".to_string())
}

pub fn accounts_base_url() -> String {
    std::env::var("SPOTIFY_ACCOUNTS_BASE_URL")
        .unwrap_or_else(|_| "https://accounts.spotify.com".to_string())
}
