use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub general: GeneralSettings,
    pub downloads: DownloadSettings,
    pub network: NetworkSettings,
    pub appearance: AppearanceSettings,
    pub apps_api_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    pub start_with_windows: bool,
    pub minimize_to_tray: bool,
    pub confirm_before_deleting: bool,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSettings {
    pub default_download_folder: String,
    pub maximum_simultaneous_downloads: usize,
    pub maximum_connections_per_download: u32,
    pub download_speed_limit_kbps: u64,
    pub auto_start_downloads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSettings {
    pub connection_timeout_seconds: u64,
    pub retry_count: u32,
    pub proxy: String,
    pub user_agent: String,
    pub additional_headers: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    pub dark_mode: bool,
    pub accent_color: String,
    pub ui_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        let folder = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            general: GeneralSettings {
                start_with_windows: false,
                minimize_to_tray: true,
                confirm_before_deleting: true,
                language: "English".to_owned(),
            },
            downloads: DownloadSettings {
                default_download_folder: folder.to_string_lossy().into_owned(),
                maximum_simultaneous_downloads: 3,
                maximum_connections_per_download: 4,
                download_speed_limit_kbps: 0,
                auto_start_downloads: true,
            },
            network: NetworkSettings {
                connection_timeout_seconds: 30,
                retry_count: 4,
                proxy: String::new(),
                user_agent:
                    "PulseDownloadManager/0.1 (+https://github.com/Manixpcshot/download-manager)"
                        .to_owned(),
                additional_headers: String::new(),
            },
            appearance: AppearanceSettings {
                dark_mode: true,
                accent_color: "#7C6CFF".to_owned(),
                ui_scale: 1.0,
            },
            // This endpoint is deliberately a real JSON HTTP endpoint under the project
            // repository. It can be replaced in Settings with any compatible catalog API.
            apps_api_url:
                "https://raw.githubusercontent.com/Manixpcshot/download-manager/main/apps.json"
                    .to_owned(),
        }
    }
}

impl Settings {
    pub fn directory() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("PulseDownloadManager")
    }

    pub fn path() -> PathBuf {
        Self::directory().join("settings.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("cannot read settings from {}", path.display()))?;
        let mut settings: Self = serde_json::from_str(&contents)
            .with_context(|| format!("invalid settings file {}", path.display()))?;
        settings.ensure_valid();
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(Self::directory())?;
        let path = Self::path();
        let temporary = path.with_extension("json.tmp");
        let contents = serde_json::to_vec_pretty(self)?;
        fs::write(&temporary, contents)?;
        // Windows does not replace an existing file with rename, so remove the old
        // snapshot only after the new one has been fully written.
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)
            .with_context(|| format!("cannot replace settings file {}", path.display()))?;
        Ok(())
    }

    pub fn ensure_valid(&mut self) {
        self.downloads.maximum_simultaneous_downloads =
            self.downloads.maximum_simultaneous_downloads.clamp(1, 32);
        self.downloads.maximum_connections_per_download =
            self.downloads.maximum_connections_per_download.clamp(1, 16);
        self.network.connection_timeout_seconds =
            self.network.connection_timeout_seconds.clamp(5, 600);
        self.network.retry_count = self.network.retry_count.min(20);
        self.appearance.ui_scale = self.appearance.ui_scale.clamp(0.8, 1.5);
        if self.apps_api_url.trim().is_empty() {
            self.apps_api_url = Self::default().apps_api_url;
        }
        if self.default_folder_path().as_os_str().is_empty() {
            self.downloads.default_download_folder = ".".to_owned();
        }
    }

    pub fn default_folder_path(&self) -> PathBuf {
        PathBuf::from(&self.downloads.default_download_folder)
    }

    pub fn data_directory() -> PathBuf {
        dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("PulseDownloadManager")
    }

    pub fn database_path() -> PathBuf {
        Self::data_directory().join("downloads.sqlite3")
    }

    pub fn set_download_folder(&mut self, path: impl AsRef<Path>) {
        self.downloads.default_download_folder = path.as_ref().to_string_lossy().into_owned();
    }
}
