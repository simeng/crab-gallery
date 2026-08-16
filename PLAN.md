# crab-gallery — Improvement Plan

Goal: a **fast** local image gallery that keeps an in-memory index of `./images`,
live-updates it via a file watcher (no polling), lets the user browse quickly
in the browser with auto-scaled, server-cached previews, a scaled main image,
and one-click access to the full-resolution original in a fullscreen view.
Images can also be **uploaded** (e.g. from a phone) via an API-key-protected
endpoint; saved files land directly in `./images` and become available
immediately.

## Current state

- Axum + Tera + libvips + notify. In-memory `HashMap<path, ImageFile>` + sorted list.
- File watcher (notify RecommendedWatcher) handles create/remove/modify — keep this pattern.
- Problems:
  - Every preview request re-reads the source file and re-runs libvips — no caching.
  - Main image is always served as the full-resolution original (slow on big photos).
  - `h` / `fit=cover` etc. are declared in `ResizeParams` but only `w` + `contain` ever work; thumbnails are cropped by CSS.
  - No HTTP caching (ETag/304), no keyboard navigation, no fullscreen, no way to view the original at full res.
  - Nav buttons wrap around confusingly (Prev on first image → index 0).

## Backend changes

### `src/app.rs`
- Add `ThumbCache`: LRU cache (capacity ~1024 entries) guarded by `std::sync::Mutex`,
  keyed by `(path, width, file_mtime_secs)` → pre-encoded JPEG bytes.
  mtime in the key means modified files invalidate automatically.
- Replace `ResizeParams`/`FitOption` with a simple `ImageParams { w: Option<i32>, orig: Option<bool> }`.
- Drop the unused `VipsApp` field from `AppState` (global vips init happens once in `main`).

### `src/controllers.rs`
- **`GET /images/{path}`** — single endpoint, two modes:
  - `?w=N` (preview): return a libvips-generated JPEG scaled to width N (clamped 8–4096).
    Served from the LRU cache; on miss, generate inside `tokio::task::spawn_blocking`
    (libvips is not Send-safe across async tasks, so all vips work happens in one
    blocking closure that returns owned bytes). Response: `image/jpeg`, ETag, `Cache-Control`.
  - no params or `?orig`: raw source bytes with the detected MIME type.
    ETag = `"mtime-size"`, honors `If-None-Match` → **304**, `Cache-Control: public`.
  - Validate extension (jpg/jpeg/png) and that the file exists → 404 otherwise.
- **`GET /`** — index page, all images (capped at 500) sorted by mtime desc, cached thumbnails `?w=320`.
- **`GET /view/{path}`** — viewer page:
  - main image served scaled (`?w=1920`, auto-scaled down; CSS `object-fit: contain`) with width/height metadata for layout stability (aspect-ratio).
  - real prev/next as `Option` (no wrap-around), position `i / n`.
  - thumbnail window of 7 centered on current image, active one centered.
  - emits `data-*` attributes + small JS block for keyboard nav, fullscreen, prefetching next/prev main images.
- **`GET /api/images`** — returns the full in-memory list (was: first 5).

### Upload: `POST /upload`
- Multipart form (`axum/multipart` feature); accepts one or more file parts.
- API key required, configurable via `--api-key KEY` CLI flag or
  `CRAB_GALLERY_API_KEY` env var; accepted in `X-Api-Key` header or
  `Authorization: Bearer <key>`. If no key is configured → uploads disabled (503).
  Wrong/missing key → 401.
- Each file part: sanitized filename (basename only, safe chars, spaces→`_`),
  extension must be jpg/jpeg/png (else that part is rejected with an error list),
  name collisions resolved by inserting a timestamp suffix. File written to
  `./images/`, then indexed immediately in memory (dims via vips in a blocking
  task, path-deduped insert) so it shows up without waiting for watcher events;
  watcher Create events stay idempotent (dedupe by path).
- Responds JSON with the list of saved filenames.

### `src/lib.rs` (watcher)
- Keep `notify::RecommendedWatcher`, recursive watch on `./images` — **do not poll**.
- Create: load dims synchronously in the watcher callback (vips !Send), insert + sort list in a spawned async task under the lock.
- Remove: delete by title match.
- Modify: refresh mtime + dims when they change.
- Drop unused `VipsApp` parameters from `scan_images` / `spawn_image_watcher`.

## Frontend changes (`templates/`)

### `index.tera`
- Responsive CSS grid (`repeat(auto-fill, minmax(180px, 1fr))`), lazy-loaded cached thumbnails, date label per image, image count in header.

### `view.tera`
- Main image auto-scales to fit (max ~75vh, `object-fit: contain`).
- Toolbar: First / Prev / Next / Last (disabled at boundaries) + **Fullscreen** button + **Original** link (opens raw source in new tab → browser's native fullscreen-ish viewer).
- Keyboard: ←/→ navigate, `F` toggles fullscreen (Fullscreen API on the featured image wrapper; Esc exits natively), `Esc` handled by browser.
- Prefetch next/prev main images + their thumbnails with `Image()` / `<link rel=preload>` so navigation feels instant.
- Active thumbnail auto-scrolled into view.

### `style.css`
- Grid layout, dark theme polish, fullscreen styling (black bg, image centered contain), disabled button states.

## Cargo
- Add `multipart` to the axum feature list.
- `README.md`: describe features, endpoints, how to run.
- `AGENTS.md`: keep the "no polling" rule; document the preview cache contract and
  the vips-in-spawn_blocking pattern for future changes.

## Verification
1. `cargo build` clean.
2. Run server; curl `/`, `/view/{img}`, `/images/{img}` (raw + ETag), `/images/{img}?w=160` (JPEG, 2nd request served from cache — no "generated" log line).
3. Copy a file into `./images/`, remove one → watcher logs add/remove and index reflects it.
4. `curl -F "files=@img.jpg" -H "X-Api-Key: …" localhost:8033/upload` → 200, image appears in gallery; wrong key → 401.
4. Browser: grid loads fast, arrow keys navigate, F fullscreen works, Original opens full-res.
