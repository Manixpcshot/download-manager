use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub title: String,
    pub message: String,
    pub kind: NotificationKind,
    pub created_at: Instant,
    pub expires_after: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct NotificationCenter {
    pub items: Vec<Notification>,
    pub open: bool,
}

impl NotificationCenter {
    pub fn push(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        kind: NotificationKind,
    ) {
        self.items.insert(
            0,
            Notification {
                title: title.into(),
                message: message.into(),
                kind,
                created_at: Instant::now(),
                expires_after: Some(Duration::from_secs(8)),
            },
        );
        self.items.truncate(32);
    }

    pub fn remove_expired(&mut self) {
        let keep_expired = self.open;
        self.items.retain(|item| {
            item.expires_after
                .map(|duration| item.created_at.elapsed() < duration)
                .unwrap_or(true)
                || keep_expired
        });
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}
