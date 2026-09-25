use anyhow::Result;
use chrono::Utc;
use eframe::egui::{self, Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Stroke, Ui, Vec2, Window};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::apps::AppsState;
use crate::database::{Database, SharedDatabase};
use crate::downloader::{DownloadEngine, DownloadEvent, EngineConfig};
use crate::models::{AppItem, DownloadRecord, DownloadStatus};
use crate::notifications::{NotificationCenter, NotificationKind};
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

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let queue_count = self.queue_count();
        let completed_count = self.completed_count();
        let failed_count = self.failed_count();
        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(238.0)
            .frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::SIDEBAR } else { Color32::from_rgb(235, 240, 248) }).inner_margin(Margin::same(16.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 17.0, self.accent);
                    ui.painter().text(rect.center(), Align2::CENTER_CENTER, "P", FontId::proportional(19.0), Color32::WHITE);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("PULSE").strong().size(16.0));
                        ui.label(RichText::new("Download manager").small().color(theme::MUTED));
                    });
                });
                ui.add_space(26.0);
                ui.label(RichText::new("WORKSPACE").small().color(theme::MUTED).strong());
                ui.add_space(6.0);
                self.nav_item(ui, Page::Dashboard, "⌂", "Downloads", None);
                self.nav_item(ui, Page::Queue, "≡", "Queue", Some(queue_count));
                self.nav_item(ui, Page::Apps, "▣", "Apps", None);
                ui.add_space(20.0);
                ui.label(RichText::new("LIBRARY").small().color(theme::MUTED).strong());
                ui.add_space(6.0);
                self.nav_item(ui, Page::Completed, "✓", "Completed", Some(completed_count));
                self.nav_item(ui, Page::Failed, "!", "Failed", Some(failed_count));
                ui.add_space(20.0);
                self.nav_item(ui, Page::Settings, "⚙", "Settings", None);
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    ui.separator();
                    ui.label(RichText::new("Native Rust desktop app").small().color(theme::MUTED));
                    ui.label(RichText::new("v0.1.0").small().color(theme::MUTED));
                });
            });
    }

    fn nav_item(&mut self, ui: &mut Ui, page: Page, icon: &str, label: &str, count: Option<usize>) {
        let selected = self.page == page;
        let response = ui.add_sized(
            [ui.available_width(), 38.0],
            egui::SelectableLabel::new(
                selected,
                RichText::new(format!("  {icon}    {label}"))
                    .size(14.0)
                    .color(if selected { Color32::WHITE } else { theme::MUTED }),
            ),
        );
        if response.clicked() {
            self.page = page;
            if page == Page::Apps && self.apps.items.is_empty() && !self.apps.loading {
                self.apps.refresh(&self.settings.apps_api_url);
            }
        }
        if let Some(count) = count {
            let rect = response.rect;
            let text = count.to_string();
            ui.painter().text(
                egui::pos2(rect.right() - 16.0, rect.center().y),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(11.0),
                if selected { Color32::WHITE } else { theme::MUTED },
            );
        }
    }

    fn render_topbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::BG } else { Color32::from_rgb(244, 247, 252) }).inner_margin(Margin::same(16.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.page.title()).size(21.0).strong());
                    ui.add_space(12.0);
                    let search_width = (ui.available_width() - 260.0).max(160.0);
                    ui.add_sized(
                        [search_width, 36.0],
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search downloads…")
                            .margin(Vec2::new(12.0, 8.0)),
                    );
                    if ui.button("🔔").on_hover_text("Notifications").clicked() {
                        self.notifications.open = !self.notifications.open;
                    }
                    let button = egui::Button::new(RichText::new("＋  Add download").strong()).fill(self.accent).min_size(Vec2::new(142.0, 36.0));
                    if ui.add(button).clicked() {
                        self.open_add_dialog(None);
                    }
                });
            });
    }

    fn render_dashboard(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::BG } else { Color32::from_rgb(244, 247, 252) })).show(ctx, |ui| {
            ScrollArea::vertical().id_salt("downloads-scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                self.render_stats(ui);
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    for (filter, label) in [(StatusFilter::All, "All"), (StatusFilter::Active, "Active"), (StatusFilter::Completed, "Completed"), (StatusFilter::Failed, "Failed")] {
                        let selected = self.filter == filter;
                        if ui.add(egui::SelectableLabel::new(selected, RichText::new(label).size(13.0))).clicked() {
                            self.filter = filter;
                            self.page = match filter { StatusFilter::Completed => Page::Completed, StatusFilter::Failed => Page::Failed, _ => Page::Dashboard };
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("sort-downloads").selected_text(match self.sort { SortKey::Recent => "Newest first", SortKey::Name => "Name", SortKey::Size => "Size", SortKey::Status => "Status" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.sort, SortKey::Recent, "Newest first");
                            ui.selectable_value(&mut self.sort, SortKey::Name, "Name");
                            ui.selectable_value(&mut self.sort, SortKey::Size, "Size");
                            ui.selectable_value(&mut self.sort, SortKey::Status, "Status");
                        });
                        ui.label(RichText::new("Sort").small().color(theme::MUTED));
                    });
                });
                ui.add_space(12.0);
                let records = self.visible_records();
                if records.is_empty() {
                    self.render_empty(ui, "No downloads here", "Add a URL or drop a .url shortcut to get started.");
                } else {
                    let mut action = None;
                    for record in &records {
                        if let Some(next) = self.render_download_card(ui, record) {
                            action = Some((record.id.clone(), next));
                        }
                        ui.add_space(10.0);
                    }
                    if let Some((id, action)) = action {
                        self.handle_record_action(&id, action);
                    }
                }
                ui.add_space(24.0);
            });
        });
    }

    fn render_stats(&self, ui: &mut Ui) {
        let active = self.records.iter().filter(|record| matches!(record.status, DownloadStatus::Downloading | DownloadStatus::Queued)).count();
        let completed = self.records.iter().filter(|record| record.status == DownloadStatus::Completed).count();
        let total = self.records.len();
        let speed = self.records.iter().filter(|record| record.status == DownloadStatus::Downloading).map(|record| record.speed_bps).sum::<f64>();
        let stats = [
            ("All downloads", total.to_string(), "Tracked locally", theme::BLUE),
            ("Active now", active.to_string(), "Across the queue", self.accent),
            ("Completed", completed.to_string(), "Ready to open", theme::SUCCESS),
            ("Current speed", utils::format_speed(speed), "Combined throughput", theme::WARNING),
        ];
        ui.columns(4, |columns| {
            for (column, (label, value, caption, color)) in columns.iter_mut().zip(stats) {
                Frame::none().fill(theme::PANEL).stroke(Stroke::new(1.0_f32, theme::BORDER)).rounding(Rounding::same(13.0)).inner_margin(Margin::same(14.0)).show(column, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 4.0, color);
                        ui.label(RichText::new(label).small().color(theme::MUTED));
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new(value).size(22.0).strong());
                    ui.label(RichText::new(caption).small().color(theme::MUTED));
                });
            }
        });
    }

    fn render_download_card(&self, ui: &mut Ui, record: &DownloadRecord) -> Option<RecordAction> {
        let mut action = None;
        theme::card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                self.file_badge(ui, record);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(record.file_name.as_str()).strong().size(15.0));
                    ui.label(RichText::new(truncate(&record.url, 86)).small().color(theme::MUTED));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let color = theme::status_color(record.status);
                    let pill = egui::Button::new(RichText::new(format!("●  {}", record.status.label())).small().color(color)).fill(color.linear_multiply(0.15)).stroke(Stroke::new(0.0_f32, Color32::TRANSPARENT));
                    ui.add(pill);
                });
            });
            ui.add_space(14.0);
            let progress = record.progress();
            let progress_text = match record.total_bytes {
                Some(total) => format!("{} / {}   {:.0}%", utils::format_bytes(record.downloaded_bytes), utils::format_bytes(total), progress * 100.0),
                None => format!("{} downloaded", utils::format_bytes(record.downloaded_bytes)),
            };
            ui.add(egui::ProgressBar::new(progress).desired_height(8.0).fill(self.accent).text(progress_text));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Speed  {}", utils::format_speed(record.speed_bps))).small().color(theme::MUTED));
                ui.separator();
                ui.label(RichText::new(format!("ETA  {}", utils::format_eta(record.eta_seconds))).small().color(theme::MUTED));
                ui.separator();
                ui.label(RichText::new(format!("Priority  {}", record.priority.max(0))).small().color(theme::MUTED));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("⋯").on_hover_text("More actions").clicked() {
                        action = Some(RecordAction::Copy);
                    }
                    match record.status {
                        DownloadStatus::Downloading => {
                            if ui.small_button("Pause").clicked() { action = Some(RecordAction::Pause); }
                            if ui.small_button("Cancel").clicked() { action = Some(RecordAction::Cancel); }
                        }
                        DownloadStatus::Paused => {
                            if ui.small_button("Resume").clicked() { action = Some(RecordAction::Resume); }
                            if ui.small_button("Cancel").clicked() { action = Some(RecordAction::Cancel); }
                        }
                        DownloadStatus::Queued => {
                            if ui.small_button("Start").clicked() { action = Some(RecordAction::Start); }
                            if ui.small_button("Cancel").clicked() { action = Some(RecordAction::Cancel); }
                        }
                        DownloadStatus::Failed => {
                            if ui.small_button("Retry").clicked() { action = Some(RecordAction::Retry); }
                        }
                        DownloadStatus::Completed => {
                            if ui.small_button("Open file").clicked() { action = Some(RecordAction::Open); }
                            if ui.small_button("Folder").clicked() { action = Some(RecordAction::Folder); }
                        }
                        DownloadStatus::Cancelled => {
                            if ui.small_button("Retry").clicked() { action = Some(RecordAction::Retry); }
                        }
                    }
                    if ui.small_button("Copy URL").clicked() { action = Some(RecordAction::Copy); }
                    if ui.small_button("Delete").clicked() { action = Some(RecordAction::Delete); }
                });
            });
            if let Some(error) = &record.error {
                ui.add_space(7.0);
                ui.label(RichText::new(format!("Error: {}", truncate(error, 150))).small().color(theme::DANGER));
            }
        });
        action
    }

    fn file_badge(&self, ui: &mut Ui, record: &DownloadRecord) {
        let extension = record.file_name.rsplit('.').next().unwrap_or("FILE").to_ascii_uppercase();
        let extension = extension.chars().take(4).collect::<String>();
        let color = file_color(&extension);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(48.0, 54.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, Rounding::same(11.0), color.linear_multiply(0.2));
        ui.painter().rect_stroke(rect, Rounding::same(11.0), Stroke::new(1.0_f32, color.linear_multiply(0.6)));
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, extension, FontId::proportional(12.0), color);
    }

    fn render_queue(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::BG } else { Color32::from_rgb(244, 247, 252) })).show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Control your flow").size(22.0).strong());
                        ui.label(RichText::new("Prioritize downloads and decide when the queue runs.").color(theme::MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new("Stop queue").fill(theme::PANEL_ALT)).clicked() { self.queue_running = false; self.engine.stop_queue(); }
                        if ui.add(egui::Button::new("Pause queue").fill(theme::PANEL_ALT)).clicked() { self.queue_running = false; self.engine.pause_all(); }
                        if ui.add(egui::Button::new("Start queue").fill(self.accent)).clicked() { self.start_queue(); }
                    });
                });
                ui.add_space(18.0);
                Frame::none().fill(theme::PANEL).stroke(Stroke::new(1.0_f32, theme::BORDER)).rounding(Rounding::same(13.0)).inner_margin(Margin::same(15.0)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if self.queue_running { "Queue is running" } else { "Queue is paused" }).strong().color(if self.queue_running { theme::SUCCESS } else { theme::WARNING }));
                        ui.separator();
                        ui.label(RichText::new(format!("{} waiting", self.queue_count())).color(theme::MUTED));
                        ui.separator();
                        ui.label(RichText::new(format!("{} simultaneous downloads", self.settings.downloads.maximum_simultaneous_downloads)).color(theme::MUTED));
                    });
                });
                ui.add_space(12.0);
                let mut queued = self.records.iter().filter(|record| crate::queue::is_waiting(record)).cloned().collect::<Vec<_>>();
                queued.sort_by(|left, right| right.priority.cmp(&left.priority).then_with(|| left.created_at.cmp(&right.created_at)));
                if queued.is_empty() {
                    self.render_empty(ui, "Queue is clear", "New downloads will appear here before they start.");
                } else {
                    let mut move_action = None;
                    let mut record_action = None;
                    for (index, record) in queued.iter().enumerate() {
                        theme::card_frame().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{:02}", index + 1)).size(16.0).color(self.accent).strong());
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(record.file_name.as_str()).strong());
                                    ui.label(RichText::new(format!("{} · {}", record.status.label(), utils::format_bytes(record.total_bytes.unwrap_or(0)))).small().color(theme::MUTED));
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("↓").clicked() { move_action = Some((record.id.clone(), 1)); }
                                    if ui.small_button("↑").clicked() { move_action = Some((record.id.clone(), -1)); }
                                    if record.status == DownloadStatus::Failed && ui.small_button("Retry").clicked() { record_action = Some((record.id.clone(), RecordAction::Retry)); }
                                    if ui.small_button("Delete").clicked() { record_action = Some((record.id.clone(), RecordAction::Delete)); }
                                });
                            });
                        });
                        ui.add_space(8.0);
                    }
                    if let Some((id, direction)) = move_action { self.set_priority(&id, direction); }
                    if let Some((id, action)) = record_action { self.handle_record_action(&id, action); }
                }
            });
        });
    }

    fn render_apps(&mut self, ctx: &egui::Context) {
        if self.apps.items.is_empty() && !self.apps.loading && self.apps.error.is_none() {
            self.apps.refresh(&self.settings.apps_api_url);
        }
        egui::CentralPanel::default().frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::BG } else { Color32::from_rgb(244, 247, 252) })).show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Curated apps").size(22.0).strong());
                        ui.label(RichText::new("Browse trusted software delivered by your catalog API.").color(theme::MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Refresh catalog").clicked() { self.apps.refresh(&self.settings.apps_api_url); }
                        if self.apps.loading { ui.spinner(); }
                    });
                });
                ui.add_space(18.0);
                if let Some(error) = &self.apps.error {
                    Frame::none().fill(theme::DANGER.linear_multiply(0.12)).stroke(Stroke::new(1.0_f32, theme::DANGER.linear_multiply(0.5))).rounding(Rounding::same(12.0)).inner_margin(Margin::same(14.0)).show(ui, |ui| {
                        ui.label(RichText::new("Catalog unavailable").strong().color(theme::DANGER));
                        ui.label(error);
                        ui.label(RichText::new("Check the API endpoint in Settings → General.").small().color(theme::MUTED));
                    });
                } else if self.apps.loading && self.apps.items.is_empty() {
                    self.render_empty(ui, "Loading catalog", "Fetching the latest app metadata…");
                } else if self.apps.items.is_empty() {
                    self.render_empty(ui, "No apps published", "The catalog API returned an empty list.");
                } else {
                    let query = self.search.trim().to_lowercase();
                    let items = self.apps.items.iter().filter(|item| query.is_empty() || item.name.to_lowercase().contains(&query) || item.category.to_lowercase().contains(&query) || item.developer.to_lowercase().contains(&query)).cloned().collect::<Vec<_>>();
                    let columns = if ui.available_width() > 880.0 { 2 } else { 1 };
                    let mut download = None;
                    let mut details = None;
                    ui.columns(columns, |column_uis| {
                        for (index, item) in items.iter().enumerate() {
                            let column = index % columns;
                            let (download_clicked, details_clicked) = render_app_card(&mut column_uis[column], item, self.accent);
                            if download_clicked { download = Some(item.clone()); }
                            if details_clicked { details = self.apps.items.iter().position(|value| value.id == item.id); }
                            column_uis[column].add_space(10.0);
                        }
                    });
                    if let Some(item) = download { self.download_app(item); }
                    if let Some(index) = details { self.apps.selected = Some(index); }
                }
            });
        });
        self.render_app_details(ctx);
    }

    fn render_app_details(&mut self, ctx: &egui::Context) {
        let Some(index) = self.apps.selected else { return; };
        if index >= self.apps.items.len() {
            self.apps.selected = None;
            return;
        }
        let item = self.apps.items[index].clone();
        let mut close = false;
        let mut download = false;
        Window::new("App details").collapsible(false).resizable(false).default_width(460.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                app_icon(ui, &item.name, self.accent, 54.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(item.name.as_str()).size(20.0).strong());
                    ui.label(RichText::new(format!("{} · {}", item.category, item.version)).color(theme::MUTED));
                });
            });
            ui.add_space(12.0);
            ui.label(item.short_description.as_str());
            ui.add_space(10.0);
            for (label, value) in [("Developer", item.developer.clone()), ("Size", item.size_label()), ("Last updated", item.updated_at.clone())] {
                ui.horizontal(|ui| { ui.label(RichText::new(label).color(theme::MUTED)); ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(value); }); });
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Download").fill(self.accent)).clicked() { download = true; }
                if ui.button("Close").clicked() { close = true; }
            });
        });
        if close { self.apps.selected = None; }
        if download { self.download_app(item); self.apps.selected = None; }
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(Frame::none().fill(if self.settings.appearance.dark_mode { theme::BG } else { Color32::from_rgb(244, 247, 252) })).show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Preferences").size(22.0).strong());
                ui.label(RichText::new("Tune the engine and interface for your workflow.").color(theme::MUTED));
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    for (tab, label) in [(SettingsTab::General, "General"), (SettingsTab::Downloads, "Downloads"), (SettingsTab::Network, "Network"), (SettingsTab::Appearance, "Appearance")] {
                        if ui.add(egui::SelectableLabel::new(self.settings_tab == tab, label)).clicked() { self.settings_tab = tab; }
                    }
                });
                ui.add_space(14.0);
                let mut changed = false;
                match self.settings_tab {
                    SettingsTab::General => changed |= self.settings_general(ui),
                    SettingsTab::Downloads => changed |= self.settings_downloads(ui),
                    SettingsTab::Network => changed |= self.settings_network(ui),
                    SettingsTab::Appearance => changed |= self.settings_appearance(ui, ctx),
                }
                if changed { self.persist_settings(); }
            });
        });
    }

    fn settings_general(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        Self::settings_section(ui, "General", "App behavior and catalog source.", |ui| {
            let mut start = self.settings.general.start_with_windows;
            if ui.checkbox(&mut start, "Start with Windows").changed() {
                match system::set_start_with_windows(start) {
                    Ok(()) => { self.settings.general.start_with_windows = start; changed = true; }
                    Err(error) => self.notifications.push("Startup setting unavailable", error.to_string(), NotificationKind::Warning),
                }
            }
            changed |= ui.checkbox(&mut self.settings.general.minimize_to_tray, "Minimize to tray").changed();
            changed |= ui.checkbox(&mut self.settings.general.confirm_before_deleting, "Confirm before deleting").changed();
            ui.add_space(10.0);
            ui.label(RichText::new("Language").small().color(theme::MUTED));
            let language_response = egui::ComboBox::from_id_salt("language")
                .selected_text(&self.settings.general.language)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.general.language, "English".to_owned(), "English")
                });
            changed |= language_response.inner.map(|response| response.changed()).unwrap_or(false);
            ui.add_space(12.0);
            ui.label(RichText::new("Apps API endpoint").small().color(theme::MUTED));
            changed |= ui.text_edit_singleline(&mut self.settings.apps_api_url).changed();
            ui.label(RichText::new("GET endpoint returning an array or { apps: [...] }.").small().color(theme::MUTED));
        });
        changed
    }

    fn settings_downloads(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        Self::settings_section(ui, "Downloads", "Storage, concurrency and automatic starting.", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Default folder").color(theme::MUTED));
                if ui.text_edit_singleline(&mut self.settings.downloads.default_download_folder).changed() { changed = true; }
                if ui.button("Choose…").clicked() {
                    let current = self.settings.default_folder_path();
                    if let Some(folder) = rfd::FileDialog::new().set_directory(current).pick_folder() {
                        self.settings.set_download_folder(folder);
                        changed = true;
                    }
                }
            });
            changed |= ui.checkbox(&mut self.settings.downloads.auto_start_downloads, "Start downloads automatically").changed();
            ui.add_space(8.0);
            changed |= ui.add(egui::Slider::new(&mut self.settings.downloads.maximum_simultaneous_downloads, 1..=16).text("Maximum simultaneous downloads")).changed();
            changed |= ui.add(egui::Slider::new(&mut self.settings.downloads.maximum_connections_per_download, 1..=16).text("Connections per download")).changed();
            changed |= ui.add(egui::DragValue::new(&mut self.settings.downloads.download_speed_limit_kbps).speed(64.0).suffix(" KB/s (0 = unlimited)")).changed();
        });
        changed
    }

    fn settings_network(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        Self::settings_section(ui, "Network", "Connection policy, proxy and request headers.", |ui| {
            changed |= ui.add(egui::Slider::new(&mut self.settings.network.connection_timeout_seconds, 5..=600).text("Connection timeout (seconds)")).changed();
            changed |= ui.add(egui::Slider::new(&mut self.settings.network.retry_count, 0..=20).text("Automatic retries")).changed();
            ui.add_space(8.0);
            ui.label(RichText::new("Proxy").small().color(theme::MUTED));
            changed |= ui.text_edit_singleline(&mut self.settings.network.proxy).on_hover_text("Example: http://user:password@proxy.example:8080").changed();
            ui.label(RichText::new("Leave empty to use the system/network defaults.").small().color(theme::MUTED));
            ui.add_space(8.0);
            ui.label(RichText::new("User-Agent").small().color(theme::MUTED));
            changed |= ui.text_edit_singleline(&mut self.settings.network.user_agent).changed();
            ui.add_space(8.0);
            ui.label(RichText::new("Additional headers").small().color(theme::MUTED));
            changed |= ui.add(egui::TextEdit::multiline(&mut self.settings.network.additional_headers).desired_rows(3).hint_text("One header per line: X-Token: value")).changed();
        });
        changed
    }

    fn settings_appearance(&mut self, ui: &mut Ui, ctx: &egui::Context) -> bool {
        let mut changed = false;
        Self::settings_section(ui, "Appearance", "Colors, scale and display density.", |ui| {
            if ui.checkbox(&mut self.settings.appearance.dark_mode, "Dark mode").changed() {
                self.apply_appearance(ctx);
                changed = true;
            }
            ui.horizontal(|ui| {
                ui.label("Accent color");
                let mut color = self.accent;
                if ui.color_edit_button_srgba(&mut color).changed() {
                    self.accent = color;
                    self.settings.appearance.accent_color = format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
                    self.apply_appearance(ctx);
                    changed = true;
                }
                if ui.text_edit_singleline(&mut self.settings.appearance.accent_color).changed() {
                    self.apply_appearance(ctx);
                    changed = true;
                }
            });
            changed |= ui.add(egui::Slider::new(&mut self.settings.appearance.ui_scale, 0.8..=1.5).text("UI scale")).changed();
            if changed { self.apply_appearance(ctx); }
        });
        changed
    }

    fn settings_section<F>(ui: &mut Ui, title: &str, subtitle: &str, content: F)
    where
        F: FnOnce(&mut Ui),
    {
        Frame::none().fill(theme::PANEL).stroke(Stroke::new(1.0_f32, theme::BORDER)).rounding(Rounding::same(14.0)).inner_margin(Margin::same(18.0)).show(ui, |ui| {
            ui.label(RichText::new(title).size(16.0).strong());
            ui.label(RichText::new(subtitle).small().color(theme::MUTED));
            ui.add_space(14.0);
            content(ui);
        });
    }

    fn render_empty(&self, ui: &mut Ui, title: &str, subtitle: &str) {
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(70.0, 70.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 34.0, self.accent.linear_multiply(0.16));
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "↓", FontId::proportional(30.0), self.accent);
            ui.add_space(12.0);
            ui.label(RichText::new(title).size(17.0).strong());
            ui.label(RichText::new(subtitle).color(theme::MUTED));
        });
        ui.add_space(36.0);
    }

    fn render_notifications(&mut self, ctx: &egui::Context) {
        if !self.notifications.open {
            return;
        }
        let notification_items = self.notifications.items.iter().take(7).cloned().collect::<Vec<_>>();
        egui::Area::new("notification-area".into()).anchor(Align2::RIGHT_TOP, egui::vec2(-18.0, 68.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            Frame::none().fill(theme::PANEL).stroke(Stroke::new(1.0_f32, theme::BORDER)).rounding(Rounding::same(12.0)).inner_margin(Margin::same(14.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Notifications").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| { if ui.small_button("Clear").clicked() { self.notifications.clear(); } });
                });
                ui.separator();
                if self.notifications.items.is_empty() {
                    ui.label(RichText::new("You're all caught up.").color(theme::MUTED));
                } else {
                    for notification in &notification_items {
                        let color = notification_color(notification.kind);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("●").color(color));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(notification.title.as_str()).strong());
                                ui.label(RichText::new(notification.message.as_str()).small().color(theme::MUTED));
                            });
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
    }

    fn render_confirm_delete(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete.clone() else { return; };
        let name = self.find_record(&id).map(|record| record.file_name.clone()).unwrap_or_default();
        let mut close = false;
        let mut delete = false;
        Window::new("Remove download").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(format!("Remove \"{name}\" from Pulse?"));
            ui.label(RichText::new("The downloaded file will not be deleted.").small().color(theme::MUTED));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Keep").clicked() { close = true; }
                if ui.add(egui::Button::new("Remove").fill(theme::DANGER)).clicked() { delete = true; }
            });
        });
        if close { self.confirm_delete = None; }
        if delete {
            self.confirm_delete = None;
            self.delete_record(&id);
        }
    }

    fn queue_count(&self) -> usize {
        self.records.iter().filter(|record| crate::queue::is_startable(record)).count()
    }

    fn completed_count(&self) -> usize {
        self.records.iter().filter(|record| record.status == DownloadStatus::Completed).count()
    }

    fn failed_count(&self) -> usize {
        self.records.iter().filter(|record| record.status == DownloadStatus::Failed).count()
    }
}

impl eframe::App for DownloadManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_engine_events();
        self.apps.poll();
        self.poll_tray(ctx);
        self.handle_drop_files(ctx);
        self.notifications.remove_expired();

        if ctx.input(|input| input.viewport().close_requested()) && self.settings.general.minimize_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        self.render_sidebar(ctx);
        self.render_topbar(ctx);
        match self.page {
            Page::Dashboard | Page::Completed | Page::Failed => {
                if self.page == Page::Completed { self.filter = StatusFilter::Completed; }
                if self.page == Page::Failed { self.filter = StatusFilter::Failed; }
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
        let mut submit = false;
        let mut close = false;
        let mut paste = false;
        Window::new("Add download").collapsible(false).resizable(false).default_width(540.0).show(ctx, |ui| {
            ui.label(RichText::new("Download URL").small().color(theme::MUTED));
            if let Some(dialog) = self.add_dialog.as_mut() {
                ui.horizontal(|ui| {
                    ui.add_sized([ui.available_width() - 86.0, 36.0], egui::TextEdit::singleline(&mut dialog.url).hint_text("https://example.com/file.zip"));
                    if ui.button("Paste").clicked() { paste = true; }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("File name").color(theme::MUTED));
                    ui.text_edit_singleline(&mut dialog.file_name).on_hover_text("Leave blank to use the name from the URL");
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Save folder").color(theme::MUTED));
                    ui.text_edit_singleline(&mut dialog.folder);
                    if ui.button("Choose…").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().set_directory(&dialog.folder).pick_folder() { dialog.folder = folder.to_string_lossy().into_owned(); }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SHA-256").color(theme::MUTED));
                    ui.add(egui::TextEdit::singleline(&mut dialog.checksum).hint_text("Optional integrity check"));
                });
                if let Some(error) = &dialog.error { ui.add_space(6.0); ui.label(RichText::new(error).color(theme::DANGER)); }
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() { close = true; }
                if ui.add(egui::Button::new("Add download").fill(self.accent)).clicked() { submit = true; }
            });
        });
        if paste { self.paste_url(); }
        if close { self.add_dialog = None; }
        if submit { self.submit_add_dialog(); }
    }
}

fn render_app_card(ui: &mut Ui, item: &AppItem, accent: Color32) -> (bool, bool) {
    let mut download = false;
    let mut details = false;
    theme::card_frame().show(ui, |ui| {
        ui.horizontal(|ui| {
            app_icon(ui, &item.name, accent, 48.0);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(item.name.as_str()).size(15.0).strong());
                ui.label(RichText::new(format!("{}  ·  {}", item.category, item.version)).small().color(theme::MUTED));
            });
        });
        ui.add_space(10.0);
        ui.label(RichText::new(item.short_description.as_str()).color(theme::MUTED));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(item.size_label()).small().color(theme::MUTED));
            ui.separator();
            ui.label(RichText::new(item.developer.as_str()).small().color(theme::MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("View details").clicked() { details = true; }
                if ui.add(egui::Button::new("Download").fill(accent)).clicked() { download = true; }
            });
        });
    });
    (download, details)
}

fn app_icon(ui: &mut Ui, name: &str, accent: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(12.0), accent.linear_multiply(0.18));
    ui.painter().rect_stroke(rect, Rounding::same(12.0), Stroke::new(1.0_f32, accent.linear_multiply(0.55)));
    let initial = name.chars().next().unwrap_or('A').to_ascii_uppercase().to_string();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, initial, FontId::proportional(size * 0.42), accent);
}

fn file_color(extension: &str) -> Color32 {
    match extension {
        "ZIP" | "RAR" | "7Z" | "TAR" => Color32::from_rgb(242, 173, 73),
        "EXE" | "MSI" => Color32::from_rgb(89, 156, 255),
        "MP4" | "MOV" | "MKV" => Color32::from_rgb(212, 111, 255),
        "MP3" | "WAV" | "FLAC" => Color32::from_rgb(78, 211, 177),
        "PDF" | "DOC" | "DOCX" => Color32::from_rgb(241, 99, 116),
        _ => Color32::from_rgb(140, 157, 255),
    }
}

fn notification_color(kind: NotificationKind) -> Color32 {
    match kind {
        NotificationKind::Info => theme::BLUE,
        NotificationKind::Success => theme::SUCCESS,
        NotificationKind::Warning => theme::WARNING,
        NotificationKind::Error => theme::DANGER,
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut result = value.chars().take(max_chars.saturating_sub(1)).collect::<String>();
    result.push('…');
    result
}
