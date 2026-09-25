use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE, RANGE, USER_AGENT};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::database::SharedDatabase;
use crate::models::{DownloadId, DownloadRecord, DownloadStatus};
use crate::settings::Settings;
use crate::utils::{duration_from_secs, header_lines, partial_directory, safe_file_name};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_concurrent_downloads: usize,
    pub max_connections_per_download: u32,
    pub speed_limit_bps: Option<u64>,
    pub timeout: Duration,
    pub retry_count: u32,
    pub proxy: Option<String>,
    pub user_agent: String,
    pub additional_headers: Vec<(String, String)>,
}

impl From<&Settings> for EngineConfig {
    fn from(settings: &Settings) -> Self {
        Self {
            max_concurrent_downloads: settings.downloads.maximum_simultaneous_downloads.clamp(1, 32),
            max_connections_per_download: settings
                .downloads
                .maximum_connections_per_download
                .clamp(1, 16),
            speed_limit_bps: (settings.downloads.download_speed_limit_kbps > 0)
                .then_some(settings.downloads.download_speed_limit_kbps.saturating_mul(1024)),
            timeout: duration_from_secs(settings.network.connection_timeout_seconds),
            retry_count: settings.network.retry_count.min(20),
            proxy: (!settings.network.proxy.trim().is_empty())
                .then(|| settings.network.proxy.trim().to_owned()),
            user_agent: if settings.network.user_agent.trim().is_empty() {
                "PulseDownloadManager/0.1".to_owned()
            } else {
                settings.network.user_agent.trim().to_owned()
            },
            additional_headers: header_lines(&settings.network.additional_headers),
        }
    }
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Started {
        id: DownloadId,
    },
    Metadata {
        id: DownloadId,
        total_bytes: Option<u64>,
        content_type: Option<String>,
    },
    Progress {
        id: DownloadId,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        speed_bps: f64,
        eta_seconds: Option<u64>,
    },
    Paused {
        id: DownloadId,
        downloaded_bytes: u64,
    },
    Completed {
        id: DownloadId,
        path: String,
    },
    Cancelled {
        id: DownloadId,
        downloaded_bytes: u64,
    },
    Failed {
        id: DownloadId,
        downloaded_bytes: u64,
        error: String,
    },
}

pub struct DownloadEngine {
    command_tx: mpsc::Sender<SchedulerMessage>,
    event_rx: mpsc::Receiver<DownloadEvent>,
    thread: Option<JoinHandle<()>>,
}

impl DownloadEngine {
    pub fn new(database: SharedDatabase, config: EngineConfig) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("pulse-download-scheduler".to_owned())
            .spawn(move || scheduler_loop(command_rx, event_tx, database, config))
            .expect("download scheduler thread must start");
        Self {
            command_tx,
            event_rx,
            thread: Some(thread),
        }
    }

    pub fn start(&self, record: DownloadRecord) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::Start(record)));
    }

    pub fn start_queue(&self, records: Vec<DownloadRecord>) {
        let _ = self.command_tx.send(SchedulerMessage::Command(
            EngineCommand::StartQueue(records),
        ));
    }

    pub fn pause(&self, id: &str) {
        let _ = self.command_tx.send(SchedulerMessage::Command(
            EngineCommand::Pause(id.to_owned()),
        ));
    }

    pub fn resume(&self, record: DownloadRecord) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::Resume(record)));
    }

    pub fn cancel(&self, id: &str) {
        let _ = self.command_tx.send(SchedulerMessage::Command(
            EngineCommand::Cancel(id.to_owned()),
        ));
    }

    pub fn pause_all(&self) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::PauseAll));
    }

    pub fn resume_all(&self) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::ResumeAll));
    }

    pub fn stop_queue(&self) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::StopQueue));
    }

    pub fn set_config(&self, config: EngineConfig) {
        let _ = self.command_tx.send(SchedulerMessage::Command(
            EngineCommand::SetConfig(config),
        ));
    }

    pub fn try_event(&self) -> Option<DownloadEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn shutdown(&mut self) {
        let _ = self
            .command_tx
            .send(SchedulerMessage::Command(EngineCommand::Shutdown));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DownloadEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum EngineCommand {
    Start(DownloadRecord),
    StartQueue(Vec<DownloadRecord>),
    Pause(String),
    Resume(DownloadRecord),
    Cancel(String),
    PauseAll,
    ResumeAll,
    StopQueue,
    SetConfig(EngineConfig),
    Shutdown,
}

enum SchedulerMessage {
    Command(EngineCommand),
    WorkerFinished {
        id: DownloadId,
        record: DownloadRecord,
        outcome: WorkerOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerOutcome {
    Completed,
    Paused,
    Cancelled,
    Failed,
}

struct ActiveJob {
    control: Arc<JobControl>,
}

struct JobControl {
    paused: AtomicBool,
    cancelled: AtomicBool,
}

impl JobControl {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        }
    }
}

fn scheduler_loop(
    command_rx: mpsc::Receiver<SchedulerMessage>,
    event_tx: mpsc::Sender<DownloadEvent>,
    database: SharedDatabase,
    mut config: EngineConfig,
) {
    let (worker_tx, worker_rx) = mpsc::channel();
    let mut active: HashMap<DownloadId, ActiveJob> = HashMap::new();
    let mut waiting: VecDeque<DownloadRecord> = VecDeque::new();
    let mut queue_paused = false;
    let mut resume_after_pause = false;
    let mut should_exit = false;

    while !should_exit {
        match command_rx.recv_timeout(Duration::from_millis(120)) {
            Ok(message) => handle_scheduler_message(
                message,
                &mut active,
                &mut waiting,
                &mut queue_paused,
                &mut resume_after_pause,
                &mut config,
                &event_tx,
                &worker_tx,
                &database,
                &mut should_exit,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        while let Ok(message) = command_rx.try_recv() {
            handle_scheduler_message(
                message,
                &mut active,
                &mut waiting,
                &mut queue_paused,
                &mut resume_after_pause,
                &mut config,
                &event_tx,
                &worker_tx,
                &database,
                &mut should_exit,
            );
        }

        while let Ok(message) = worker_rx.try_recv() {
            let SchedulerMessage::WorkerFinished {
                id,
                record,
                outcome,
            } = message
            else {
                continue;
            };
            active.remove(&id);
            if outcome == WorkerOutcome::Paused && resume_after_pause && !queue_paused {
                waiting.push_back(record);
            }
        }

        if should_exit {
            for job in active.values() {
                job.control.cancelled.store(true, Ordering::Release);
            }
            break;
        }

        if !queue_paused {
            while active.len() < config.max_concurrent_downloads {
                let Some(record) = waiting.pop_front() else {
                    break;
                };
                if active.contains_key(&record.id) {
                    continue;
                }
                spawn_worker(
                    record,
                    &config,
                    &mut active,
                    &event_tx,
                    &worker_tx,
                    &database,
                );
            }
        }
    }

    // Give running workers a bounded chance to observe shutdown before the scheduler exits.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !active.is_empty() && Instant::now() < deadline {
        for job in active.values() {
            job.control.cancelled.store(true, Ordering::Release);
        }
        while let Ok(message) = worker_rx.try_recv() {
            if let SchedulerMessage::WorkerFinished { id, .. } = message {
                active.remove(&id);
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_scheduler_message(
    message: SchedulerMessage,
    active: &mut HashMap<DownloadId, ActiveJob>,
    waiting: &mut VecDeque<DownloadRecord>,
    queue_paused: &mut bool,
    resume_after_pause: &mut bool,
    config: &mut EngineConfig,
    event_tx: &mpsc::Sender<DownloadEvent>,
    worker_tx: &mpsc::Sender<SchedulerMessage>,
    database: &SharedDatabase,
    should_exit: &mut bool,
) {
    let SchedulerMessage::Command(command) = message else {
        return;
    };
    match command {
        EngineCommand::Start(record) => {
            *queue_paused = false;
            enqueue_record(record, waiting, active, config, event_tx, worker_tx, database);
        }
        EngineCommand::StartQueue(records) => {
            *queue_paused = false;
            *resume_after_pause = false;
            for record in records {
                enqueue_record(record, waiting, active, config, event_tx, worker_tx, database);
            }
        }
        EngineCommand::Pause(id) => {
            if let Some(job) = active.get(&id) {
                job.control.paused.store(true, Ordering::Release);
            } else if let Some(index) = waiting.iter().position(|record| record.id == id) {
                let record = waiting.remove(index).expect("queue index must exist");
                persist_status(database, &record.id, DownloadStatus::Paused, None, None);
                let _ = event_tx.send(DownloadEvent::Paused {
                    id: record.id,
                    downloaded_bytes: record.downloaded_bytes,
                });
            }
        }
        EngineCommand::Resume(record) => {
            if let Some(job) = active.get(&record.id) {
                job.control.paused.store(false, Ordering::Release);
            } else {
                enqueue_record(record, waiting, active, config, event_tx, worker_tx, database);
            }
        }
        EngineCommand::Cancel(id) => {
            if let Some(job) = active.get(&id) {
                job.control.cancelled.store(true, Ordering::Release);
            } else if let Some(index) = waiting.iter().position(|record| record.id == id) {
                let record = waiting.remove(index).expect("queue index must exist");
                persist_status(database, &record.id, DownloadStatus::Cancelled, None, None);
                let _ = event_tx.send(DownloadEvent::Cancelled {
                    id: record.id,
                    downloaded_bytes: record.downloaded_bytes,
                });
            }
        }
        EngineCommand::PauseAll | EngineCommand::StopQueue => {
            *queue_paused = true;
            *resume_after_pause = false;
            for job in active.values() {
                job.control.paused.store(true, Ordering::Release);
            }
            for record in waiting.iter() {
                persist_status(database, &record.id, DownloadStatus::Paused, None, None);
            }
        }
        EngineCommand::ResumeAll => {
            *queue_paused = false;
            *resume_after_pause = true;
            for job in active.values() {
                job.control.paused.store(false, Ordering::Release);
            }
        }
        EngineCommand::SetConfig(new_config) => *config = new_config,
        EngineCommand::Shutdown => *should_exit = true,
    }
}

fn enqueue_record(
    record: DownloadRecord,
    waiting: &mut VecDeque<DownloadRecord>,
    active: &HashMap<DownloadId, ActiveJob>,
    config: &EngineConfig,
    event_tx: &mpsc::Sender<DownloadEvent>,
    worker_tx: &mpsc::Sender<SchedulerMessage>,
    database: &SharedDatabase,
) {
    if active.contains_key(&record.id) || waiting.iter().any(|item| item.id == record.id) {
        return;
    }
    // A worker is spawned by the scheduler loop, while this function only adds work. Keeping
    // the queue explicit makes ordering and persistence deterministic.
    waiting.push_back(record);
    let _ = (config, event_tx, worker_tx, database);
}

fn spawn_worker(
    record: DownloadRecord,
    config: &EngineConfig,
    active: &mut HashMap<DownloadId, ActiveJob>,
    event_tx: &mpsc::Sender<DownloadEvent>,
    worker_tx: &mpsc::Sender<SchedulerMessage>,
    database: &SharedDatabase,
) {
    let id = record.id.clone();
    let control = Arc::new(JobControl::new());
    active.insert(
        id.clone(),
        ActiveJob {
            control: Arc::clone(&control),
        },
    );
    let worker_config = config.clone();
    let worker_event_tx = event_tx.clone();
    let worker_result_tx = worker_tx.clone();
    let worker_database = Arc::clone(database);
    thread::Builder::new()
        .name(format!("pulse-download-{}", &id[..8.min(id.len())]))
        .spawn(move || {
            let outcome = run_download(
                record.clone(),
                worker_config,
                control,
                worker_database,
                worker_event_tx,
            );
            let _ = worker_result_tx.send(SchedulerMessage::WorkerFinished {
                id,
                record,
                outcome,
            });
        })
        .expect("download worker thread must start");
}

fn persist_status(
    database: &SharedDatabase,
    id: &str,
    status: DownloadStatus,
    error: Option<&str>,
    completed_at: Option<i64>,
) {
    if let Ok(database) = database.lock() {
        let _ = database.update_status(id, status, error, completed_at);
    }
}

#[derive(Debug)]
struct RemoteInfo {
    total_bytes: Option<u64>,
    accepts_ranges: bool,
    content_type: Option<String>,
}

fn build_client(config: &EngineConfig) -> Result<Client> {
    let mut headers = HeaderMap::new();
    let user_agent = HeaderValue::from_str(&config.user_agent)
        .map_err(|error| anyhow!("invalid User-Agent: {error}"))?;
    headers.insert(USER_AGENT, user_agent);
    for (name, value) in &config.additional_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| anyhow!("invalid header name {name}: {error}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| anyhow!("invalid value for header {name}: {error}"))?;
        headers.insert(name, value);
    }
    let mut builder = ClientBuilder::new()
        .default_headers(headers)
        .timeout(config.timeout)
        .connect_timeout(config.timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .cookie_store(true);
    if let Some(proxy) = &config.proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

fn probe(client: &Client, url: &str) -> Result<RemoteInfo> {
    let mut total_bytes = None;
    let mut content_type = None;
    let mut accepts_ranges = false;

    if let Ok(response) = client.head(url).send() {
        if response.status().is_success() {
            total_bytes = response.content_length();
            content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            accepts_ranges = response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.eq_ignore_ascii_case("bytes"))
                .unwrap_or(false);
        }
    }

    let response = client.get(url).header(RANGE, "bytes=0-0").send();
    if let Ok(response) = response {
        if response.status() == StatusCode::PARTIAL_CONTENT {
            accepts_ranges = true;
            if let Some(range) = response.headers().get(CONTENT_RANGE) {
                if let Ok(range) = range.to_str() {
                    if let Some(total) = range.split('/').nth(1).and_then(|value| value.parse().ok()) {
                        total_bytes = Some(total);
                    }
                }
            }
        } else if total_bytes.is_none() && response.status().is_success() {
            total_bytes = response.content_length();
        }
        if content_type.is_none() {
            content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
        }
    }

    Ok(RemoteInfo {
        total_bytes,
        accepts_ranges,
        content_type,
    })
}

fn run_download(
    mut record: DownloadRecord,
    config: EngineConfig,
    control: Arc<JobControl>,
    database: SharedDatabase,
    event_tx: mpsc::Sender<DownloadEvent>,
) -> WorkerOutcome {
    let id = record.id.clone();
    let client = match build_client(&config) {
        Ok(client) => client,
        Err(error) => {
            return fail_download(&record, &database, &event_tx, error.to_string(), 0);
        }
    };
    let target = match safe_target_path(&record) {
        Ok(path) => path,
        Err(error) => return fail_download(&record, &database, &event_tx, error.to_string(), 0),
    };
    if let Some(parent) = target.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return fail_download(&record, &database, &event_tx, error.to_string(), 0);
        }
    }

    persist_status(&database, &id, DownloadStatus::Downloading, None, None);
    let _ = event_tx.send(DownloadEvent::Started { id: id.clone() });

    let remote = match probe(&client, &record.url) {
        Ok(info) => info,
        Err(error) => return fail_download(&record, &database, &event_tx, error.to_string(), 0),
    };
    record.total_bytes = remote.total_bytes;
    record.content_type = remote.content_type.clone();
    if let Ok(database) = database.lock() {
        let _ = database.update_progress(&id, record.downloaded_bytes, record.total_bytes, 0.0, None);
    }
    let _ = event_tx.send(DownloadEvent::Metadata {
        id: id.clone(),
        total_bytes: remote.total_bytes,
        content_type: remote.content_type,
    });

    let partial_dir = partial_directory(&target, &id);
    if let Err(error) = fs::create_dir_all(&partial_dir) {
        return fail_download(&record, &database, &event_tx, error.to_string(), record.downloaded_bytes);
    }

    let segment_count = match remote.total_bytes {
        Some(total) if remote.accepts_ranges && total > 0 => {
            let requested = record.connections.clamp(1, config.max_connections_per_download);
            requested.min(total.div_ceil(512 * 1024).max(1) as u32).max(1) as usize
        }
        _ => 1,
    };
    let segments = make_segments(remote.total_bytes, segment_count);
    let total_downloaded = Arc::new(AtomicU64::new(0));
    for (index, _segment) in segments.iter().enumerate() {
        let path = partial_dir.join(format!("{index:04}.part"));
        if let Ok(length) = fs::metadata(path).map(|metadata| metadata.len()) {
            total_downloaded.fetch_add(length, Ordering::AcqRel);
        }
    }

    let _ = event_tx.send(DownloadEvent::Progress {
        id: id.clone(),
        downloaded_bytes: total_downloaded.load(Ordering::Acquire),
        total_bytes: remote.total_bytes,
        speed_bps: 0.0,
        eta_seconds: None,
    });

    let tracker = Arc::new(ProgressTracker::new(
        id.clone(),
        remote.total_bytes,
        Arc::clone(&total_downloaded),
        Arc::clone(&database),
        event_tx.clone(),
    ));
    let limiter = Arc::new(RateLimiter::new(config.speed_limit_bps));
    let abort_on_error = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(segments.len());

    for (index, segment) in segments.iter().copied().enumerate() {
        let worker_client = client.clone();
        let worker_control = Arc::clone(&control);
        let worker_tracker = Arc::clone(&tracker);
        let worker_limiter = Arc::clone(&limiter);
        let worker_abort = Arc::clone(&abort_on_error);
        let worker_url = record.url.clone();
        let worker_dir = partial_dir.clone();
        let accepts_ranges = remote.accepts_ranges;
        let retries = config.retry_count;
        handles.push(thread::spawn(move || {
            download_segment(
                worker_client,
                &worker_url,
                worker_dir.join(format!("{index:04}.part")),
                segment,
                accepts_ranges,
                retries,
                worker_control,
                worker_tracker,
                worker_limiter,
                worker_abort,
            )
        }));
    }

    let mut fatal_error = None;
    let mut was_paused = false;
    let mut was_cancelled = false;
    for handle in handles {
        match handle.join() {
            Ok(Ok(SegmentOutcome::Completed)) => {}
            Ok(Ok(SegmentOutcome::Paused)) => was_paused = true,
            Ok(Ok(SegmentOutcome::Cancelled)) => was_cancelled = true,
            Ok(Err(error)) => fatal_error = Some(error.to_string()),
            Err(_) => fatal_error = Some("A download worker thread panicked".to_owned()),
        }
    }

    let downloaded = total_downloaded.load(Ordering::Acquire);
    if control.cancelled.load(Ordering::Acquire) || was_cancelled {
        persist_status(&database, &id, DownloadStatus::Cancelled, None, None);
        let _ = event_tx.send(DownloadEvent::Cancelled {
            id,
            downloaded_bytes: downloaded,
        });
        return WorkerOutcome::Cancelled;
    }
    if control.paused.load(Ordering::Acquire) || was_paused {
        persist_status(&database, &record.id, DownloadStatus::Paused, None, None);
        let _ = event_tx.send(DownloadEvent::Paused {
            id,
            downloaded_bytes: downloaded,
        });
        return WorkerOutcome::Paused;
    }
    if let Some(error) = fatal_error {
        return fail_download(&record, &database, &event_tx, error, downloaded);
    }

    if let Err(error) = assemble_segments(
        &partial_dir,
        &target,
        &segments,
        record.expected_sha256.as_deref(),
    ) {
        return fail_download(&record, &database, &event_tx, error.to_string(), downloaded);
    }
    let final_size = fs::metadata(&target).map(|metadata| metadata.len()).unwrap_or(downloaded);
    if let Err(error) = fs::remove_dir_all(&partial_dir) {
        // A completed file is valid even if cleanup was delayed by antivirus software. Keep the
        // warning in the persisted error column only when the file itself is not present.
        if !target.exists() {
            return fail_download(&record, &database, &event_tx, error.to_string(), final_size);
        }
    }
    if let Ok(database) = database.lock() {
        let _ = database.update_progress(&record.id, final_size, record.total_bytes, 0.0, Some(0));
        let _ = database.update_status(
            &record.id,
            DownloadStatus::Completed,
            None,
            Some(chrono::Utc::now().timestamp()),
        );
    }
    let _ = event_tx.send(DownloadEvent::Progress {
        id: record.id.clone(),
        downloaded_bytes: final_size,
        total_bytes: record.total_bytes.or(Some(final_size)),
        speed_bps: 0.0,
        eta_seconds: Some(0),
    });
    let _ = event_tx.send(DownloadEvent::Completed {
        id: record.id,
        path: target.to_string_lossy().into_owned(),
    });
    WorkerOutcome::Completed
}

fn safe_target_path(record: &DownloadRecord) -> Result<PathBuf> {
    let requested = PathBuf::from(&record.save_path);
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = safe_file_name(&record.file_name);
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    };
    let candidate = parent.join(file_name);
    if candidate.file_name().is_none() {
        bail!("Download destination has no file name");
    }
    Ok(candidate)
}

fn fail_download(
    record: &DownloadRecord,
    database: &SharedDatabase,
    event_tx: &mpsc::Sender<DownloadEvent>,
    error: String,
    downloaded_bytes: u64,
) -> WorkerOutcome {
    if let Ok(database) = database.lock() {
        let _ = database.update_progress(
            &record.id,
            downloaded_bytes,
            record.total_bytes,
            0.0,
            None,
        );
        let _ = database.update_status(&record.id, DownloadStatus::Failed, Some(&error), None);
    }
    let _ = event_tx.send(DownloadEvent::Failed {
        id: record.id.clone(),
        downloaded_bytes,
        error,
    });
    WorkerOutcome::Failed
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    start: u64,
    end: Option<u64>,
}

fn make_segments(total: Option<u64>, count: usize) -> Vec<Segment> {
    match total {
        Some(total) if total > 0 => {
            let count = count.max(1).min(total as usize);
            let chunk = total.div_ceil(count as u64);
            (0..count)
                .map(|index| {
                    let start = index as u64 * chunk;
                    let end = ((start + chunk).min(total)).saturating_sub(1);
                    Segment {
                        start,
                        end: Some(end),
                    }
                })
                .filter(|segment| segment.start <= segment.end.unwrap_or(0))
                .collect()
        }
        _ => vec![Segment { start: 0, end: None }],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentOutcome {
    Completed,
    Paused,
    Cancelled,
}

fn download_segment(
    client: Client,
    url: &str,
    path: PathBuf,
    segment: Segment,
    accepts_ranges: bool,
    retry_count: u32,
    control: Arc<JobControl>,
    tracker: Arc<ProgressTracker>,
    limiter: Arc<RateLimiter>,
    abort_on_error: Arc<AtomicBool>,
) -> Result<SegmentOutcome> {
    let expected_length = segment.end.map(|end| end - segment.start + 1);
    if let Some(expected) = expected_length {
        if fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0) >= expected {
            return Ok(SegmentOutcome::Completed);
        }
    }

    let mut last_error = None;
    for attempt in 0..=retry_count {
        if control.cancelled.load(Ordering::Acquire) {
            return Ok(SegmentOutcome::Cancelled);
        }
        if control.paused.load(Ordering::Acquire) {
            return Ok(SegmentOutcome::Paused);
        }
        if abort_on_error.load(Ordering::Acquire) {
            bail!("another segment failed");
        }

        let mut current = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        if !accepts_ranges && current > 0 {
            current = 0;
            let _ = File::create(&path)?;
        }
        let request_start = segment.start.saturating_add(current);
        let mut request = client.get(url);
        if accepts_ranges {
            if let Some(end) = segment.end {
                if request_start > end {
                    return Ok(SegmentOutcome::Completed);
                }
                request = request.header(RANGE, format!("bytes={request_start}-{end}"));
            } else if request_start > 0 {
                request = request.header(RANGE, format!("bytes={request_start}-"));
            }
        }

        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(error.to_string());
                retry_sleep(attempt, retry_count, &control);
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            last_error = Some(format!("server returned HTTP {status}"));
            retry_sleep(attempt, retry_count, &control);
            continue;
        }
        if accepts_ranges && segment.end.is_some() && status != StatusCode::PARTIAL_CONTENT {
            last_error = Some("server ignored the HTTP Range request".to_owned());
            retry_sleep(attempt, retry_count, &control);
            continue;
        }

        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut response = response;
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut read_error = None;
        loop {
            if control.cancelled.load(Ordering::Acquire) {
                return Ok(SegmentOutcome::Cancelled);
            }
            if control.paused.load(Ordering::Acquire) {
                return Ok(SegmentOutcome::Paused);
            }
            let bytes = match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(bytes) => bytes,
                Err(error) => {
                    read_error = Some(error.to_string());
                    break;
                }
            };
            limiter.throttle(bytes as u64, &control);
            file.write_all(&buffer[..bytes])?;
            tracker.add(bytes as u64);
            if let Some(expected) = expected_length {
                let length = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
                if length > expected {
                    abort_on_error.store(true, Ordering::Release);
                    bail!("server sent more bytes than the requested segment");
                }
            }
        }
        file.flush()?;
        let current = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        if let Some(expected) = expected_length {
            if current >= expected {
                return Ok(SegmentOutcome::Completed);
            }
            if read_error.is_none() {
                read_error = Some(format!("connection ended early ({current}/{expected} bytes)"));
            }
        } else if read_error.is_none() {
            return Ok(SegmentOutcome::Completed);
        }
        last_error = read_error.or_else(|| Some("download stream ended early".to_owned()));
        retry_sleep(attempt, retry_count, &control);
    }
    abort_on_error.store(true, Ordering::Release);
    Err(anyhow!(
        "segment download failed after retries: {}",
        last_error.unwrap_or_else(|| "unknown network error".to_owned())
    ))
}

fn retry_sleep(attempt: u32, retry_count: u32, control: &JobControl) {
    if attempt >= retry_count {
        return;
    }
    let seconds = 2_u64.saturating_pow(attempt.min(4)).min(16);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if control.cancelled.load(Ordering::Acquire) || control.paused.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct ProgressTracker {
    id: DownloadId,
    total: Option<u64>,
    downloaded: Arc<AtomicU64>,
    database: SharedDatabase,
    event_tx: mpsc::Sender<DownloadEvent>,
    state: Mutex<ProgressState>,
}

struct ProgressState {
    last_report: Instant,
    last_bytes: u64,
    started: Instant,
}

impl ProgressTracker {
    fn new(
        id: DownloadId,
        total: Option<u64>,
        downloaded: Arc<AtomicU64>,
        database: SharedDatabase,
        event_tx: mpsc::Sender<DownloadEvent>,
    ) -> Self {
        let now = Instant::now();
        let initial = downloaded.load(Ordering::Acquire);
        Self {
            id,
            total,
            downloaded,
            database,
            event_tx,
            state: Mutex::new(ProgressState {
                last_report: now,
                last_bytes: initial,
                started: now,
            }),
        }
    }

    fn add(&self, bytes: u64) {
        let current = self.downloaded.fetch_add(bytes, Ordering::AcqRel) + bytes;
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_report);
        if elapsed < Duration::from_millis(250) && self.total.map(|total| current < total).unwrap_or(true) {
            return;
        }
        let interval_speed = (current.saturating_sub(state.last_bytes)) as f64
            / elapsed.as_secs_f64().max(0.001);
        let average_speed = current as f64 / now.duration_since(state.started).as_secs_f64().max(0.001);
        let speed = if interval_speed.is_finite() && interval_speed > 0.0 {
            interval_speed
        } else {
            average_speed
        };
        let eta = self.total.and_then(|total| {
            (speed > 0.0 && current < total).then_some(((total - current) as f64 / speed) as u64)
        });
        state.last_report = now;
        state.last_bytes = current;
        drop(state);
        if let Ok(database) = self.database.lock() {
            let _ = database.update_progress(&self.id, current, self.total, speed, eta);
        }
        let _ = self.event_tx.send(DownloadEvent::Progress {
            id: self.id.clone(),
            downloaded_bytes: current,
            total_bytes: self.total,
            speed_bps: speed,
            eta_seconds: eta,
        });
    }
}

struct RateLimiter {
    limit_bps: Option<u64>,
    state: Mutex<RateState>,
}

struct RateState {
    started: Instant,
    bytes: u64,
}

impl RateLimiter {
    fn new(limit_bps: Option<u64>) -> Self {
        Self {
            limit_bps,
            state: Mutex::new(RateState {
                started: Instant::now(),
                bytes: 0,
            }),
        }
    }

    fn throttle(&self, bytes: u64, control: &JobControl) {
        let Some(limit) = self.limit_bps.filter(|limit| *limit > 0) else {
            return;
        };
        let sleep_for = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.bytes = state.bytes.saturating_add(bytes);
            let expected = Duration::from_secs_f64(state.bytes as f64 / limit as f64);
            let elapsed = state.started.elapsed();
            expected.saturating_sub(elapsed)
        };
        let deadline = Instant::now() + sleep_for;
        while Instant::now() < deadline {
            if control.cancelled.load(Ordering::Acquire) || control.paused.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn assemble_segments(
    partial_dir: &Path,
    target: &Path,
    segments: &[Segment],
    expected_sha256: Option<&str>,
) -> Result<()> {
    let temporary = partial_dir.join("assembled.tmp");
    let mut output = File::create(&temporary)
        .with_context(|| format!("cannot create temporary output {}", temporary.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    for (index, segment) in segments.iter().enumerate() {
        let part = partial_dir.join(format!("{index:04}.part"));
        let metadata = fs::metadata(&part)
            .with_context(|| format!("missing segment {}", part.display()))?;
        if let Some(end) = segment.end {
            let expected = end - segment.start + 1;
            if metadata.len() != expected {
                bail!("incomplete segment {} ({}/{})", index + 1, metadata.len(), expected);
            }
        }
        let mut input = File::open(&part)?;
        loop {
            let bytes = input.read(&mut buffer)?;
            if bytes == 0 {
                break;
            }
            output.write_all(&buffer[..bytes])?;
            hasher.update(&buffer[..bytes]);
        }
    }
    output.flush()?;
    output.sync_all()?;
    if let Some(expected) = expected_sha256.filter(|value| !value.trim().is_empty()) {
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            let _ = fs::remove_file(&temporary);
            bail!("SHA-256 mismatch (expected {expected}, got {actual})");
        }
    }
    if target.exists() {
        fs::remove_file(target)
            .with_context(|| format!("cannot replace existing file {}", target.display()))?;
    }
    fs::rename(&temporary, target)
        .with_context(|| format!("cannot finalize downloaded file {}", target.display()))?;
    Ok(())
}

