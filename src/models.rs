use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use uuid::Uuid;

pub type DownloadId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Downloading => "Downloading",
            Self::Paused => "Paused",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading)
    }

    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_value(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Queued,
        }
    }
}

impl fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: DownloadId,
    pub url: String,
    pub file_name: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub status: DownloadStatus,
    pub save_path: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub speed_bps: f64,
    pub eta_seconds: Option<u64>,
    pub error: Option<String>,
    pub priority: i32,
    pub connections: u32,
    pub expected_sha256: Option<String>,
    pub content_type: Option<String>,
}

impl DownloadRecord {
    pub fn new(url: String, file_name: String, save_path: String, connections: u32) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            url,
            file_name,
            total_bytes: None,
            downloaded_bytes: 0,
            status: DownloadStatus::Queued,
            save_path,
            created_at: Utc::now().timestamp(),
            completed_at: None,
            speed_bps: 0.0,
            eta_seconds: None,
            error: None,
            priority: 0,
            connections: connections.clamp(1, 16),
            expected_sha256: None,
            content_type: None,
        }
    }

    pub fn progress(&self) -> f32 {
        match self.total_bytes {
            Some(total) if total > 0 => {
                (self.downloaded_bytes as f64 / total as f64).clamp(0.0, 1.0) as f32
            }
            Some(_) => 0.0,
            None => 0.0,
        }
    }

    pub fn destination(&self) -> &Path {
        Path::new(&self.save_path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppItem {
    pub id: String,
    pub name: String,
    pub icon_url: Option<String>,
    pub short_description: String,
    pub version: String,
    pub size_bytes: Option<u64>,
    pub developer: String,
    pub updated_at: String,
    pub category: String,
    pub download_url: String,
    pub details_url: Option<String>,
    pub sha256: Option<String>,
}

impl AppItem {
    pub fn size_label(&self) -> String {
        self.size_bytes
            .map(crate::utils::format_bytes)
            .unwrap_or_else(|| "Size unavailable".to_owned())
    }
}
