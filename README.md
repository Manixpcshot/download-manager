# Pulse Download Manager

Pulse is a native Windows 10/11 download manager written in Rust. It is designed as a real desktop application rather than a browser shell: the interface is compiled and rendered by `eframe/egui`, the download engine is independent of the UI, and persistent state is kept in a local SQLite database.

> Project status: **functional 0.1 release candidate**. The core HTTP engine, segmented downloads, persistence, queue, catalog API client, tray menu, settings, and Windows startup integration are implemented. The Windows CI workflow is the source of release binaries.

## What is included

- Native dark-first Windows desktop UI with a resizable layout, sidebar, search, sorting, cards, progress, speed and ETA.
- HTTP/HTTPS downloads through `reqwest` with Rustls certificate validation, redirect limits, timeout, proxy and custom headers.
- HTTP Range probing and concurrent segment workers for servers that advertise byte ranges.
- Safe pause/resume/cancel behavior, retry with exponential backoff, rate limiting, temporary segment files, atomic finalization and optional SHA-256 verification.
- Queue controls, priority ordering, maximum concurrent downloads and per-download connection count.
- SQLite persistence in the per-user local data directory. A download can be resumed after restarting the application.
- Native system-tray menu: Open, Pause All, Resume All and Exit.
- Apps catalog loaded from a real JSON HTTP endpoint. The catalog URL is configurable and its download links are validated before they are offered.
- Add URL, paste URL, drag a URL or Windows `.url` shortcut into the window, copy URL, open file, open folder, retry and delete-entry actions.
- Settings for startup, tray behavior, download folder, concurrency, speed cap, timeout, retry count, proxy, User-Agent, headers, theme, accent and UI scale.
- No automatic execution of downloaded installers. Opening a file is always an explicit user action.

## Repository layout

```text
.
├── Cargo.toml
├── build.rs
├── apps.json                         # Example catalog consumed by the Apps screen
├── docs/
│   └── API.md                        # Apps API contract
├── .github/workflows/
│   ├── ci.yml                        # Windows build/test/check
│   └── release.yml                   # Tagged Windows EXE + SHA-256 artifact
└── src/
    ├── main.rs                       # Native window bootstrap
    ├── app.rs                        # Application state and screens
    ├── models.rs                     # Download/catalog data models
    ├── settings.rs                   # JSON settings and paths
    ├── database.rs                   # SQLite schema and repository methods
    ├── downloader/
    │   ├── mod.rs
    │   └── engine.rs                 # Scheduler, workers, ranges, retry, hash
    ├── queue.rs                      # Queue behavior is coordinated by engine/app
    ├── apps.rs                       # Catalog HTTP client
    ├── notifications.rs              # In-app notification center
    ├── tray.rs                       # Native tray icon and menu
    ├── system.rs                     # Windows startup and file actions
    ├── ui/theme.rs                   # Compiled UI theme primitives
    └── utils.rs                      # Paths, validation and formatting helpers
```

`queue.rs` is represented by the scheduler in `downloader/engine.rs`; the scheduler owns active workers and the persisted app layer owns ordering/priority. If queue policy grows further, that boundary is the intended extraction point.

## Prerequisites

- Windows 10 or Windows 11 (x64)
- Rust stable through [rustup](https://rustup.rs/)
- Visual Studio 2022 Build Tools with **Desktop development with C++** and the Windows 10/11 SDK

The app uses the native Windows windowing stack through `eframe/winit`; it does not embed a web page or require a JavaScript runtime.

## Run from source

PowerShell:

```powershell
git clone https://github.com/Manixpcshot/download-manager.git
cd download-manager
cargo run
```

The default catalog URL points to the repository's `apps.json` on GitHub. If the repository branch is not public or has not been merged, set a compatible endpoint under **Settings → General**. The included `apps.json` can also be served locally for development.

## Build a Windows release EXE

```powershell
rustup default stable-x86_64-pc-windows-msvc
cargo clean
cargo build --release
```

The executable is written to:

```text
target\release\pulse-download-manager.exe
```

The optimized profile enables LTO, one codegen unit, symbol stripping and `panic = "abort"`. For a portable ZIP with a checksum:

```powershell
.\build-release.ps1
```

The script creates `dist\PulseDownloadManager-v0.1.0-windows-x64.zip` and a matching `.sha256` file. The GitHub Actions release workflow performs the same packaging on a Windows runner when a `v*` tag is pushed.

## Local data and settings

The application never stores downloads inside the repository. On Windows the paths resolve under the per-user locations provided by the `dirs` crate:

- Settings: `%APPDATA%\PulseDownloadManager\settings.json`
- Database: `%LOCALAPPDATA%\PulseDownloadManager\downloads.sqlite3`
- Temporary segment directories: hidden `.pulse-download` directories next to the selected destination

Temporary segments remain after a pause or recoverable failure and are removed only after successful assembly. The final file is assembled in a temporary file and renamed into place; a partial output is not presented as a completed download.

## Apps API

See [docs/API.md](docs/API.md) for the request, response schema, validation rules and a complete example. The client accepts either:

```json
[{ "id": "...", "name": "...", "download_url": "https://..." }]
```

or:

```json
{ "apps": [{ "id": "...", "name": "...", "download_url": "https://..." }] }
```

All other fields in the documented contract are required for a production catalog. An invalid download URL is rejected before the item reaches the UI.

## Browser hand-off / integration

The application intentionally does not become a browser. The safe integration boundary is an external helper or browser extension that sends a validated `http`/`https` URL to a local integration endpoint or opens a `.url` shortcut for the user to drop into Pulse. The current desktop build supports `.url` drag-and-drop and keeps the download engine independent from any browser process. A future helper can be added without changing the UI or granting the manager access to browser cookies.

## Security notes

- HTTPS uses normal Rustls certificate validation; invalid certificates are not accepted.
- Redirects are limited to ten hops.
- URL schemes are restricted to HTTP and HTTPS.
- URL-derived filenames are sanitized against separators, control characters, Windows reserved names and traversal.
- Temporary files are kept beside the requested destination in an ID-scoped directory, not in a user-controlled URL path.
- SHA-256 can be entered for a URL download or supplied by the catalog API.
- Installers/files are never launched automatically. `Open file` is an explicit action.
- Proxy credentials, if entered, are stored in the per-user settings file. Use an OS-protected account and restrict access to that profile.

## Tests and CI

Run formatting, linting and tests locally:

```powershell
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
```

The CI workflow runs those checks on `windows-latest` and also builds the release EXE. The release workflow attaches the ZIP and checksum to the GitHub release.

## License

MIT. See the source distribution for the license text.
