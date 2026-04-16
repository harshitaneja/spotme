use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const AUTH_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_PORT: u16 = 8480;
pub const SEEK_SHORT_MS: u64 = 5000;
pub const SEEK_LONG_MS: u64 = 15000;
pub const VOLUME_STEP: u8 = 5;

/// Persistent user configuration saved as TOML.
#[derive(Serialize, Deserialize, Clone)]
pub struct UserConfig {
    #[serde(default = "default_volume")]
    pub volume: u8,
    #[serde(default = "default_volume_step")]
    pub volume_step: u8,
    #[serde(default)]
    pub fullscreen_on_start: bool,
}

fn default_volume() -> u8 {
    50
}
fn default_volume_step() -> u8 {
    VOLUME_STEP
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            volume: default_volume(),
            volume_step: default_volume_step(),
            fullscreen_on_start: false,
        }
    }
}

impl UserConfig {
    pub fn load() -> Self {
        let path = &paths().config_file;
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(cfg) = toml::from_str(&content) {
                return cfg;
            }
        }
        let cfg = Self::default();
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        let path = &paths().config_file;
        if let Ok(content) = toml::to_string_pretty(self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(path)
                {
                    use std::io::Write;
                    let _ = file.write_all(content.as_bytes());
                }
            }
            #[cfg(not(unix))]
            {
                let _ = std::fs::write(path, content);
            }
        }
    }
}

pub struct AppPaths {
    pub cache_file: PathBuf,
    pub token_cache_file: PathBuf,
    pub config_file: PathBuf,
    pub log_file: PathBuf,
    pub env_file: PathBuf,
}

impl AppPaths {
    pub fn init() -> Self {
        let proj_dirs = ProjectDirs::from("com", "spotme", "spotme");

        let (cache_dir, data_dir, config_dir) = if let Some(p) = proj_dirs {
            (
                p.cache_dir().to_path_buf(),
                p.data_dir().to_path_buf(),
                p.config_dir().to_path_buf(),
            )
        } else {
            let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            (base.clone(), base.clone(), base.clone())
        };

        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            eprintln!("Warning: Failed to create cache directory: {}", e);
        }
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            eprintln!("Warning: Failed to create data directory: {}", e);
        }
        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            eprintln!("Warning: Failed to create config directory: {}", e);
        }

        Self {
            cache_file: cache_dir.join("spotme_cache.json"),
            token_cache_file: cache_dir.join("spotify_token_cache.json"),
            config_file: config_dir.join("config.toml"),
            log_file: data_dir.join("spotme.log"),
            env_file: config_dir.join(".env"),
        }
    }
}

pub fn paths() -> &'static AppPaths {
    static APP_PATHS: std::sync::OnceLock<AppPaths> = std::sync::OnceLock::new();
    APP_PATHS.get_or_init(AppPaths::init)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_paths_singleton() {
        let p1 = paths();
        let p2 = paths();
        assert_eq!(
            p1.cache_file.to_string_lossy(),
            p2.cache_file.to_string_lossy()
        );
        assert!(p1
            .cache_file
            .to_string_lossy()
            .contains("spotme_cache.json"));
        assert!(p1.log_file.to_string_lossy().contains("spotme.log"));
        assert!(p1.env_file.to_string_lossy().contains(".env"));
    }
}
