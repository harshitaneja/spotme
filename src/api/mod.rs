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
                .expect("Failed to build global HTTP client")
        })
        .clone()
}
