use directories::ProjectDirs;
use std::path::PathBuf;

pub struct AppPaths {
    pub cache_file: PathBuf,
    pub token_cache_file: PathBuf,
    pub log_file: PathBuf,
    pub env_file: PathBuf,
}

impl AppPaths {
    pub fn init() -> Self {
        let proj_dirs = ProjectDirs::from("com", "spotme", "spotme")
            .expect("Failed to bind robust OS-native directory structures!");

        let cache_dir = proj_dirs.cache_dir();
        let data_dir = proj_dirs.data_dir();
        let config_dir = proj_dirs.config_dir();

        std::fs::create_dir_all(cache_dir).unwrap_or_default();
        std::fs::create_dir_all(data_dir).unwrap_or_default();
        std::fs::create_dir_all(config_dir).unwrap_or_default();

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
