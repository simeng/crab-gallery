use std::{collections::HashMap, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
};
use base64::Engine;

/// Max total request body for `POST /upload` (raw bytes). Larger than the
/// 64MB per-image cap to allow for base64 inflation (~4/3) in the
/// form-urlencoded format.
pub const MAX_UPLOAD_BODY_BYTES: usize = 128 * 1024 * 1024;
use mimetype_detector::detect_file;
use tera::Context;

use crate::app::{AppState, ImageFile, ImageParams, PreviewKey, UploadResponse};
use crate::{
    is_supported_image_ext, load_image_info, sanitize_filename, sort_image_list, unique_image_path,
};

const MAX_PREVIEW_WIDTH: u32 = 4096;
const MIN_PREVIEW_WIDTH: u32 = 8;
/// Default width for the main "featured" image in the viewer.
pub const MAIN_IMAGE_WIDTH: u32 = 1920;

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve a URL filename (image title) to an indexed image. URLs use the
/// bare filename, so with multiple `--dir`s the first match wins.
async fn find_by_title(state: &AppState, title: &str) -> Option<Arc<ImageFile>> {
    let list = state.image_list.read().await;
    list.iter()
        .find(|i| i.title.as_deref() == Some(title))
        .cloned()
}

pub async fn render_api(State(state): State<Arc<AppState>>) -> JsonResponse<Vec<ImageFile>> {
    let image_list = state.image_list.read().await;
    JsonResponse(image_list.iter().map(|t| (**t).clone()).collect())
}

pub async fn render_style(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/css")
        .body(state.tera.render("style.css", &Context::new()).unwrap())
        .unwrap()
}

/// Gallery index: all images (capped), newest first.
pub async fn render_index(State(state): State<Arc<AppState>>) -> HtmlResponse<String> {
    // image_list is kept sorted newest-first by the watcher/upload paths.
    let thumbnails: Vec<ImageFile> = {
        let list = state.image_list.read().await;
        list.iter().take(500).map(|t| (**t).clone()).collect()
    };
    let mut context = Context::new();
    context.insert("latest", &thumbnails);
    context.insert("count", &thumbnails.len());
    HtmlResponse(state.tera.render("index.tera", &context).unwrap())
}

/// Viewer page for one image with prev/next navigation and a thumbnail strip.
pub async fn render_view(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<HtmlResponse<String>, StatusCode> {
    let Some(image) = find_by_title(&state, &path).await else {
        return Err(StatusCode::NOT_FOUND);
    };

    // Snapshot the sorted (newest-first) list and locate the current image.
    let mut sorted: Vec<Arc<ImageFile>> = {
        let list = state.image_list.read().await;
        list.iter().map(Arc::clone).collect()
    };
    sort_image_list(&mut sorted);
    let Some(current_idx) = sorted.iter().position(|i| i.path == image.path) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let total = sorted.len();

    let prev_img = if current_idx > 0 {
        sorted.get(current_idx - 1).cloned()
    } else {
        None
    };
    let next_img = sorted.get(current_idx + 1).cloned();

    // Thumbnail strip: a window of 9 centered on the current image
    // (4 before + current + 4 after). At the list boundaries the window
    // expands in the other direction so it is still 9 wide when possible.
    const THUMB_WINDOW: usize = 9;
    let half = THUMB_WINDOW / 2;
    let mut start = current_idx.saturating_sub(half);
    let end = (start + THUMB_WINDOW).min(total);
    if end - start < THUMB_WINDOW {
        // Near the end of the list: shift the window left to fill it.
        start = end.saturating_sub(THUMB_WINDOW);
    }
    let thumbnails: Vec<ImageFile> = sorted[start..end].iter().map(|t| (**t).clone()).collect();
    let active_thumb_idx = (current_idx - start + 1) as i32; // 1-indexed for tera loop

    // Always provide every key (empty string when absent) — Tera is strict
    // about undefined variables in templates.
    let mut context = Context::new();
    context.insert("image", image.as_ref());
    context.insert("thumbnails", &thumbnails);
    context.insert("active_thumb_idx", &active_thumb_idx);
    context.insert("current_index", &(current_idx + 1)); // 1-indexed for display
    context.insert("total", &total);
    context.insert("main_w", &MAIN_IMAGE_WIDTH);
    context.insert(
        "prev_title",
        prev_img
            .as_ref()
            .and_then(|p| p.title.as_deref())
            .unwrap_or_default(),
    );
    context.insert(
        "next_title",
        next_img
            .as_ref()
            .and_then(|n| n.title.as_deref())
            .unwrap_or_default(),
    );
    context.insert(
        "first_title",
        sorted
            .first()
            .and_then(|f| f.title.as_deref())
            .unwrap_or_default(),
    );
    context.insert(
        "last_title",
        sorted
            .last()
            .and_then(|l| l.title.as_deref())
            .unwrap_or_default(),
    );
    Ok(HtmlResponse(
        state.tera.render("view.tera", &context).unwrap(),
    ))
}

/// Image endpoint.
/// - `?w=N`   -> libvips-scaled JPEG preview (LRU-cached in memory)
/// - no param or `?orig` -> raw source bytes with ETag / 304 support
pub async fn render_image(
    Path(path): Path<String>,
    Query(params): Query<ImageParams>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    let Some(image) = find_by_title(&state, &path).await else {
        return Err(StatusCode::NOT_FOUND);
    };
    let full_path = std::path::PathBuf::from(image.path.clone());

    let meta = tokio::fs::metadata(&full_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mime_type = detect_file(&full_path).map_err(|_| StatusCode::NOT_FOUND)?;
    let mtime = mtime_secs(&meta);

    // --- Preview mode: scaled + cached -------------------------------
    if let Some(w) = params.w {
        if !is_supported_image_ext(full_path.to_str().unwrap_or("")) {
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        let width = w.clamp(MIN_PREVIEW_WIDTH as i32, MAX_PREVIEW_WIDTH as i32) as u32;
        let key = PreviewKey {
            path: full_path.to_string_lossy().into_owned(),
            width,
            mtime_secs: mtime,
        };

        let cached = state
            .preview_cache
            .lock()
            .unwrap()
            .get(&key)
            .map(|b| b.to_vec());
        let bytes = match cached {
            Some(b) => b, // cache hit: zero vips work
            None => {
                let gen_path = full_path.clone();
                let bytes = tokio::task::spawn_blocking(move || {
                    // All libvips work lives inside this single blocking closure
                    // because VipsImage is !Send (dropped before returning).
                    match libvips::ops::thumbnail(gen_path.to_str().unwrap_or(""), width as i32) {
                        Ok(img) => libvips::ops::jpegsave_buffer(&img).map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                })
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .map_err(|e| {
                    eprintln!("preview generation failed for {}: {}", path, e);
                    StatusCode::BAD_GATEWAY
                })?;
                state
                    .preview_cache
                    .lock()
                    .unwrap()
                    .insert(key.clone(), bytes.clone());
                bytes
            }
        };

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "public, max-age=86400")
            .header(
                header::ETAG,
                format!("\"{}-{}-{}\"", key.path, width, mtime),
            )
            .body(Body::from(bytes))
            .unwrap());
    }

    // --- Original mode: raw bytes with ETag ---------------------------
    let etag = format!("\"{}-{}\"", mtime, meta.len());
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .unwrap());
    }

    let content = tokio::fs::read(&full_path).await.map_err(|e| {
        eprintln!("err reading {}: {}", full_path.display(), e);
        StatusCode::NOT_FOUND
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_type.to_string())
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::ETAG, etag)
        .body(Body::from(content))
        .unwrap())
}

/// POST /upload — image upload, protected by a configurable API key.
///
/// Two request formats are supported (chosen by `Content-Type`):
/// - `multipart/form-data`: one or more file parts.
/// - `application/x-www-form-urlencoded`: `file_name` (target filename),
///   `key` (API key, alternative to the `X-Api-Key` / `Authorization`
///   headers) and `image_data` (base64-encoded image bytes).
///
/// Files are saved into `./images` and indexed immediately (the watcher also
/// fires for them; inserts are path-deduped so this is idempotent).
pub async fn upload_images(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Body,
) -> Result<JsonResponse<UploadResponse>, (StatusCode, String)> {
    let bytes = axum::body::to_bytes(body, MAX_UPLOAD_BODY_BYTES)
        .await
        .map_err(|e| {
            let too_large = std::error::Error::source(&e)
                .map(|s| s.is::<http_body_util::LengthLimitError>())
                .unwrap_or(false);
            if too_large {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "request body exceeds the {}MB limit",
                        MAX_UPLOAD_BODY_BYTES / 1024 / 1024
                    ),
                )
            } else {
                (StatusCode::BAD_REQUEST, format!("read error: {}", e))
            }
        })?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.contains("multipart/form-data") {
        upload_multipart(state, content_type, &bytes).await
    } else {
        upload_form(state, &headers, &bytes).await
    }
}

async fn upload_multipart(
    state: Arc<AppState>,
    content_type: &str,
    bytes: &Bytes,
) -> Result<JsonResponse<UploadResponse>, (StatusCode, String)> {
    let boundary = multer::parse_boundary(content_type).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid multipart content-type: {e:?}"),
        )
    })?;
    // The body was already fully collected (and size-capped by the
    // DefaultBodyLimit layer), so feed it to multer as a single-chunk stream.
    let stream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(bytes.clone())]);
    let mut multipart = multer::Multipart::new(stream, boundary);

    let mut saved = Vec::new();
    let mut errors = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("multipart error: {}", e)))?
    {
        // Text form fields have no filename; only accept file parts.
        let Some(raw_name) = field.file_name().map(String::from) else {
            continue;
        };
        let base = sanitize_filename(&raw_name);
        if !is_supported_image_ext(&base) {
            errors.push(format!(
                "\"{}\": unsupported type (jpg/jpeg/png only)",
                raw_name
            ));
            continue;
        }

        // Buffer the part (save_image_bytes enforces the 64MB cap).
        let bytes = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("read error: {}", e)))?;
        match save_image_bytes(&state, &raw_name, bytes.to_vec()).await {
            Ok(name) => saved.push(name),
            Err(msg) => errors.push(msg),
        }
    }

    Ok(JsonResponse(UploadResponse { saved, errors }))
}

async fn upload_form(
    state: Arc<AppState>,
    headers: &HeaderMap,
    bytes: &Bytes,
) -> Result<JsonResponse<UploadResponse>, (StatusCode, String)> {
    let api_key = state.api_key.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "uploads disabled: no API key configured (use --api-key or CRAB_GALLERY_API_KEY)".into(),
    ))?;

    // Percent-decoded key/value pairs; drop empty values (e.g. a trailing
    // "&=") so they can't shadow real fields.
    let form: HashMap<String, String> = form_urlencoded::parse(bytes.as_ref())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(_, v)| !v.is_empty())
        .collect();

    // API key: form field `key` (preferred for this format) or the usual
    // `X-Api-Key` / `Authorization: Bearer` headers.
    let provided = form
        .get("key")
        .map(String::as_str)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .or_else(|| {
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                })
        })
        .unwrap_or("");
    if provided.is_empty() || provided != api_key {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid or missing API key".into(),
        ));
    }

    let image_data = form.get("image_data").ok_or((
        StatusCode::BAD_REQUEST,
        "missing 'image_data' field (expected base64-encoded image bytes)".into(),
    ))?;
    let raw_name = form
        .get("file_name")
        .filter(|s| !s.trim().is_empty())
        .map(String::as_str)
        .unwrap_or("upload.jpg");

    let decoded = match base64::engine::general_purpose::STANDARD.decode(image_data.as_bytes()) {
        Ok(b) => b,
        // Tolerate base64 variants that drop the padding.
        Err(_) => base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(image_data.as_bytes())
            .map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("'image_data' is not valid base64: {}", e),
                )
            })?,
    };

    match save_image_bytes(&state, raw_name, decoded).await {
        Ok(name) => Ok(JsonResponse(UploadResponse {
            saved: vec![name],
            errors: vec![],
        })),
        Err(msg) => {
            let status = if msg.contains("unsupported type") {
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            } else if msg.contains("64MB") {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            Err((status, msg))
        }
    }
}

/// Validate, persist and index one uploaded image; returns the saved filename.
async fn save_image_bytes(
    state: &Arc<AppState>,
    raw_name: &str,
    data: Vec<u8>,
) -> Result<String, String> {
    let base = sanitize_filename(raw_name);
    if !is_supported_image_ext(&base) {
        return Err(format!(
            "\"{}\": unsupported type (jpg/jpeg/png only)",
            raw_name
        ));
    }
    if data.len() > 64 * 1024 * 1024 {
        return Err(format!("\"{}\": exceeds 64MB limit", raw_name));
    }

    let upload_dir = state.upload_dir.to_string_lossy().into_owned();
    let target = unique_image_path(&upload_dir, &base);
    tokio::fs::create_dir_all(&state.upload_dir)
        .await
        .map_err(|e| format!("cannot create upload dir: {}", e))?;
    tokio::fs::write(&target, &data[..])
        .await
        .map_err(|e| format!("write error: {}", e))?;

    let state = Arc::clone(state);

    // Index immediately so the image is available without waiting on the watcher.
    let target_str = target.clone();
    let info = tokio::task::spawn_blocking(move || load_image_info(&target_str))
        .await
        .unwrap_or(None);
    match info {
        Some(image_file) => {
            let mut map = state.images.write().await;
            let mut list = state.image_list.write().await;
            list.retain(|i| i.path != image_file.path); // dedupe watcher race
            map.insert(image_file.path.clone(), Arc::clone(&image_file));
            list.push(image_file);
            sort_image_list(&mut list);
            Ok(file_name_of(&target))
        }
        None => {
            let _ = tokio::fs::remove_file(&target).await;
            Err(format!(
                "\"{}\": file could not be decoded as an image",
                raw_name
            ))
        }
    }
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}
