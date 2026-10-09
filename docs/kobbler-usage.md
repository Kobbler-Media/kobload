# Kobbler's Kobload Integration

This document details how Kobbler integrates with Kobload for book acquisition.

## 1. Want Request (Automated Book Search)

**Call Site**: User marks a book as "wanted"
- UI: Click "Want" button on book detail page, series page, or author page
- Route: `POST /api/v1/library/books/{book_id}/want` → `routes/library/wanted.rs:18-67`
- Job: `book_want` job enqueued → `jobs/handlers/book_want.rs`

**Method**: NATS job `enqueue_job("kobload", "want_request", payload)`

**Payload** (`nats_jobs::WantRequest`):
```json
{
  "source_system": "kobbler",
  "source_id": "<book_id>",
  "callback_url": "<kobload_ingest_url from config>",
  "auth_token": null,
  "patterns": [],
  "match_targets": [],
  "match_mode": "fuzzy",
  "min_score": 0.65,
  "early_exit_score": 0.85,
  "auto_download": true,
  "limit": 100,
  "metadata": {
    "book_id": "<book_id>",
    "title": "Book Title",
    "author": "Author Name"
  },
  "media_want": {
    "Book": {
      "title": "Book Title",
      "author": "Author Name",
      "isbn": "9781234567890",
      "format_preferences": ["epub", "kepub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr"],
      "identifiers": {
        "isbn10": "1234567890",
        "isbn13": "9781234567890"
      }
    }
  }
}
```

**Expected**:
1. Kobload searches configured indexers for matching releases
2. Creates a watch pattern for ongoing monitoring
3. If match found (score >= min_score), starts download automatically
4. Sends `WantResponse` event back via NATS

---

## 2. Want Response Handler

**Call Site**: NATS subscriber receives response from Kobload
- Subscriber: `subscribe_want_response_events()` in `nats.rs:636-713`
- Subject: `events.want.response.kobbler`
- Handler: `handle_want_response()` in `nats.rs:716-832`

**Payload** (`nats_jobs::WantResponse`):
```json
{
  "source_system": "kobbler",
  "source_id": "<book_id>",
  "watch_pattern_id": "<uuid>",
  "status": "downloading|watching|failed",
  "download_id": "<uuid or null>",
  "results_found": 42,
  "best_score": 0.87,
  "matched_pattern": "Author - Title",
  "matched_target_id": null,
  "metadata": { "book_id": "..." }
}
```

**Expected**:
- `Downloading`: Update book.want_status to "downloading", create wanted_attempt
- `Watching`: Update book.want_status to "watching" (no immediate match found)
- `Failed`: Update book.want_status to "search_failed"
- Broadcast `LibraryEvent::wanted_status_updated` for UI

---

## 3. Download Completion Webhook

**Call Site**: Kobload completes a download and POSTs to callback URL
- Route: `POST /api/v1/ingest/{token}`
- Handler: `kobload_ingest()` in `routes/ingest/kobload.rs:214-232`
- Implementation: `process_kobload_ingest_impl()` in `routes/ingest/kobload.rs:250-997`

**Payload** (`KobloadCallbackRequest`):
```json
{
  "download_id": "<uuid>",
  "status": "completed",
  "metadata": {
    "book_id": "<uuid>",
    "title": "Book Title",
    "author": "Author Name"
  },
  "files": [
    {
      "id": "<uuid>",
      "path": "dir/Book.epub",
      "url": "/api/v1/downloads/<id>/files/dir%2FBook.epub",
      "size": 1234567,
      "extension": "epub",
      "mime_type": "application/epub+zip",
      "score": 100
    }
  ],
  "file_groups": [
    {
      "name": "Book Title",
      "formats": ["epub", "mobi"],
      "best_path": "dir/Book.epub"
    }
  ],
  "error": null,
  "recoverable_error": null
}
```

**Expected**:
1. Validate ingest token against `ingest_tokens` table
2. Update `wanted_attempts.status` to "ingesting"
3. Classify files: single book vs multiple books
4. For each ebook file:
   - Download from Kobload: `GET /api/v1/downloads/{id}/files/{path}`
   - Save to watch directory or `storage_root/downloads`
   - Create `ingest_queue` item with metadata
5. Confirm import to Kobload: `POST /api/v1/downloads/{id}/imported`
6. Mark book as in_library, update attempt status to "completed"
7. Delete watch pattern via NATS

---

## 4. Manual Search & Download

**Call Site**: User searches for releases in ReleaseSearchModal
- UI Components:
  - `/library` page: Click search icon on BookCard → `web/src/routes/library/+page.svelte:1153`
  - `/library/books/[id]` page: Search button → `web/src/routes/(details)/library/books/[id]/+page.svelte:351`
  - `/series/[id]` page: Search button on book → `web/src/routes/(details)/series/[id]/+page.svelte:282`
  - `/authors/[id]` page: Search button on book → `web/src/routes/(details)/authors/[id]/+page.svelte:275`
- Component: `ReleaseSearchModal.svelte`

### 4a. Search Releases

**Route**: `GET /api/v1/library/search/releases?q=<query>&book_id=<optional>`
**Handler**: `search_library_releases()` in `routes/library/search_download.rs:74-205`

**Method**: HTTP via KobloadClient
```rust
client.search(&query.q, None, None).await
// Calls: POST {kobload}/api/v1/search
```

**Payload sent to Kobload**:
```json
{
  "query": "Brandon Sanderson Mistborn",
  "protocol": null,
  "media_want": null
}
```

**Response**: Array of releases from configured indexers, annotated with `previously_downloaded` if book_id provided

### 4b. Download Selected Release

**Route**: `POST /api/v1/library/search/download`
**Handler**: `download_to_library()` in `routes/library/search_download.rs:209-443`

**Request Payload**:
```json
{
  "release": { /* Prowlarr release object */ },
  "book_id": "<uuid or null>",
  "create_book": {
    "title": "Book Title",
    "author": "Author Name",
    "series": "Series Name",
    "series_index": 1.0
  }
}
```

**Method**: HTTP via KobloadClient
```rust
client.create_download(&request).await
// Calls: POST {kobload}/api/v1/downloads
```

**Payload sent to Kobload** (`DownloadRequest`):
```json
{
  "source": "prowlarr",
  "protocol": "torrent",
  "name": "Release Filename.epub",
  "download_url": "magnet:?xt=urn:btih:...",
  "callback_url": "<kobload_ingest_url>",
  "auth_token": null,
  "metadata": {
    "book_id": "<uuid>",
    "provider": "hardcover",
    "title": "Book Title",
    "author": "Author Name",
    "series": "Series Name",
    "series_index": 1.0,
    "release_title": "Release Title",
    "watch_directory_id": "<uuid or null>"
  },
  "release": { /* full release object */ }
}
```

**Expected**:
1. Creates wanted_attempt record
2. Returns `{ book_id, attempt_id, download_id }`
3. Download completion handled via webhook (section 3)

---

## 5. Watch Pattern Management

### 5a. Delete Watch Pattern (on book acquired)

**Call Site**: Book is successfully ingested
- Handler: `on_book_acquired()` in `wanted/acquisition.rs:19-41`
- Called from: `process_kobload_ingest_impl()` after successful ingest

**Method**: NATS job `enqueue_job("kobload", "watch_pattern_delete", payload)`

**Payload**:
```json
{
  "source_system": "kobbler",
  "source_id": "<book_id>"
}
```

**Expected**: Kobload deletes the watch pattern, stops monitoring for this book

### 5b. Delete Watch Pattern (on skip/unwant)

**Call Sites**:
- Skip book: `routes/library/wanted.rs:143`
- Unwant book: `routes/library/books.rs:883`

**Method**: Same NATS job as above

---

## 6. SSE Bridge (Real-time Download Status)

**Call Site**: On application startup
- Startup: `main.rs` spawns `kobload_bridge::run_kobload_bridge()`
- Handler: `kobload_bridge.rs:30-82`

**Method**: SSE subscription to `GET {kobload}/api/v1/downloads/stream`

**Events received**:
```json
// Snapshot (on connect)
{ "type": "snapshot", "downloads": [...] }

// Created (new download)
{ "type": "created", "download": {...} }

// Updated (status change)
{ "type": "updated", "download": {...} }
```

**Expected**:
1. Maintain persistent SSE connection
2. Broadcast events to WebSocket clients via `LibraryEvent::kobload_download_*`
3. Reconnect on failure with 5-second backoff

---

## 7. Download File Retrieval

**Call Site**: During ingest webhook processing (section 3)
- Handler: `ingest_single_book()` in `routes/ingest/kobload.rs:504-997`

**Method**: HTTP GET via reqwest
```rust
GET {kobload}/api/v1/downloads/{download_id}/files/{path}
Authorization: Bearer {api_key}
```

**Expected**: Returns raw file bytes, saved to local filesystem

---

## 8. Confirm Import

**Call Site**: After successful file ingest (section 3)
- Handler: `ingest_single_book()` in `routes/ingest/kobload.rs:947-956`

**Method**: HTTP POST via KobloadClient (inline request)
```rust
POST {kobload}/api/v1/downloads/{download_id}/imported
Authorization: Bearer {api_key}
```

**Expected**: Kobload marks download as imported, may delete files from download client

---

## 9. Test Connection

**Call Site**: Settings > Obtain page
- UI: `web/src/routes/settings/obtain/+page.svelte`
- Route: `POST /api/v1/admin/test-kobload`
- Handler: `test_kobload_connection()` in `routes/wanted/kobload.rs:58-90`

**Method**: HTTP via KobloadClient
```rust
client.health_check().await
// Calls: GET {kobload}/health
```

**Expected**: Returns OK or error message

---

## Configuration

Kobbler requires these runtime config values (stored in `config/kobbler.toml`):

| Key | Description | Example |
|-----|-------------|---------|
| `kobload_base_url` | Kobload API URL | `http://kobload-api:7080` |
| `kobload_api_key` | API key for auth | `<api_key>` |
| `kobload_ingest_url` | Callback URL with token | `http://kobbler-api:8080/api/v1/ingest/<token>` |
| `kobload_watch_directory_id` | Default watch dir for downloads | `<uuid>` |
| `wanted_min_score` | Fuzzy match threshold | `0.65` |

---

## NATS Job Types Summary

| Job Type | Direction | Purpose |
|----------|-----------|---------|
| `want_request` | Kobbler → Kobload | Search + create watch pattern |
| `watch_pattern_delete` | Kobbler → Kobload | Remove watch pattern |
| `watch_pattern_upsert` | Kobbler → Kobload | Create/update watch pattern |
| `events.want.response.kobbler` | Kobload → Kobbler | Want request response |
| `events.download.>` | Kobload → Kobbler | Download status updates |
