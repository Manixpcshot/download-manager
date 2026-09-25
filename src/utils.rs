use egui::Color32;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

pub fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if size >= 100.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

pub fn format_speed(value: f64) -> String {
    if value <= 0.0 {
        "—".to_owned()
    } else {
        format!("{}/s", format_bytes(value as u64))
    }
}

pub fn format_eta(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".to_owned(),
        Some(value) if value < 60 => format!("{value}s left"),
        Some(value) if value < 3600 => format!("{}m {:02}s left", value / 60, value % 60),
        Some(value) => format!("{}h {:02}m left", value / 3600, (value % 3600) / 60),
    }
}

pub fn safe_file_name(input: &str) -> String {
    let candidate = Path::new(input)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(input);
    let mut sanitized: String = candidate
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    sanitized = sanitized.trim().trim_matches('.').to_owned();
    if sanitized.is_empty() {
        sanitized = "download.bin".to_owned();
    }
    let stem_upper = sanitized
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        stem_upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        sanitized.insert(0, '_');
    }
    if sanitized.chars().count() > 180 {
        sanitized = sanitized.chars().take(180).collect();
        sanitized = sanitized.trim_end_matches(&[' ', '.'][..]).to_owned();
    }
    sanitized
}

pub fn guess_file_name(raw_url: &str) -> String {
    Url::parse(raw_url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
                .map(safe_file_name)
        })
        .filter(|name| !name.is_empty() && name != ".")
        .unwrap_or_else(|| "download.bin".to_owned())
}

pub fn validate_download_url(raw_url: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw_url.trim()).map_err(|error| format!("Invalid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS URLs are supported".to_owned());
    }
    if parsed.host_str().is_none() {
        return Err("The URL must include a host name".to_owned());
    }
    Ok(parsed)
}

pub fn parse_url_file(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let value = line.trim();
        if let Some(url) = value.strip_prefix("URL=") {
            if validate_download_url(url).is_ok() {
                return Some(url.to_owned());
            }
        }
        if validate_download_url(value).is_ok() {
            return Some(value.to_owned());
        }
    }
    None
}

pub fn partial_directory(target: &Path, id: &str) -> PathBuf {
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{id}.pulse-download"))
}

pub fn header_lines(value: &str) -> Vec<(String, String)> {
    value
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                None
            } else {
                Some((name.to_owned(), value.to_owned()))
            }
        })
        .collect()
}

pub fn parse_hex_color(value: &str, fallback: Color32) -> Color32 {
    let raw = value.trim().trim_start_matches('#');
    if raw.len() != 6 {
        return fallback;
    }
    let Ok(red) = u8::from_str_radix(&raw[0..2], 16) else {
        return fallback;
    };
    let Ok(green) = u8::from_str_radix(&raw[2..4], 16) else {
        return fallback;
    };
    let Ok(blue) = u8::from_str_radix(&raw[4..6], 16) else {
        return fallback;
    };
    Color32::from_rgb(red, green, blue)
}

pub fn is_url_like(value: &str) -> bool {
    validate_download_url(value).is_ok()
}

pub fn duration_from_secs(value: u64) -> Duration {
    Duration::from_secs(value.clamp(1, 600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_path_traversal_and_windows_names() {
        assert_eq!(safe_file_name(r"..\\CON?.exe"), "CON_.exe");
        assert_eq!(safe_file_name("report:2026.pdf"), "report_2026.pdf");
    }

    #[test]
    fn validates_only_http_urls() {
        assert!(validate_download_url("https://example.com/file.zip").is_ok());
        assert!(validate_download_url("file:///C:/secret").is_err());
        assert!(validate_download_url("javascript:alert(1)").is_err());
    }
}
