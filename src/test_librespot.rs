use librespot_connect::{Spirc, ConnectConfig};
use librespot_core::authentication::Credentials as LibrespotCredentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig, NoOpVolume};
use librespot_playback::player::Player;

#[tokio::main]
async fn main() {
    let raw_text = std::fs::read_to_string(".spotify_token_cache.json").unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw_text).unwrap();
    let token = json["access_token"].as_str().unwrap().to_string();

    println!("Starting test daemon...");
    
    let credentials = LibrespotCredentials::with_access_token(token);
    let session_config = SessionConfig::default();
    
    println!("Connecting session...");
    let session = Session::new(session_config, None);

    println!("Finding audio backend...");
    let backend = audio_backend::find(None).expect("No audio backend found");
    let player_config = PlayerConfig::default();
    
    println!("Creating player...");
    let player = Player::new(
        player_config,
        session.clone(),
        Box::new(NoOpVolume),
        move || {
            backend(None, AudioFormat::default())
        },
    );

    println!("Finding mixer...");
    let mixer = mixer::find(None).expect("No mixer found");
    let mut connect_config = ConnectConfig::default();
    connect_config.name = "SpotMe Local Player".to_string();

    println!("Starting Spirc...");
    let (_spirc, spirc_task) = Spirc::new(
        connect_config,
        session,
        credentials,
        player,
        mixer(MixerConfig::default()).unwrap(),
    ).await.expect("Failed to start spirc");

    println!("Success! Running for 5 seconds...");
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(5), spirc_task).await;
}
