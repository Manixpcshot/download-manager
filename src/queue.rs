use crate::models::{DownloadRecord, DownloadStatus};

pub fn is_waiting(record: &DownloadRecord) -> bool {
    matches!(
        record.status,
        DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed
    )
}

pub fn is_startable(record: &DownloadRecord) -> bool {
    matches!(record.status, DownloadStatus::Queued | DownloadStatus::Paused)
}

pub fn sort_by_priority(records: &mut [DownloadRecord]) {
    records.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.created_at.cmp(&right.created_at))
    });
}
