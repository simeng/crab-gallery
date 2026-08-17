# crab-gallery

A fast, self-contained image gallery. It keeps an **in-memory index** of one
or more image folders (default `./images`), live-updates it with a file
watcher (no polling), and serves a
browser UI where you can quickly browse, jump around, view fullscreen, and
open full-resolution originals.

## Features

- **Instant index** — all images are scanned once at startup; widths, heights
  and mtimes are kept in memory.
- **Live updates** — a `notify` file watcher adds/removes/updates individual
  files without rescanning the folder (and never polls).
- **Auto-scaled, cached previews** — `GET /images/<file>?w=N` returns a
  libvips-generated JPEG scaled to width `N`, held in an in-memory LRU cache
  keyed by `(path, width, mtime)`, so repeated requests are served with zero
  image processing.
- **Scaled main image** — the viewer serves the featured image pre-scaled to
  1920px (also cached) instead of shipping full-resolution originals for
  browsing.
- **Original / fullscreen** — every viewer page has an "Original" button that
  opens the untouched source file in a new tab, and a Fullscreen button (or
  press `F`) using the browser Fullscreen API. Keyboard: `←` / `→` to navigate.
- **ETag / 304** — originals are served with ETags so browsers revalidate
  cheaply; preview URLs are long-cacheable since mtime is part of the cache key.
- **Uploads** — `POST /upload` accepts multipart files, saves them straight
  into `./images` (sanitized names, collision-safe) and indexes them
  immediately, so they show up in the gallery instantly.

## Quick start

```sh
cargo run                                  # browse only (uploads disabled)
cargo run -- --api-key mysecret            # enable uploads
CRAB_GALLERY_API_KEY=mysecret cargo run    # same, via environment variable
```

Then open <http://localhost:8033>.

> Requires libvips to be installed on the system.

## Configuration (CLI flags)

```text
-d, --dirs <DIR>        Image folder(s) to include (repeatable) [default: ./images]
    --upload-dir <DIR>  Folder that POST /upload saves into [default: first --dir]
    --host <HOST>       Address to bind [default: 0.0.0.0]
    --port <PORT>       Port to bind [default: 8033]
    --api-key <KEY>     Upload API key (or CRAB_GALLERY_API_KEY); unset = uploads off
```

Examples:

```sh
# two photo folders on a LAN port, uploads into an inbox folder
crab-gallery -d ./images -d ./vacation --upload-dir ./inbox \
             --host 0.0.0.0 --port 9000 --api-key mysecret
```

With multiple `--dir`s, URLs use the bare filename and the **first** match
wins if two folders contain the same filename.

## Endpoints

| Route             | Method | Description                                                        |
| ----------------- | ------ | ------------------------------------------------------------------ |
| `/`               | GET    | Gallery grid (all images, newest first)                            |
| `/view/{file}`    | GET    | Viewer: scaled main image, prev/next, thumbnails, fullscreen       |
| `/images/{file}`  | GET    | Raw source file (`ETag`, `304` support). `?w=N` → cached JPEG preview at width N, `?orig` → raw source |
| `/api/images`     | GET    | JSON list of all indexed images (path, title, w/h, modified_at)    |
| `/upload`         | POST   | Multipart upload (API key required). See below.                    |
| `/style.css`      | GET    | Stylesheet                                                          |

## Uploading

The upload endpoint requires an API key (configured with `--api-key KEY` or
`CRAB_GALLERY_API_KEY`). If no key is configured, uploads are disabled (503).
Send the key via `X-Api-Key` or `Authorization: Bearer <key>`:

```sh
curl -F "files=@photo1.jpg" -H "X-Api-Key: mysecret" http://localhost:8033/upload
curl -F "files=@a.jpg" -F "files=@b.png" -H "Authorization: Bearer mysecret" \
     http://localhost:8033/upload
```

Only `jpg`/`jpeg`/`png` are accepted (max 64 MB per file). Filenames are
sanitized; name collisions get a timestamp suffix. The response is JSON:

```json
{ "saved": ["photo1.jpg"], "errors": [] }
```

From a phone, any HTTP form that posts `multipart/form-data` with the key in
the headers works (e.g. a simple web form posting to `/upload`).

## Architecture notes

- **Rust**: axum (web) + tera (templates) + libvips (image scaling) +
  notify (file watching) + tokio (async runtime).
- **Templates** are compiled into the binary as a fallback. At startup, if
  `templates/` exists next to the working directory its files are used (live
  editing without recompiling); any template missing on disk is served from
  the embedded copy.
- The in-memory index is `HashMap<path, ImageFile>` + a sorted list behind
  `tokio::sync::RwLock`. Canonical path keys are `./images/<file>`.
- libvips types are **not Send**, so all vips work happens inside synchronous
  code or a single `spawn_blocking` closure that returns owned bytes.
- See `AGENTS.md` for implementation constraints and `PLAN.md` for the
  design/roadmap notes.

## Development

```sh
cargo build
cargo test
cargo watch -x run
```
