$ErrorActionPreference = "Stop"

cargo build --release
New-Item -ItemType Directory -Force -Path dist | Out-Null
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches.Groups[1].Value
$archive = "dist/PulseDownloadManager-v$version-windows-x64.zip"
if (Test-Path $archive) { Remove-Item $archive -Force }
Compress-Archive -Path target/release/pulse-download-manager.exe -DestinationPath $archive
$hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $(Split-Path $archive -Leaf)" | Set-Content "$archive.sha256"
Write-Host "Created $archive"
Write-Host "Created $archive.sha256"
