use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::models::AppItem;
use crate::utils::validate_download_url;

#[derive(Debug, Default)]
pub struct AppsState {
    pub items: Vec<AppItem>,
    pub loading: bool,
    pub error: Option<String>,
    pub last_updated: Option<String>,
    receiver: Option<mpsc::Receiver<Result<Vec<AppItem>>>>,
    pub selected: Option<usize>,
}

impl AppsState {
    pub fn refresh(&mut self, endpoint: &str) {
        if self.loading {
            return;
        }
        let endpoint = endpoint.trim().to_owned();
        self.loading = true;
        self.error = None;
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        thread::Builder::new()
            .name("pulse-app-catalog".to_owned())
            .spawn(move || {
                let result = fetch_catalog(&endpoint);
                let _ = sender.send(result);
            })
            .expect("app catalog thread must start");
    }

    pub fn poll(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(items)) => {
                self.items = items;
                self.loading = false;
                self.last_updated = Some(chrono::Local::now().format("%H:%M").to_string());
            }
            Ok(Err(error)) => {
                self.loading = false;
                self.error = Some(error.to_string());
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.receiver = Some(receiver);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = false;
                self.error = Some("The catalog request ended unexpectedly".to_owned());
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogResponse {
    Array(Vec<AppItem>),
    Envelope { apps: Vec<AppItem> },
}

fn fetch_catalog(endpoint: &str) -> Result<Vec<AppItem>> {
    let parsed = url::Url::parse(endpoint).context("Apps API URL is not valid")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("Apps API URL must use HTTP or HTTPS");
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("PulseDownloadManager/0.1")
        .build()?;
    let response = client.get(parsed).send()?.error_for_status()?;
    let payload: CatalogResponse = response.json().context("Apps API returned invalid JSON")?;
    let items = match payload {
        CatalogResponse::Array(items) => items,
        CatalogResponse::Envelope { apps } => apps,
    };
    let mut valid = Vec::with_capacity(items.len());
    for item in items {
        validate_download_url(&item.download_url)
            .map_err(|error| anyhow::anyhow!("App {} has an invalid download URL: {error}", item.name))?;
        valid.push(item);
    }
    Ok(valid)
}
