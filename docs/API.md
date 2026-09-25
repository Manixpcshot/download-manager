# Pulse Apps API contract

The **Apps** screen is populated from an HTTP JSON endpoint. The desktop client fetches the endpoint with Rust's HTTP client, validates every `download_url` as `http` or `https`, and only then exposes an item to the UI.

The default endpoint is:

```text
https://raw.githubusercontent.com/Manixpcshot/download-manager/main/apps.json
```

It can be changed in **Settings → General → Apps API endpoint**. HTTPS and a trusted certificate are recommended for production catalogs.

## Request

```http
GET /v1/apps HTTP/1.1
Accept: application/json
User-Agent: PulseDownloadManager/0.1
```

The client follows up to ten HTTP redirects and has a 30-second request timeout for the catalog request.

## Response

The server may return either a top-level array or an object with an `apps` property. The object form is preferred because it can be extended later.

```json
{
  "apps": [
    {
      "id": "7zip",
      "name": "7-Zip",
      "icon_url": "https://catalog.example/assets/7zip.png",
      "short_description": "Open-source file archiver.",
      "version": "24.09",
      "size_bytes": 1605632,
      "developer": "Igor Pavlov",
      "updated_at": "2025-01-05",
      "category": "Utilities",
      "download_url": "https://downloads.example/7z2409-x64.exe",
      "details_url": "https://www.7-zip.org/",
      "sha256": "optional-lowercase-or-uppercase-64-hex-characters"
    }
  ]
}
```

### Fields

| Field | Type | Required | Description |
|---|---|---:|---|
| `id` | string | yes | Stable catalog identifier. |
| `name` | string | yes | Human-readable application name. |
| `icon_url` | string or null | no | Metadata for a catalog icon. The current client also renders a deterministic local monogram, so an unavailable image never blocks the catalog. |
| `short_description` | string | yes | One or two sentence description. |
| `version` | string | yes | Version shown to the user. |
| `size_bytes` | unsigned integer or null | no | Expected installer size. The engine probes the actual response as well. |
| `developer` | string | yes | Publisher/developer name. |
| `updated_at` | ISO-8601/date string | yes | Catalog update date. |
| `category` | string | yes | Category used for display and search. |
| `download_url` | URL string | yes | Installer URL. Must use `http` or `https`. |
| `details_url` | URL string or null | no | External product page opened only after an explicit user action. |
| `sha256` | string or null | no | Expected SHA-256 for end-to-end integrity verification. |

Invalid entries cause the catalog request to fail rather than silently offering an unsafe or malformed link.

## Example local API

A minimal compatible endpoint can be served during development with any static HTTP server from the repository root:

```powershell
python -m http.server 8080
```

Then set the endpoint to:

```text
http://127.0.0.1:8080/apps.json
```

This local address is for development only; production catalog deployments should use HTTPS and a trusted host.
