use std::sync::Arc;

use axum::{
    body::Body,
    extract::{multipart::Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
};
use mimetype_detector::detect_file;
use tera::Context;

use crate::app::{AppState, ImageFile, ImageParams, PreviewKey, UploadResponse};
use crate::{IMAGE_DIR, is_supported_image_ext, load_image_info, sanitize_filename, sort_image_list, unique_image_path};

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
    let key = format!("{}/{}", IMAGE_DIR, path);

    // Snapshot the sorted (newest-first) list and find the current image.
    let mut sorted: Vec<Arc<ImageFile>> = {
        let list = state.image_list.read().await;
        list.iter().map(Arc::clone).collect()
    };
    sort_image_list(&mut sorted);
    let Some(current_idx) = sorted.iter().position(|i| i.path == key) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let total = sorted.len();
    let image = Arc::clone(&sorted[current_idx]);

    let prev_img = if current_idx > 0 { sorted.get(current_idx - 1).cloned() } else { None };
    let next_img = sorted.get(current_idx + 1).cloned();

    // Thumbnail strip: 4 before + current + 4 after (fills the image width).
    let start = if current_idx > 4 { current_idx - 4 } else { 0 };
    let end = (current_idx + 5).min(total);
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
        prev_img.as_ref().and_then(|p| p.title.as_deref()).unwrap_or_default(),
    );
    context.insert(
        "next_title",
        next_img.as_ref().and_then(|n| n.title.as_deref()).unwrap_or_default(),
    );
    context.insert(
        "first_title",
        sorted.first().and_then(|f| f.title.as_deref()).unwrap_or_default(),
    );
    context.insert(
        "last_title",
        sorted.last().and_then(|l| l.title.as_deref()).unwrap_or_default(),
    );
    Ok(HtmlResponse(state.tera.render("view.tera", &context).unwrap()))
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
    let full_path = std::path::Path::new(IMAGE_DIR).join(&path);

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

        let cached = state.preview_cache.lock().unwrap().get(&key).map(|b| b.to_vec());
        let bytes = match cached {
            Some(b) => b, // cache hit: zero vips work
            None => {
                let gen_path = full_path.clone();
                let bytes = tokio::task::spawn_blocking(move || {
                    // All libvips work lives inside this single blocking closure
                    // because VipsImage is !Send (dropped before returning).
                    match libvips::ops::thumbnail(
                        gen_path.to_str().unwrap_or(""),
                        width as i32,
                    ) {
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
            .header(header::ETAG, format!("\"{}-{}-{}\"", key.path, width, mtime))
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

/// POST /upload — multipart form upload, protected by a configurable API key.
/// Files are saved into `./images` and indexed immediately (the watcher also
/// fires for them; inserts are path-deduped so this is idempotent).
pub async fn upload_images(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<JsonResponse<UploadResponse>, (StatusCode, String)> {
    let api_key = state.api_key.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "uploads disabled: no API key configured (use --api-key or CRAB_GALLERY_API_KEY)".into(),
    ))?;

    let provided = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
        .unwrap_or("");
    if provided.is_empty() || provided != api_key {
        return Err((StatusCode::UNAUTHORIZED, "invalid or missing API key".into()));
    }

    let mut saved = Vec::new();
    let mut errors = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("multipart error: {}", e)))?
    {
        // Text form fields have no filename; only accept file parts.
        let Some(raw_name) = field.file_name().map(String::from) else { continue };
        let base = sanitize_filename(&raw_name);
        if !is_supported_image_ext(&base) {
            errors.push(format!("\"{}\": unsupported type (jpg/jpeg/png only)", raw_name));
            continue;
        }

        // Buffer the part (local gallery; reject anything over 64MB).
        let target = unique_image_path(&base);
        tokio::fs::create_dir_all(IMAGE_DIR).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("cannot create image dir: {}", e))
        })?;
        let bytes = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("read error: {}", e)))?;
        if bytes.len() > 64 * 1024 * 1024 {
            errors.push(format!("\"{}\": exceeds 64MB limit", raw_name));
            continue;
        }

        tokio::fs::write(&target, &bytes[..])
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write error: {}", e)))?;

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
                saved.push(file_name_of(&target));
            }
            None => {
                let _ = tokio::fs::remove_file(&target).await;
                errors.push(format!("\"{}\": file could not be decoded as an image", raw_name));
            }
        }
    }

    Ok(JsonResponse(UploadResponse { saved, errors }))
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}
