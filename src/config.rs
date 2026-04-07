use directories::ProjectDirs;
use std::path::PathBuf;

pub const AUTH_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_PORT: u16 = 8480;
pub const SEEK_SHORT_MS: u64 = 5000;
pub const SEEK_LONG_MS: u64 = 15000;
pub const VOLUME_STEP: u8 = 5;

pub struct AppPaths {
    pub cache_file: PathBuf,
    pub token_cache_file: PathBuf,
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
