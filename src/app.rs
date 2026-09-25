use anyhow::Result;
use chrono::Utc;
use eframe::egui::{self, Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Stroke, Ui, Vec2, Window};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::apps::AppsState;
use crate::database::{Database, SharedDatabase};
use crate::downloader::{DownloadEngine, DownloadEvent, EngineConfig};
use crate::models::{AppItem, DownloadRecord, DownloadStatus};
use crate::notifications::{Notification, NotificationCenter, NotificationKind};
use crate::settings::Settings;
use crate::system;
use crate::tray::{TrayAction, TrayController};
use crate::ui::theme;
use crate::utils;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Queue,
    Apps,
    Completed,
    Failed,
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Downloads",
            Self::Queue => "Download queue",
            Self::Apps => "Apps",
            Self::Completed => "Completed",
            Self::Failed => "Failed downloads",
            Self::Settings => "Settings",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Dashboard => "Everything Pulse is fetching, in one place.",
            Self::Queue => "Prioritize waiting items and decide when the queue runs.",
            Self::Apps => "Browse trusted software published by your catalog API.",
            Self::Completed => "Finished downloads that are ready to open.",
            Self::Failed => "Items that stopped early and can be retried.",
            Self::Settings => "Tune the engine, storage and interface for your workflow.",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Dashboard => "⌂",
            Self::Queue => "≡",
            Self::Apps => "▣",
            Self::Completed => "✓",
            Self::Failed => "!",
            Self::Settings => "⚙",
        }
    }
}

/// How much of the navigation shell fits into the current window.
///
/// The interface is a native desktop window, so the layout reacts to the real window size instead
/// of media queries: the sidebar becomes an icon rail and finally folds into the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    /// Full sidebar with labels.
    Wide,
    /// Icon-only rail, labels move into hover tooltips.
    Compact,
    /// No sidebar: navigation sits in the header strip.
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusFilter {
    All,
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Recent,
    Name,
    Size,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Downloads,
    Network,
    Appearance,
}

#[derive(Debug)]
struct AddDialog {
    url: String,
    file_name: String,
    folder: String,
    checksum: String,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordAction {
    Start,
    Pause,
    Resume,
    Cancel,
    Retry,
    Open,
    Folder,
    Copy,
    Delete,
}

pub struct DownloadManagerApp {
    settings: Settings,
    database: SharedDatabase,
    engine: DownloadEngine,
    records: Vec<DownloadRecord>,
    page: Page,
    filter: StatusFilter,
    sort: SortKey,
    search: String,
    add_dialog: Option<AddDialog>,
    confirm_delete: Option<String>,
    pending_delete: Option<String>,
    settings_tab: SettingsTab,
    apps: AppsState,
    notifications: NotificationCenter,
    tray: Option<TrayController>,
    queue_running: bool,
    accent: Color32,
}

impl DownloadManagerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        let mut settings = Settings::load().unwrap_or_default();
        settings.ensure_valid();
        let database = Arc::new(Mutex::new(Database::open(Settings::database_path())?));
        let mut records = database
            .lock()
            .map_err(|_| anyhow::anyhow!("download database lock is poisoned"))?
            .load_downloads()?;
        let mut startup_records = Vec::new();
        for record in &mut records {
            if record.status == DownloadStatus::Downloading {
                record.status = DownloadStatus::Queued;
                record.speed_bps = 0.0;
                record.eta_seconds = None;
                if let Ok(database) = database.lock() {
                    let _ = database.update(record);
                }
                if settings.downloads.auto_start_downloads {
                    startup_records.push(record.clone());
                }
            } else if record.status == DownloadStatus::Queued
                && settings.downloads.auto_start_downloads
            {
                startup_records.push(record.clone());
            }
        }

        let accent = utils::parse_hex_color(&settings.appearance.accent_color, Color32::from_rgb(124, 108, 255));
        theme::apply(&cc.egui_ctx, accent, settings.appearance.dark_mode);
        cc.egui_ctx.set_pixels_per_point(settings.appearance.ui_scale);
        let engine = DownloadEngine::new(Arc::clone(&database), EngineConfig::from(&settings));
        let mut app = Self {
            settings,
            database,
            engine,
            records,
            page: Page::Dashboard,
            filter: StatusFilter::All,
            sort: SortKey::Recent,
            search: String::new(),
            add_dialog: None,
            confirm_delete: None,
            pending_delete: None,
            settings_tab: SettingsTab::General,
            apps: AppsState::default(),
            notifications: NotificationCenter::default(),
            tray: TrayController::new(),
            queue_running: false,
            accent,
        };
        if !startup_records.is_empty() {
            app.queue_running = true;
            app.engine.start_queue(startup_records);
        }
        Ok(app)
    }

    fn persist(&self, record: &DownloadRecord) {
        if let Ok(database) = self.database.lock() {
            let _ = database.update(record);
        }
    }

    fn persist_settings(&mut self) {
        if let Err(error) = self.settings.save() {
            self.notifications.push(
                "Settings not saved",
                error.to_string(),
                NotificationKind::Error,
            );
        }
        self.engine
            .set_config(EngineConfig::from(&self.settings));
    }

    fn apply_appearance(&mut self, ctx: &egui::Context) {
        self.accent = utils::parse_hex_color(&self.settings.appearance.accent_color, self.accent);
        theme::apply(ctx, self.accent, self.settings.appearance.dark_mode);
        ctx.set_pixels_per_point(self.settings.appearance.ui_scale);
    }

    fn mutate_record<F>(&mut self, id: &str, f: F)
    where
        F: FnOnce(&mut DownloadRecord),
    {
        let snapshot = self
            .records
            .iter_mut()
            .find(|record| record.id == id)
            .map(|record| {
                f(record);
                record.clone()
            });
        if let Some(snapshot) = snapshot {
            self.persist(&snapshot);
        }
    }

    fn process_engine_events(&mut self) {
        while let Some(event) = self.engine.try_event() {
            match event {
                DownloadEvent::Started { id } => {
                    self.mutate_record(&id, |record| {
                        record.status = DownloadStatus::Downloading;
                        record.error = None;
                        record.speed_bps = 0.0;
                    });
                }
                DownloadEvent::Metadata {
                    id,
                    total_bytes,
                    content_type,
                } => {
                    self.mutate_record(&id, |record| {
                        record.total_bytes = total_bytes.or(record.total_bytes);
                        record.content_type = content_type.or_else(|| record.content_type.clone());
                    });
                }
                DownloadEvent::Progress {
                    id,
                    downloaded_bytes,
                    total_bytes,
                    speed_bps,
                    eta_seconds,
                } => {
                    self.mutate_record(&id, |record| {
                        record.downloaded_bytes = downloaded_bytes;
                        record.total_bytes = total_bytes.or(record.total_bytes);
                        record.speed_bps = speed_bps;
                        record.eta_seconds = eta_seconds;
                        if record.status == DownloadStatus::Queued {
                            record.status = DownloadStatus::Downloading;
                        }
                    });
                }
                DownloadEvent::Paused {
                    id,
                    downloaded_bytes,
                } => {
                    self.mutate_record(&id, |record| {
                        record.status = DownloadStatus::Paused;
                        record.downloaded_bytes = downloaded_bytes;
                        record.speed_bps = 0.0;
                        record.eta_seconds = None;
                    });
                }
                DownloadEvent::Completed { id, path } => {
                    let mut completed_name = None;
                    self.mutate_record(&id, |record| {
                        record.status = DownloadStatus::Completed;
                        record.save_path = path;
                        record.downloaded_bytes = record.total_bytes.unwrap_or(record.downloaded_bytes);
                        record.total_bytes = Some(record.downloaded_bytes);
                        record.speed_bps = 0.0;
                        record.eta_seconds = Some(0);
                        record.error = None;
                        record.completed_at = Some(Utc::now().timestamp());
                        completed_name = Some(record.file_name.clone());
                    });
                    if let Some(name) = completed_name {
                        self.notifications.push("Download complete", name, NotificationKind::Success);
                    }
                }
                DownloadEvent::Cancelled {
                    id,
                    downloaded_bytes,
                } => {
                    let should_delete = self.pending_delete.as_deref() == Some(id.as_str());
                    self.mutate_record(&id, |record| {
                        record.status = DownloadStatus::Cancelled;
                        record.downloaded_bytes = downloaded_bytes;
                        record.speed_bps = 0.0;
                        record.eta_seconds = None;
                    });
                    if should_delete {
                        self.pending_delete = None;
                        self.delete_record_immediately(&id);
                    }
                }
                DownloadEvent::Failed {
                    id,
                    downloaded_bytes,
                    error,
                } => {
                    let error_for_record = error.clone();
                    self.mutate_record(&id, |record| {
                        record.status = DownloadStatus::Failed;
                        record.downloaded_bytes = downloaded_bytes;
                        record.speed_bps = 0.0;
                        record.eta_seconds = None;
                        record.error = Some(error_for_record);
                    });
                    self.notifications.push("Download failed", error, NotificationKind::Error);
                }
            }
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let actions = self
            .tray
            .as_ref()
            .map(TrayController::actions)
            .unwrap_or_default();
        for action in actions {
            match action {
                TrayAction::Open => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::PauseAll => {
                    self.engine.pause_all();
                    self.queue_running = false;
                }
                TrayAction::ResumeAll => self.resume_all_queue(),
                TrayAction::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    fn resume_all_queue(&mut self) {
        let records = self
            .records
            .iter()
            .filter(|record| crate::queue::is_startable(record))
            .cloned()
            .collect::<Vec<_>>();
        self.queue_running = true;
        self.engine.resume_all();
        self.engine.start_queue(records);
    }

    fn handle_drop_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<String> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| {
                    if utils::is_url_like(file.name.trim()) {
                        return Some(file.name.trim().to_owned());
                    }
                    file.path.as_ref().and_then(|path| {
                        let is_url_shortcut = path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .map(|extension| extension.eq_ignore_ascii_case("url"))
                            .unwrap_or(false);
                        is_url_shortcut.then(|| utils::parse_url_file(path)).flatten()
                    })
                })
                .collect()
        });
        if let Some(url) = dropped.into_iter().find(|value| utils::is_url_like(value)) {
            self.open_add_dialog(Some(url));
        }
    }

    fn open_add_dialog(&mut self, url: Option<String>) {
        self.add_dialog = Some(AddDialog {
            url: url.unwrap_or_default(),
            file_name: String::new(),
            folder: self.settings.downloads.default_download_folder.clone(),
            checksum: String::new(),
            error: None,
        });
    }

    fn paste_url(&mut self) {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => {
                if let Some(dialog) = &mut self.add_dialog {
                    dialog.url = text.trim().to_owned();
                    dialog.error = None;
                }
            }
            Err(error) => self.notifications.push(
                "Clipboard unavailable",
                error.to_string(),
                NotificationKind::Warning,
            ),
        }
    }

    fn copy_text(&mut self, value: &str) {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(value.to_owned())) {
            Ok(()) => self.notifications.push("Copied", "URL copied to the clipboard", NotificationKind::Info),
            Err(error) => self.notifications.push("Clipboard unavailable", error.to_string(), NotificationKind::Warning),
        }
    }

    fn submit_add_dialog(&mut self) {
        let Some(mut dialog) = self.add_dialog.take() else {
            return;
        };
        let url = match utils::validate_download_url(&dialog.url) {
            Ok(url) => url,
            Err(error) => {
                dialog.error = Some(error);
                self.add_dialog = Some(dialog);
                return;
            }
        };
        let file_name = if dialog.file_name.trim().is_empty() {
            utils::guess_file_name(url.as_str())
        } else {
            utils::safe_file_name(&dialog.file_name)
        };
        if !dialog.checksum.trim().is_empty()
            && (dialog.checksum.trim().len() != 64
                || !dialog.checksum.trim().chars().all(|character| character.is_ascii_hexdigit()))
        {
            dialog.error = Some("SHA-256 must contain exactly 64 hexadecimal characters".to_owned());
            self.add_dialog = Some(dialog);
            return;
        }
        let folder = PathBuf::from(dialog.folder.trim());
        let save_path = folder.join(&file_name);
        let mut record = DownloadRecord::new(
            url.to_string(),
            file_name,
            save_path.to_string_lossy().into_owned(),
            self.settings.downloads.maximum_connections_per_download,
        );
        record.expected_sha256 = (!dialog.checksum.trim().is_empty()).then(|| dialog.checksum.trim().to_ascii_lowercase());
        if let Err(error) = self.database.lock().map_err(|_| anyhow::anyhow!("database lock is poisoned")).and_then(|database| database.insert(&record)) {
            self.notifications.push("Could not add download", error.to_string(), NotificationKind::Error);
            return;
        }
        let should_start = self.settings.downloads.auto_start_downloads;
        self.records.insert(0, record.clone());
        self.notifications.push("Download added", record.file_name.clone(), NotificationKind::Info);
        if should_start {
            self.queue_running = true;
            self.engine.start(record);
        }
    }

    fn download_app(&mut self, item: AppItem) {
        let Ok(url) = utils::validate_download_url(&item.download_url) else {
            self.notifications.push("App cannot be downloaded", "The catalog contains an invalid URL", NotificationKind::Error);
            return;
        };
        let mut file_name = utils::guess_file_name(url.as_str());
        if file_name == "download.bin" {
            file_name = utils::safe_file_name(&format!("{}-{}", item.name, item.version));
        }
        let folder = self.settings.default_folder_path();
        let mut record = DownloadRecord::new(
            url.to_string(),
            file_name.clone(),
            folder.join(&file_name).to_string_lossy().into_owned(),
            self.settings.downloads.maximum_connections_per_download,
        );
        record.total_bytes = item.size_bytes;
        record.expected_sha256 = item.sha256.clone();
        if let Err(error) = self.database.lock().map_err(|_| anyhow::anyhow!("database lock is poisoned")).and_then(|database| database.insert(&record)) {
            self.notifications.push("Could not add app", error.to_string(), NotificationKind::Error);
            return;
        }
        self.records.insert(0, record.clone());
        self.notifications.push("App added to downloads", file_name, NotificationKind::Info);
        if self.settings.downloads.auto_start_downloads {
            self.queue_running = true;
            self.engine.start(record);
        }
        self.page = Page::Dashboard;
    }

    fn find_record_mut(&mut self, id: &str) -> Option<&mut DownloadRecord> {
        self.records.iter_mut().find(|record| record.id == id)
    }

    fn find_record(&self, id: &str) -> Option<&DownloadRecord> {
        self.records.iter().find(|record| record.id == id)
    }

    fn handle_record_action(&mut self, id: &str, action: RecordAction) {
        let Some(record) = self.find_record(id).cloned() else {
            return;
        };
        match action {
            RecordAction::Start | RecordAction::Resume => {
                let mut record = record;
                record.status = DownloadStatus::Queued;
                record.error = None;
                self.persist(&record);
                if let Some(current) = self.find_record_mut(id) {
                    *current = record.clone();
                }
                self.queue_running = true;
                self.engine.resume(record);
            }
            RecordAction::Pause => self.engine.pause(id),
            RecordAction::Cancel => self.engine.cancel(id),
            RecordAction::Retry => {
                let mut record = record;
                record.status = DownloadStatus::Queued;
                record.error = None;
                record.speed_bps = 0.0;
                record.eta_seconds = None;
                self.persist(&record);
                if let Some(current) = self.find_record_mut(id) {
                    *current = record.clone();
                }
                self.queue_running = true;
                self.engine.start(record);
            }
            RecordAction::Open => {
                if let Err(error) = system::open_file(record.destination()) {
                    self.notifications.push("Cannot open file", error.to_string(), NotificationKind::Error);
                }
            }
            RecordAction::Folder => {
                if let Err(error) = system::open_folder(record.destination()) {
                    self.notifications.push("Cannot open folder", error.to_string(), NotificationKind::Error);
                }
            }
            RecordAction::Copy => self.copy_text(&record.url),
            RecordAction::Delete => {
                if self.settings.general.confirm_before_deleting {
                    self.confirm_delete = Some(id.to_owned());
                } else {
                    self.delete_record(id);
                }
            }
        }
    }

    fn delete_record(&mut self, id: &str) {
        let Some(record) = self.find_record(id).cloned() else {
            return;
        };
        if record.status.is_active() {
            self.pending_delete = Some(id.to_owned());
            self.engine.cancel(id);
            self.notifications.push("Stopping download", "The item will be removed when its worker exits", NotificationKind::Info);
            return;
        }
        self.delete_record_immediately(id);
    }

    fn delete_record_immediately(&mut self, id: &str) {
        if let Some(record) = self.find_record(id).cloned() {
            let partial = utils::partial_directory(record.destination(), &record.id);
            let _ = std::fs::remove_dir_all(partial);
            if let Ok(database) = self.database.lock() {
                let _ = database.delete(id);
            }
            self.records.retain(|item| item.id != id);
        }
    }

    fn set_priority(&mut self, id: &str, direction: i32) {
        let mut queue = self
            .records
            .iter()
            .filter(|record| crate::queue::is_waiting(record))
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let Some(index) = queue.iter().position(|value| value == id) else {
            return;
        };
        let next = if direction < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(queue.len().saturating_sub(1))
        };
        if index == next {
            return;
        }
        queue.swap(index, next);
        for (position, queue_id) in queue.iter().enumerate() {
            let priority = (queue.len() - position) as i32;
            self.mutate_record(queue_id, |record| record.priority = priority);
        }
    }

    fn start_queue(&mut self) {
        let mut queue = self
            .records
            .iter()
            .filter(|record| crate::queue::is_startable(record))
            .cloned()
            .collect::<Vec<_>>();
        crate::queue::sort_by_priority(&mut queue);
        for record in &mut queue {
            record.status = DownloadStatus::Queued;
            record.error = None;
            let snapshot = record.clone();
            self.mutate_record(&record.id, |current| *current = snapshot);
        }
        self.queue_running = true;
        self.engine.start_queue(queue);
    }

    fn visible_records(&self) -> Vec<DownloadRecord> {
        let query = self.search.trim().to_lowercase();
        let mut records = self
            .records
            .iter()
            .filter(|record| {
                let filter_matches = match self.filter {
                    StatusFilter::All => true,
                    StatusFilter::Active => matches!(record.status, DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused),
                    StatusFilter::Completed => record.status == DownloadStatus::Completed,
                    StatusFilter::Failed => record.status == DownloadStatus::Failed,
                };
                let search_matches = query.is_empty()
                    || record.file_name.to_lowercase().contains(&query)
                    || record.url.to_lowercase().contains(&query);
                filter_matches && search_matches
            })
            .cloned()
            .collect::<Vec<_>>();
        match self.sort {
            SortKey::Recent => records.sort_by_key(|record| std::cmp::Reverse(record.created_at)),
            SortKey::Name => records.sort_by(|left, right| left.file_name.to_lowercase().cmp(&right.file_name.to_lowercase())),
            SortKey::Size => records.sort_by(|left, right| right.total_bytes.unwrap_or(0).cmp(&left.total_bytes.unwrap_or(0))),
            SortKey::Status => records.sort_by(|left, right| left.status.label().cmp(right.status.label())),
        }
        records
    }

    /// Resolved color tokens for the current theme and accent color.
    fn tokens(&self) -> theme::Tokens {
        theme::tokens(self.settings.appearance.dark_mode, self.accent)
    }

    /// Navigation shell variant that fits the current window size.
    fn layout_mode(&self, ctx: &egui::Context) -> LayoutMode {
        let width = ctx.screen_rect().width();
        if width >= 1180.0 {
            LayoutMode::Wide
        } else if width >= 940.0 {
            LayoutMode::Compact
        } else {
            LayoutMode::Narrow
        }
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.settings.appearance.dark_mode = !self.settings.appearance.dark_mode;
        self.apply_appearance(ctx);
        self.persist_settings();
    }

    fn open_download_folder(&mut self) {
        let folder = self.settings.default_folder_path();
        if let Err(error) = system::open_folder(&folder) {
            self.notifications
                .push("Cannot open folder", error.to_string(), NotificationKind::Error);
        }
    }

    /// Optional keyboard shortcuts. Every shortcut mirrors a visible button.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (new_download, escape) = ctx.input(|input| {
            (
                input.modifiers.command && input.key_pressed(egui::Key::N),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if new_download && self.add_dialog.is_none() {
            self.open_add_dialog(None);
        }
        if escape {
            self.add_dialog = None;
            self.confirm_delete = None;
            self.notifications.open = false;
        }
    }

    fn render_sidebar(&mut self, ctx: &egui::Context, mode: LayoutMode) {
        let t = self.tokens();
        let rail = mode == LayoutMode::Compact;
        let downloads = self.records.len();
        let queue_count = self.queue_count();
        let completed_count = self.completed_count();
        let failed_count = self.failed_count();
        let width = if rail { 78.0 } else { 252.0 };

        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(width)
            .frame(
                Frame::none()
                    .fill(t.sidebar)
                    .inner_margin(Margin::symmetric(if rail { 12.0 } else { 16.0 }, 18.0)),
            )
            .show(ctx, |ui| {
                if rail {
                    ui.vertical_centered(|ui| {
                        brand_mark(ui, t.accent, 40.0);
                    });
                } else {
                    ui.horizontal(|ui| {
                        brand_mark(ui, t.accent, 38.0);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("PULSE").size(15.0).strong());
                            ui.label(t.faint_text("Download manager"));
                        });
                    });
                    ui.add_space(18.0);
                    let full = ui.available_width();
                    if ui
                        .add_sized([full, 32.0], t.primary_button("＋   Add download"))
                        .clicked()
                    {
                        self.open_add_dialog(None);
                    }
                    ui.add_space(6.0);
                    if ui
                        .add_sized([full, 30.0], t.subtle_button("Open downloads folder"))
                        .clicked()
                    {
                        self.open_download_folder();
                    }
                }

                ui.add_space(18.0);
                if !rail {
                    ui.label(t.faint_text("WORKSPACE"));
                    ui.add_space(4.0);
                }
                self.nav_item(ui, &t, mode, Page::Dashboard, Some(downloads));
                self.nav_item(ui, &t, mode, Page::Queue, Some(queue_count));
                self.nav_item(ui, &t, mode, Page::Apps, None);
                ui.add_space(14.0);
                if !rail {
                    ui.label(t.faint_text("LIBRARY"));
                    ui.add_space(4.0);
                }
                self.nav_item(ui, &t, mode, Page::Completed, Some(completed_count));
                self.nav_item(ui, &t, mode, Page::Failed, Some(failed_count));
                ui.add_space(14.0);
                self.nav_item(ui, &t, mode, Page::Settings, None);

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if rail {
                        ui.add_space(6.0);
                        ui.vertical_centered(|ui| {
                            ui.label(t.faint_text("v0.1.0"));
                        });
                    } else {
                        ui.add_space(10.0);
                        ui.label(t.faint_text("Version 0.1.0 · MIT"));
                        ui.label(t.faint_text("Native Rust · egui · no webview"));
                        ui.add_space(6.0);
                        ui.separator();
                    }
                });
            });
    }

    fn nav_item(
        &mut self,
        ui: &mut Ui,
        t: &theme::Tokens,
        mode: LayoutMode,
        page: Page,
        count: Option<usize>,
    ) {
        let rail = mode == LayoutMode::Compact;
        let selected = self.page == page;
        let label = if rail {
            page.icon().to_owned()
        } else {
            format!("{}      {}", page.icon(), page.title())
        };
        let text = RichText::new(label)
            .size(if rail { 16.0 } else { 13.0 })
            .color(if selected { t.text } else { t.muted });
        let height = if rail { 40.0 } else { 36.0 };
        let response = ui.add_sized(
            [ui.available_width(), height],
            egui::SelectableLabel::new(selected, text),
        );

        if let Some(count) = count.filter(|count| *count > 0) {
            let rect = response.rect;
            if rail {
                let center = egui::pos2(rect.right() - 10.0, rect.top() + 10.0);
                ui.painter().circle_filled(center, 8.5, t.accent);
                ui.painter().text(
                    center,
                    Align2::CENTER_CENTER,
                    count.to_string(),
                    FontId::proportional(10.0),
                    Color32::WHITE,
                );
            } else {
                ui.painter().text(
                    egui::pos2(rect.right() - 14.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    count.to_string(),
                    FontId::proportional(11.0),
                    if selected { t.text } else { t.faint },
                );
            }
        }

        let hint = if rail { page.title() } else { page.subtitle() };
        if response.on_hover_text(hint).clicked() {
            self.page = page;
            if page == Page::Apps && self.apps.items.is_empty() && !self.apps.loading {
                self.apps.refresh(&self.settings.apps_api_url);
            }
        }
    }

    fn render_topbar(&mut self, ctx: &egui::Context, mode: LayoutMode) {
        let t = self.tokens();
        let unread = self.notifications.items.len();
        egui::TopBottomPanel::top("topbar")
            .frame(
                Frame::none()
                    .fill(t.bg)
                    .inner_margin(Margin::symmetric(20.0, 14.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::heading(self.page.title()));
                        if mode != LayoutMode::Narrow {
                            ui.label(t.muted_text(self.page.subtitle()));
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let add_label = if mode == LayoutMode::Narrow {
                            "＋"
                        } else {
                            "＋   Add download"
                        };
                        if ui
                            .add(t.primary_button(add_label))
                            .on_hover_text("Add download (Ctrl+N)")
                            .clicked()
                        {
                            self.open_add_dialog(None);
                        }
                        let theme_icon = if t.dark { "☀" } else { "☾" };
                        if ui
                            .add(t.subtle_button(theme_icon))
                            .on_hover_text("Switch between dark and light")
                            .clicked()
                        {
                            self.toggle_theme(ctx);
                        }
                        let bell = if unread > 0 {
                            format!("🔔  {unread}")
                        } else {
                            "🔔".to_owned()
                        };
                        if ui
                            .add(t.subtle_button(bell))
                            .on_hover_text("Notifications")
                            .clicked()
                        {
                            self.notifications.open = !self.notifications.open;
                        }
                        ui.add_space(6.0);
                        let search_width = ui.available_width().min(420.0).max(120.0);
                        ui.add_sized(
                            [search_width, 32.0],
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("Search downloads…")
                                .margin(Vec2::new(10.0, 6.0)),
                        );
                    });
                });
                if mode == LayoutMode::Narrow {
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        for page in [
                            Page::Dashboard,
                            Page::Queue,
                            Page::Apps,
                            Page::Completed,
                            Page::Failed,
                            Page::Settings,
                        ] {
                            let selected = self.page == page;
                            let text = RichText::new(format!("{}   {}", page.icon(), page.title()))
                                .size(12.0)
                                .color(if selected { t.text } else { t.muted });
                            if ui.add(egui::SelectableLabel::new(selected, text)).clicked() {
                                self.page = page;
                            }
                        }
                    });
                }
            });
    }

    fn render_dashboard(&mut self, ctx: &egui::Context) {
        let t = self.tokens();
        egui::CentralPanel::default()
            .frame(Frame::none().fill(t.bg))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("downloads-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(1240.0));
                        self.render_stats(ui, &t);
                        ui.add_space(theme::gap());
                        self.render_filter_bar(ui, &t);
                        ui.add_space(theme::gap());
                        let records = self.visible_records();
                        if records.is_empty() {
                            empty_state(
                                ui,
                                &t,
                                "Nothing to show",
                                "Adjust the filters, or add a download to get started.",
                            );
                        } else {
                            let mut action = None;
                            for record in &records {
                                if let Some(next) = self.render_download_card(ui, &t, record) {
                                    action = Some((record.id.clone(), next));
                                }
                                ui.add_space(theme::gap());
                            }
                            if let Some((id, action)) = action {
                                self.handle_record_action(&id, action);
                            }
                        }
                        ui.add_space(24.0);
                    });
            });
    }

    fn render_stats(&self, ui: &mut Ui, t: &theme::Tokens) {
        let active = self
            .records
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    DownloadStatus::Downloading | DownloadStatus::Queued
                )
            })
            .count();
        let completed = self
            .records
            .iter()
            .filter(|record| record.status == DownloadStatus::Completed)
            .count();
        let total = self.records.len();
        let speed = self
            .records
            .iter()
            .filter(|record| record.status == DownloadStatus::Downloading)
            .map(|record| record.speed_bps)
            .sum::<f64>();
        let stats = [
            ("All downloads", total.to_string(), "Tracked locally", t.info),
            ("Active now", active.to_string(), "Running or queued", t.accent),
            ("Completed", completed.to_string(), "Ready to open", t.success),
            (
                "Current speed",
                utils::format_speed(speed),
                "Combined throughput",
                t.warning,
            ),
        ];
        let columns = responsive_columns(ui.available_width(), 1240.0, 4);
        ui.columns(columns, |column_uis| {
            for (index, (label, value, caption, color)) in stats.into_iter().enumerate() {
                let column = index % columns;
                stat_card(&mut column_uis[column], t, label, &value, caption, color);
                column_uis[column].add_space(theme::gap());
            }
        });
    }

    fn render_filter_bar(&mut self, ui: &mut Ui, t: &theme::Tokens) {
        let all = self.records.len();
        let active = self
            .records
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused
                )
            })
            .count();
        let completed = self.completed_count();
        let failed = self.failed_count();
        let filters = [
            (StatusFilter::All, "All", all),
            (StatusFilter::Active, "Active", active),
            (StatusFilter::Completed, "Completed", completed),
            (StatusFilter::Failed, "Failed", failed),
        ];
        ui.horizontal_wrapped(|ui| {
            for (filter, label, count) in filters {
                let selected = self.filter == filter;
                let text = RichText::new(format!("{label}   {count}"))
                    .size(12.5)
                    .color(if selected { t.text } else { t.muted });
                if ui
                    .add(egui::SelectableLabel::new(selected, text))
                    .clicked()
                {
                    self.filter = filter;
                    self.page = match filter {
                        StatusFilter::Completed => Page::Completed,
                        StatusFilter::Failed => Page::Failed,
                        _ => Page::Dashboard,
                    };
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("sort-downloads")
                    .selected_text(sort_label(self.sort))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.sort, SortKey::Recent, "Newest first");
                        ui.selectable_value(&mut self.sort, SortKey::Name, "Name");
                        ui.selectable_value(&mut self.sort, SortKey::Size, "Size");
                        ui.selectable_value(&mut self.sort, SortKey::Status, "Status");
                    });
                ui.label(t.muted_text("Sort by"));
            });
        });
    }

    fn render_download_card(
        &self,
        ui: &mut Ui,
        t: &theme::Tokens,
        record: &DownloadRecord,
    ) -> Option<RecordAction> {
        let mut action = None;
        let status_color = t.status(record.status);
        let stacked_actions = ui.available_width() < 720.0;
        t.card().show(ui, |ui| {
            ui.horizontal(|ui| {
                file_badge(ui, record);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(truncate(&record.file_name, 58)).size(14.5).strong());
                    ui.label(t.faint_text(truncate(&record.url, 76)));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        t.pill(theme::status_label(record.status, status_color), status_color),
                    );
                });
            });
            ui.add_space(12.0);
            let progress = record.progress();
            let percent = format!("{:.0}%", progress * 100.0);
            ui.horizontal(|ui| {
                let bar_width = (ui.available_width() - 56.0).max(90.0);
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(bar_width)
                        .desired_height(6.0)
                        .fill(status_color),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(t.muted_text(percent));
                });
            });
            ui.add_space(10.0);
            if stacked_actions {
                ui.horizontal_wrapped(|ui| self.meta_row(ui, t, record));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| self.action_row(ui, t, record, &mut action));
            } else {
                ui.horizontal(|ui| {
                    self.meta_row(ui, t, record);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.action_row(ui, t, record, &mut action);
                    });
                });
            }
            if let Some(error) = &record.error {
                ui.add_space(10.0);
                t.inset_frame().show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("⚠").color(t.danger));
                        ui.label(
                            t.faint_text(format!("Error: {}", truncate(error, 160)))
                                .color(t.danger),
                        );
                    });
                });
            }
        });
        action
    }

    fn meta_row(&self, ui: &mut Ui, t: &theme::Tokens, record: &DownloadRecord) {
        let size = match record.total_bytes {
            Some(total) => format!(
                "{} / {}",
                utils::format_bytes(record.downloaded_bytes),
                utils::format_bytes(total)
            ),
            None => format!("{} downloaded", utils::format_bytes(record.downloaded_bytes)),
        };
        ui.label(t.faint_text(size));
        meta_separator(ui, t);
        ui.label(t.faint_text(format!("Speed {}", utils::format_speed(record.speed_bps))));
        meta_separator(ui, t);
        ui.label(t.faint_text(format!("ETA {}", utils::format_eta(record.eta_seconds))));
        meta_separator(ui, t);
        ui.label(t.faint_text(format!("Priority {}", record.priority.max(0))));
        if record.connections > 1 {
            meta_separator(ui, t);
            ui.label(t.faint_text(format!("{} connections", record.connections)));
        }
    }

    fn action_row(
        &self,
        ui: &mut Ui,
        t: &theme::Tokens,
        record: &DownloadRecord,
        action: &mut Option<RecordAction>,
    ) {
        match record.status {
            DownloadStatus::Downloading | DownloadStatus::Queued => {
                if ui
                    .add(t.primary_button("Pause"))
                    .on_hover_text("Pause this download")
                    .clicked()
                {
                    *action = Some(RecordAction::Pause);
                }
                if ui
                    .add(t.subtle_button("Cancel"))
                    .on_hover_text("Cancel and keep the partial data")
                    .clicked()
                {
                    *action = Some(RecordAction::Cancel);
                }
            }
            DownloadStatus::Paused => {
                if ui
                    .add(t.primary_button("Resume"))
                    .on_hover_text("Resume from the saved segments")
                    .clicked()
                {
                    *action = Some(RecordAction::Resume);
                }
                if ui.add(t.subtle_button("Cancel")).clicked() {
                    *action = Some(RecordAction::Cancel);
                }
            }
            DownloadStatus::Completed => {
                if ui
                    .add(t.primary_button("Open file"))
                    .on_hover_text("Open the finished file with its default application")
                    .clicked()
                {
                    *action = Some(RecordAction::Open);
                }
                if ui
                    .add(t.subtle_button("Open folder"))
                    .on_hover_text("Reveal the file in Explorer")
                    .clicked()
                {
                    *action = Some(RecordAction::Folder);
                }
            }
            DownloadStatus::Failed | DownloadStatus::Cancelled => {
                if ui
                    .add(t.primary_button("Retry"))
                    .on_hover_text("Start this download again")
                    .clicked()
                {
                    *action = Some(RecordAction::Retry);
                }
            }
        }
        if ui
            .add(t.subtle_button("Copy URL"))
            .on_hover_text("Copy the source URL to the clipboard")
            .clicked()
        {
            *action = Some(RecordAction::Copy);
        }
        if ui
            .add(t.subtle_button("Delete"))
            .on_hover_text("Remove this entry from Pulse")
            .clicked()
        {
            *action = Some(RecordAction::Delete);
        }
    }

    fn render_queue(&mut self, ctx: &egui::Context) {
        let t = self.tokens();
        let active = self
            .records
            .iter()
            .filter(|record| record.status == DownloadStatus::Downloading)
            .count();
        let limit = self.settings.downloads.download_speed_limit_kbps;
        let limit_label = if limit == 0 {
            "Unlimited speed".to_owned()
        } else {
            format!("{limit} KB/s limit")
        };
        let mut queued = self
            .records
            .iter()
            .filter(|record| crate::queue::is_waiting(record))
            .cloned()
            .collect::<Vec<_>>();
        queued.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
        });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(t.bg))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("queue-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(1180.0));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(theme::card_title("Queue control"));
                                ui.label(t.muted_text(format!(
                                    "{} waiting · {} running · {} slots",
                                    self.queue_count(),
                                    active,
                                    self.settings.downloads.maximum_simultaneous_downloads
                                )));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(t.subtle_button("Stop queue")).clicked() {
                                    self.queue_running = false;
                                    self.engine.stop_queue();
                                }
                                if ui.add(t.subtle_button("Pause queue")).clicked() {
                                    self.queue_running = false;
                                    self.engine.pause_all();
                                }
                                if ui.add(t.primary_button("Start queue")).clicked() {
                                    self.start_queue();
                                }
                            });
                        });
                        ui.add_space(theme::gap());
                        t.inset_frame().show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                status_dot(
                                    ui,
                                    if self.queue_running {
                                        t.success
                                    } else {
                                        t.warning
                                    },
                                );
                                ui.label(
                                    RichText::new(if self.queue_running {
                                        "Queue is running"
                                    } else {
                                        "Queue is paused"
                                    })
                                    .size(12.5)
                                    .strong(),
                                );
                                meta_separator(ui, &t);
                                ui.label(t.muted_text(format!(
                                    "{} waiting",
                                    self.queue_count()
                                )));
                                meta_separator(ui, &t);
                                ui.label(t.muted_text(limit_label.clone()));
                                meta_separator(ui, &t);
                                ui.label(t.muted_text("Order uses priority, then age"));
                            });
                        });
                        ui.add_space(theme::gap());

                        if queued.is_empty() {
                            empty_state(
                                ui,
                                &t,
                                "Queue is clear",
                                "New downloads appear here before they start.",
                            );
                        } else {
                            let mut move_action = None;
                            let mut record_action = None;
                            for (index, record) in queued.iter().enumerate() {
                                t.card().show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{:02}", index + 1))
                                                .size(15.0)
                                                .strong()
                                                .color(t.accent),
                                        );
                                        ui.add_space(8.0);
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(truncate(&record.file_name, 54))
                                                    .strong(),
                                            );
                                            ui.label(t.faint_text(format!(
                                                "{} · {} · Priority {}",
                                                record.status.label(),
                                                utils::format_bytes(
                                                    record.total_bytes.unwrap_or(0)
                                                ),
                                                record.priority.max(0)
                                            )));
                                        });
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui.add(t.subtle_button("Delete")).clicked() {
                                                record_action = Some((
                                                    record.id.clone(),
                                                    RecordAction::Delete,
                                                ));
                                            }
                                            if record.status == DownloadStatus::Failed
                                                && ui.add(t.subtle_button("Retry")).clicked()
                                            {
                                                record_action = Some((
                                                    record.id.clone(),
                                                    RecordAction::Retry,
                                                ));
                                            }
                                            if ui
                                                .add(t.subtle_button("↓"))
                                                .on_hover_text("Move down the queue")
                                                .clicked()
                                            {
                                                move_action = Some((record.id.clone(), 1));
                                            }
                                            if ui
                                                .add(t.subtle_button("↑"))
                                                .on_hover_text("Move up the queue")
                                                .clicked()
                                            {
                                                move_action = Some((record.id.clone(), -1));
                                            }
                                        });
                                    });
                                });
                                ui.add_space(theme::gap());
                            }
                            if let Some((id, direction)) = move_action {
                                self.set_priority(&id, direction);
                            }
                            if let Some((id, action)) = record_action {
                                self.handle_record_action(&id, action);
                            }
                        }
                        ui.add_space(24.0);
                    });
            });
    }

    fn render_apps(&mut self, ctx: &egui::Context) {
        if self.apps.items.is_empty() && !self.apps.loading && self.apps.error.is_none() {
            self.apps.refresh(&self.settings.apps_api_url);
        }
        let t = self.tokens();
        let accent = self.accent;
        egui::CentralPanel::default()
            .frame(Frame::none().fill(t.bg))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("apps-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(1240.0));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(theme::card_title(format!(
                                    "{} apps in this catalog",
                                    self.apps.items.len()
                                )));
                                let updated = self
                                    .apps
                                    .last_updated
                                    .clone()
                                    .unwrap_or_else(|| "not refreshed yet".to_owned());
                                ui.label(t.muted_text(format!("Last catalog refresh: {updated}")));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(t.subtle_button("Refresh catalog")).clicked() {
                                    self.apps.refresh(&self.settings.apps_api_url);
                                }
                                if self.apps.loading {
                                    ui.spinner();
                                }
                            });
                        });
                        ui.add_space(theme::gap());
                        if let Some(error) = self.apps.error.clone() {
                            danger_banner(
                                ui,
                                &t,
                                "Catalog unavailable",
                                &error,
                                "Check the Apps API endpoint in Settings → General.",
                            );
                        } else if self.apps.loading && self.apps.items.is_empty() {
                            empty_state(
                                ui,
                                &t,
                                "Loading catalog",
                                "Fetching the latest app metadata…",
                            );
                        } else if self.apps.items.is_empty() {
                            empty_state(
                                ui,
                                &t,
                                "No apps published",
                                "The catalog API returned an empty list.",
                            );
                        } else {
                            let query = self.search.trim().to_lowercase();
                            let items = self
                                .apps
                                .items
                                .iter()
                                .filter(|item| {
                                    query.is_empty()
                                        || item.name.to_lowercase().contains(&query)
                                        || item.category.to_lowercase().contains(&query)
                                        || item.developer.to_lowercase().contains(&query)
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            let columns = responsive_columns(ui.available_width(), 1240.0, 3);
                            let mut download = None;
                            let mut details = None;
                            ui.columns(columns, |column_uis| {
                                for (index, item) in items.iter().enumerate() {
                                    let column = index % columns;
                                    let (download_clicked, details_clicked) =
                                        render_app_card(&mut column_uis[column], &t, item, accent);
                                    if download_clicked {
                                        download = Some(item.clone());
                                    }
                                    if details_clicked {
                                        details = self
                                            .apps
                                            .items
                                            .iter()
                                            .position(|value| value.id == item.id);
                                    }
                                    column_uis[column].add_space(theme::gap());
                                }
                            });
                            if let Some(item) = download {
                                self.download_app(item);
                            }
                            if let Some(index) = details {
                                self.apps.selected = Some(index);
                            }
                        }
                        ui.add_space(24.0);
                    });
            });
        self.render_app_details(ctx);
    }

    fn render_app_details(&mut self, ctx: &egui::Context) {
        let Some(index) = self.apps.selected else {
            return;
        };
        if index >= self.apps.items.len() {
            self.apps.selected = None;
            return;
        }
        let t = self.tokens();
        let item = self.apps.items[index].clone();
        let mut close = false;
        let mut download = false;
        Window::new("App details")
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    app_icon(ui, &item.name, t.accent, 54.0);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(item.name.as_str()).size(20.0).strong());
                        ui.label(t.muted_text(format!(
                            "{} · {} · {}",
                            item.category,
                            item.version,
                            item.size_label()
                        )));
                    });
                });
                ui.add_space(14.0);
                ui.label(item.short_description.as_str());
                ui.add_space(12.0);
                t.inset_frame().show(ui, |ui| {
                    detail_row(ui, &t, "Developer", &item.developer);
                    detail_row(ui, &t, "Updated", &item.updated_at);
                    detail_row(ui, &t, "Catalog id", &item.id);
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.add(t.primary_button("Download")).clicked() {
                        download = true;
                    }
                    if ui.add(t.subtle_button("Close")).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.apps.selected = None;
        }
        if download {
            self.download_app(item);
            self.apps.selected = None;
        }
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        let t = self.tokens();
        egui::CentralPanel::default()
            .frame(Frame::none().fill(t.bg))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(880.0));
                        ui.horizontal_wrapped(|ui| {
                            for (tab, label) in [
                                (SettingsTab::General, "General"),
                                (SettingsTab::Downloads, "Downloads"),
                                (SettingsTab::Network, "Network"),
                                (SettingsTab::Appearance, "Appearance"),
                            ] {
                                if ui
                                    .add(egui::SelectableLabel::new(
                                        self.settings_tab == tab,
                                        RichText::new(label).size(12.5),
                                    ))
                                    .clicked()
                                {
                                    self.settings_tab = tab;
                                }
                            }
                        });
                        ui.add_space(theme::gap());
                        let mut changed = false;
                        match self.settings_tab {
                            SettingsTab::General => changed |= self.settings_general(ui, &t),
                            SettingsTab::Downloads => changed |= self.settings_downloads(ui, &t),
                            SettingsTab::Network => changed |= self.settings_network(ui, &t),
                            SettingsTab::Appearance => {
                                changed |= self.settings_appearance(ui, ctx, &t);
                            }
                        }
                        if changed {
                            self.persist_settings();
                        }
                        ui.add_space(24.0);
                    });
            });
    }

    fn settings_general(&mut self, ui: &mut Ui, t: &theme::Tokens) -> bool {
        let mut changed = false;
        settings_section(ui, t, "General", "Startup behavior and the catalog source.", |ui| {
            let mut start = self.settings.general.start_with_windows;
            if ui.checkbox(&mut start, "Start Pulse with Windows").changed() {
                match system::set_start_with_windows(start) {
                    Ok(()) => {
                        self.settings.general.start_with_windows = start;
                        changed = true;
                    }
                    Err(error) => self.notifications.push(
                        "Startup setting unavailable",
                        error.to_string(),
                        NotificationKind::Warning,
                    ),
                }
            }
            changed |= ui
                .checkbox(
                    &mut self.settings.general.minimize_to_tray,
                    "Keep Pulse in the system tray when the window closes",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.settings.general.confirm_before_deleting,
                    "Ask before removing a download entry",
                )
                .changed();
            ui.add_space(10.0);
            settings_label(ui, t, "Language");
            let language = egui::ComboBox::from_id_salt("language")
                .selected_text(&self.settings.general.language)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.settings.general.language,
                        "English".to_owned(),
                        "English",
                    )
                });
            changed |= language
                .inner
                .map(|response| response.changed())
                .unwrap_or(false);
            ui.add_space(10.0);
            settings_label(ui, t, "Apps catalog endpoint");
            changed |= ui.text_edit_singleline(&mut self.settings.apps_api_url).changed();
            ui.label(t.faint_text(
                "A GET endpoint returning an array or { \"apps\": [ … ] } of catalog items.",
            ));
        });
        changed
    }

    fn settings_downloads(&mut self, ui: &mut Ui, t: &theme::Tokens) -> bool {
        let mut changed = false;
        settings_section(ui, t, "Downloads", "Storage, concurrency and automatic starting.", |ui| {
            settings_label(ui, t, "Default folder");
            ui.horizontal(|ui| {
                let field_width = (ui.available_width() - 104.0).max(140.0);
                let field = egui::TextEdit::singleline(
                    &mut self.settings.downloads.default_download_folder,
                );
                changed |= ui.add_sized([field_width, 30.0], field).changed();
                if ui.add(t.subtle_button("Choose…")).clicked() {
                    let current = self.settings.default_folder_path();
                    let picked = rfd::FileDialog::new().set_directory(current).pick_folder();
                    if let Some(folder) = picked {
                        self.settings.set_download_folder(folder);
                        changed = true;
                    }
                }
            });
            ui.add_space(10.0);
            changed |= ui
                .checkbox(
                    &mut self.settings.downloads.auto_start_downloads,
                    "Start downloads as soon as they are added",
                )
                .changed();
            ui.add_space(6.0);
            changed |= ui
                .add(egui::Slider::new(
                    &mut self.settings.downloads.maximum_simultaneous_downloads,
                    1..=16,
                ).text("Simultaneous downloads"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(
                    &mut self.settings.downloads.maximum_connections_per_download,
                    1..=16,
                ).text("Connections per download"))
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.settings.downloads.download_speed_limit_kbps)
                        .speed(64.0)
                        .suffix(" KB/s (0 = unlimited)"),
                )
                .changed();
        });
        changed
    }

    fn settings_network(&mut self, ui: &mut Ui, t: &theme::Tokens) -> bool {
        let mut changed = false;
        settings_section(ui, t, "Network", "Timeouts, retries, proxy and request headers.", |ui| {
            changed |= ui
                .add(egui::Slider::new(
                    &mut self.settings.network.connection_timeout_seconds,
                    5..=600,
                ).text("Connection timeout (seconds)"))
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.network.retry_count, 0..=20)
                        .text("Automatic retries"),
                )
                .changed();
            ui.add_space(10.0);
            settings_label(ui, t, "Proxy");
            changed |= ui
                .text_edit_singleline(&mut self.settings.network.proxy)
                .on_hover_text("Example: http://user:password@proxy.example:8080")
                .changed();
            ui.label(t.faint_text("Leave empty to use the system network defaults."));
            ui.add_space(10.0);
            settings_label(ui, t, "User-Agent");
            changed |= ui
                .text_edit_singleline(&mut self.settings.network.user_agent)
                .changed();
            ui.add_space(10.0);
            settings_label(ui, t, "Additional headers");
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut self.settings.network.additional_headers)
                        .desired_rows(3)
                        .hint_text("One header per line: X-Token: value"),
                )
                .changed();
        });
        changed
    }

    fn settings_appearance(&mut self, ui: &mut Ui, ctx: &egui::Context, t: &theme::Tokens) -> bool {
        let mut changed = false;
        settings_section(ui, t, "Appearance", "Theme, accent color and interface scale.", |ui| {
            if ui
                .checkbox(&mut self.settings.appearance.dark_mode, "Dark theme (recommended)")
                .changed()
            {
                self.apply_appearance(ctx);
                changed = true;
            }
            ui.add_space(8.0);
            settings_label(ui, t, "Accent color");
            ui.horizontal(|ui| {
                let mut color = self.accent;
                if ui.color_edit_button_srgba(&mut color).changed() {
                    self.accent = color;
                    self.settings.appearance.accent_color =
                        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
                    self.apply_appearance(ctx);
                    changed = true;
                }
                if ui
                    .add_sized(
                        [110.0, 28.0],
                        egui::TextEdit::singleline(&mut self.settings.appearance.accent_color),
                    )
                    .changed()
                {
                    self.apply_appearance(ctx);
                    changed = true;
                }
                ui.label(t.faint_text("Hex value, for example #7C6CFF"));
            });
            ui.add_space(10.0);
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.appearance.ui_scale, 0.8..=1.5)
                        .text("Interface scale"),
                )
                .changed();
            if changed {
                self.apply_appearance(ctx);
            }
        });
        changed
    }

    fn render_notifications(&mut self, ctx: &egui::Context) {
        if !self.notifications.open {
            return;
        }
        let t = self.tokens();
        let items = self
            .notifications
            .items
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>();
        let mut clear = false;
        let mut close = false;
        egui::Area::new("notification-area".into())
            .anchor(Align2::RIGHT_TOP, egui::vec2(-20.0, 78.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(348.0);
                Frame::none()
                    .fill(t.panel)
                    .stroke(Stroke::new(1.0, t.border_strong))
                    .rounding(Rounding::same(theme::RADIUS_LG))
                    .inner_margin(Margin::same(14.0))
                    .shadow(egui::Shadow {
                        offset: egui::vec2(0.0, 14.0),
                        blur: 34.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(if t.dark { 120 } else { 40 }),
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(theme::card_title(format!(
                                "Notifications ({})",
                                self.notifications.items.len()
                            )));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(t.subtle_button("Clear all")).clicked() {
                                    clear = true;
                                }
                                if ui.add(t.subtle_button("Close")).clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.add_space(6.0);
                        if items.is_empty() {
                            ui.label(t.muted_text("You're all caught up."));
                        } else {
                            for notification in &items {
                                let color = theme::notification_color(notification.kind, &t);
                                notification_card(ui, &t, notification, color);
                                ui.add_space(6.0);
                            }
                        }
                    });
            });
        if clear {
            self.notifications.clear();
        }
        if close {
            self.notifications.open = false;
        }
    }

    fn render_confirm_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete.clone() else {
            return;
        };
        let t = self.tokens();
        let name = self
            .find_record(&id)
            .map(|record| record.file_name.clone())
            .unwrap_or_default();
        let mut close = false;
        let mut delete = false;
        Window::new("Remove download")
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(RichText::new("Remove this entry from Pulse?").strong());
                ui.label(t.muted_text(truncate(&name, 64)));
                ui.add_space(10.0);
                t.inset_frame().show(ui, |ui| {
                    let note = "Already downloaded files stay on disk; partial data is cleaned up.";
                    ui.label(t.faint_text(note));
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.add(t.subtle_button("Keep it")).clicked() {
                        close = true;
                    }
                    let label = RichText::new("Remove").strong().color(Color32::WHITE);
                    let remove = egui::Button::new(label)
                        .fill(t.danger)
                        .stroke(Stroke::new(1.0, t.danger))
                        .rounding(Rounding::same(theme::RADIUS_SM));
                    if ui.add(remove).clicked() {
                        delete = true;
                    }
                });
            });
        if close {
            self.confirm_delete = None;
        }
        if delete {
            self.confirm_delete = None;
            self.delete_record(&id);
        }
    }

    fn queue_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| crate::queue::is_startable(record))
            .count()
    }

    fn completed_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.status == DownloadStatus::Completed)
            .count()
    }

    fn failed_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.status == DownloadStatus::Failed)
            .count()
    }
}

impl eframe::App for DownloadManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_engine_events();
        self.apps.poll();
        self.poll_tray(ctx);
        self.handle_drop_files(ctx);
        self.handle_shortcuts(ctx);
        self.notifications.remove_expired();

        if ctx.input(|input| input.viewport().close_requested())
            && self.settings.general.minimize_to_tray
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let mode = self.layout_mode(ctx);
        if mode != LayoutMode::Narrow {
            self.render_sidebar(ctx, mode);
        }
        self.render_topbar(ctx, mode);
        match self.page {
            Page::Dashboard | Page::Completed | Page::Failed => {
                if self.page == Page::Completed {
                    self.filter = StatusFilter::Completed;
                }
                if self.page == Page::Failed {
                    self.filter = StatusFilter::Failed;
                }
                self.render_dashboard(ctx);
            }
            Page::Queue => self.render_queue(ctx),
            Page::Apps => self.render_apps(ctx),
            Page::Settings => self.render_settings(ctx),
        }
        self.render_notifications(ctx);
        self.render_add_dialog(ctx);
        self.render_confirm_delete(ctx);
        ctx.request_repaint_after(Duration::from_millis(120));
    }
}

impl DownloadManagerApp {
    fn render_add_dialog(&mut self, ctx: &egui::Context) {
        if self.add_dialog.is_none() {
            return;
        }
        let t = self.tokens();
        let mut submit = false;
        let mut close = false;
        let mut paste = false;
        let width = (ctx.screen_rect().width() - 140.0).clamp(360.0, 560.0);
        Window::new("Add download")
            .collapsible(false)
            .resizable(false)
            .default_width(width)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(t.muted_text(
                    "Paste a direct HTTP/HTTPS link, or drop a .url shortcut onto the window.",
                ));
                ui.add_space(12.0);
                if let Some(dialog) = self.add_dialog.as_mut() {
                    settings_label(ui, &t, "Download URL");
                    ui.horizontal(|ui| {
                        let field_width = (ui.available_width() - 88.0).max(160.0);
                        let response = ui.add_sized(
                            [field_width, 32.0],
                            egui::TextEdit::singleline(&mut dialog.url)
                                .hint_text("https://example.com/file.zip"),
                        );
                        if response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            submit = true;
                        }
                        if ui.add(t.subtle_button("Paste")).clicked() {
                            paste = true;
                        }
                    });
                    ui.add_space(10.0);
                    settings_label(ui, &t, "File name (optional)");
                    ui.add_sized(
                        [ui.available_width(), 30.0],
                        egui::TextEdit::singleline(&mut dialog.file_name)
                            .hint_text("Leave empty to use the name from the URL"),
                    );
                    ui.add_space(10.0);
                    settings_label(ui, &t, "Save to folder");
                    ui.horizontal(|ui| {
                        let field_width = (ui.available_width() - 104.0).max(160.0);
                        ui.add_sized(
                            [field_width, 30.0],
                            egui::TextEdit::singleline(&mut dialog.folder),
                        );
                        if ui.add(t.subtle_button("Choose…")).clicked() {
                            if let Some(folder) = rfd::FileDialog::new()
                                .set_directory(&dialog.folder)
                                .pick_folder()
                            {
                                dialog.folder = folder.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.add_space(10.0);
                    settings_label(ui, &t, "SHA-256 checksum (optional)");
                    ui.add_sized(
                        [ui.available_width(), 30.0],
                        egui::TextEdit::singleline(&mut dialog.checksum)
                            .hint_text("64 hexadecimal characters"),
                    );
                    if let Some(error) = &dialog.error {
                        ui.add_space(12.0);
                        Frame::none()
                            .fill(theme::tint(t.danger, if t.dark { 26 } else { 18 }))
                            .stroke(Stroke::new(1.0, theme::tint(t.danger, 90)))
                            .rounding(Rounding::same(theme::RADIUS_MD))
                            .inner_margin(Margin::symmetric(12.0, 9.0))
                            .show(ui, |ui| {
                                fill_width(ui);
                                ui.label(RichText::new(error.as_str()).color(t.danger).size(12.5));
                            });
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.add(t.primary_button("Add download")).clicked() {
                            submit = true;
                        }
                        if ui.add(t.subtle_button("Cancel")).clicked() {
                            close = true;
                        }
                        ui.label(t.faint_text("Press Enter in the URL field to add instantly"));
                    });
                }
            });
        if paste {
            self.paste_url();
        }
        if close {
            self.add_dialog = None;
        }
        if submit {
            self.submit_add_dialog();
        }
    }
}

/// Fill the width that the parent container handed to this widget.
fn fill_width(ui: &mut Ui) {
    ui.set_min_width(ui.available_width());
}

/// Circular brand mark used in the sidebar and the icon rail.
fn brand_mark(ui: &mut Ui, accent: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, accent);
    ui.painter().circle_stroke(
        rect.center(),
        size / 2.0 - 1.0,
        Stroke::new(1.0, theme::tint(Color32::WHITE, 70)),
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "P",
        FontId::proportional(size * 0.5),
        Color32::WHITE,
    );
}

/// Small colored dot used by status rows and notification cards.
fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Thin separator that keeps inline meta values readable.
fn meta_separator(ui: &mut Ui, t: &theme::Tokens) {
    ui.label(t.faint_text("·"));
}

/// One statistic card in the dashboard overview.
fn stat_card(
    ui: &mut Ui,
    t: &theme::Tokens,
    label: &str,
    value: &str,
    caption: &str,
    color: Color32,
) {
    t.card().show(ui, |ui| {
        fill_width(ui);
        ui.horizontal(|ui| {
            status_dot(ui, color);
            ui.label(t.muted_text(label));
        });
        ui.add_space(6.0);
        ui.label(theme::metric(value));
        ui.label(t.faint_text(caption));
    });
}

/// Rounded badge showing the file extension of a download.
fn file_badge(ui: &mut Ui, record: &DownloadRecord) {
    let extension = record
        .file_name
        .rsplit('.')
        .next()
        .unwrap_or("FILE")
        .to_ascii_uppercase();
    let extension = extension.chars().take(4).collect::<String>();
    let color = theme::file_color(&extension);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(46.0, 50.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(theme::RADIUS_MD), theme::tint(color, 46));
    ui.painter().rect_stroke(
        rect,
        Rounding::same(theme::RADIUS_MD),
        Stroke::new(1.0, theme::tint(color, 150)),
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        extension,
        FontId::proportional(11.5),
        color,
    );
}

/// Grouped settings card with a title, a hint and arbitrary content.
fn settings_section<F>(ui: &mut Ui, t: &theme::Tokens, title: &str, subtitle: &str, content: F)
where
    F: FnOnce(&mut Ui),
{
    t.card().show(ui, |ui| {
        fill_width(ui);
        ui.label(theme::card_title(title));
        ui.label(t.muted_text(subtitle));
        ui.add_space(12.0);
        content(ui);
    });
    ui.add_space(theme::gap());
}

/// Muted label above a settings field.
fn settings_label(ui: &mut Ui, t: &theme::Tokens, text: &str) {
    ui.label(t.muted_text(text));
}

/// Label/value row used in the app details dialog.
fn detail_row(ui: &mut Ui, t: &theme::Tokens, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(t.faint_text(label));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(t.muted_text(value));
        });
    });
}

/// Inline error banner used when a remote catalog or an action fails.
fn danger_banner(ui: &mut Ui, t: &theme::Tokens, title: &str, message: &str, hint: &str) {
    Frame::none()
        .fill(theme::tint(t.danger, if t.dark { 26 } else { 18 }))
        .stroke(Stroke::new(1.0, theme::tint(t.danger, 90)))
        .rounding(Rounding::same(theme::RADIUS_LG))
        .inner_margin(Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            fill_width(ui);
            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠").color(t.danger));
                ui.label(RichText::new(title).strong().color(t.danger));
            });
            ui.add_space(4.0);
            ui.label(t.muted_text(message));
            ui.label(t.faint_text(hint));
        });
    ui.add_space(theme::gap());
}

/// Friendly placeholder for empty lists and loading states.
fn empty_state(ui: &mut Ui, t: &theme::Tokens, title: &str, subtitle: &str) {
    t.inset_frame().show(ui, |ui| {
        fill_width(ui);
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(58.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 28.0, theme::tint(t.accent, 40));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "↓",
                FontId::proportional(26.0),
                t.accent,
            );
            ui.add_space(12.0);
            ui.label(RichText::new(title).size(16.0).strong());
            ui.label(t.muted_text(subtitle));
        });
        ui.add_space(24.0);
    });
}

/// Catalog entry card used by the Apps screen.
fn render_app_card(
    ui: &mut Ui,
    t: &theme::Tokens,
    item: &AppItem,
    accent: Color32,
) -> (bool, bool) {
    let mut download = false;
    let mut details = false;
    t.card().show(ui, |ui| {
        fill_width(ui);
        ui.set_min_height(128.0);
        ui.horizontal(|ui| {
            app_icon(ui, &item.name, accent, 46.0);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(item.name.as_str()).size(14.5).strong());
                ui.label(t.faint_text(format!("{} · {}", item.category, item.version)));
            });
        });
        ui.add_space(10.0);
        ui.label(t.muted_text(item.short_description.as_str()));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(t.faint_text(item.size_label()));
            meta_separator(ui, t);
            ui.label(t.faint_text(item.developer.as_str()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(t.primary_button("Download")).clicked() {
                    download = true;
                }
                if ui.add(t.subtle_button("Details")).clicked() {
                    details = true;
                }
            });
        });
    });
    (download, details)
}

/// Square monogram icon for an app without a bitmap asset.
fn app_icon(ui: &mut Ui, name: &str, accent: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        Rounding::same(theme::RADIUS_MD),
        theme::tint(accent, 46),
    );
    ui.painter().rect_stroke(
        rect,
        Rounding::same(theme::RADIUS_MD),
        Stroke::new(1.0, theme::tint(accent, 140)),
    );
    let initial = name
        .chars()
        .next()
        .unwrap_or('A')
        .to_ascii_uppercase()
        .to_string();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(size * 0.42),
        accent,
    );
}

/// Human readable label for the active sort order.
fn sort_label(sort: SortKey) -> &'static str {
    match sort {
        SortKey::Recent => "Newest first",
        SortKey::Name => "Name",
        SortKey::Size => "Size",
        SortKey::Status => "Status",
    }
}

/// Number of equal-width columns that comfortably fit into the available width.
fn responsive_columns(width: f32, max_width: f32, max_columns: usize) -> usize {
    let target = width.min(max_width);
    let mut columns = max_columns.max(1);
    while columns > 1 && target / (columns as f32) < 320.0 {
        columns -= 1;
    }
    columns
}

/// Short "time ago" label for notification entries.
fn relative_time(created: Instant) -> String {
    let seconds = Instant::now().saturating_duration_since(created).as_secs();
    match seconds {
        0..=4 => "just now".to_owned(),
        5..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

/// Shorten long names so that single-line rows stay readable.
fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut result = value.chars().take(max_chars.saturating_sub(1)).collect::<String>();
    result.push('…');
    result
}

/// One entry inside the notification popover.
fn notification_card(ui: &mut Ui, t: &theme::Tokens, notification: &Notification, color: Color32) {
    let title = RichText::new(notification.title.as_str())
        .strong()
        .size(12.5);
    let message = notification.message.as_str();
    let age = relative_time(notification.created_at);
    Frame::none()
        .fill(theme::tint(color, if t.dark { 22 } else { 16 }))
        .stroke(Stroke::new(1.0, theme::tint(color, 70)))
        .rounding(Rounding::same(theme::RADIUS_MD))
        .inner_margin(Margin::symmetric(12.0, 9.0))
        .show(ui, |ui| {
            fill_width(ui);
            ui.horizontal(|ui| {
                status_dot(ui, color);
                ui.vertical(|ui| {
                    ui.label(title);
                    ui.label(t.muted_text(message));
                    ui.label(t.faint_text(age));
                });
            });
        });
}
