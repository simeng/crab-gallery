use std::sync::{Arc, Mutex};

use crab_gallery::app::{AppState, PreviewCache};
use crab_gallery::controllers::{
    render_api, render_image, render_index, render_style, render_view, upload_images,
    MAX_UPLOAD_BODY_BYTES,
};
use crab_gallery::{scan_images, spawn_image_watcher};

use axum::{Router, routing::{get, post}};
use libvips::VipsApp;
use tera::{Kwargs, Tera, TeraResult, Value};

/// Read the upload API key from `--api-key KEY` (first match wins) or the
/// `CRAB_GALLERY_API_KEY` environment variable.
fn resolve_api_key() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--api-key" || arg == "-k" {
            if let Some(key) = args.next() {
                return Some(key);
            }
            return None;
        }
        if let Some(key) = arg.strip_prefix("--api-key=") {
            return Some(key.to_string());
        }
    }
    std::env::var("CRAB_GALLERY_API_KEY").ok().filter(|k| !k.is_empty())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";

    // Global libvips init (ops are then usable from any worker thread).
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);
    println!("Vips version: {}", app.version_string()?);

    let mut tera = Tera::default();
    tera.register_filter("date_format", date_format_filter);
    tera.load_from_glob("templates/**/*").unwrap();

    let (images, image_list) = scan_images();
    println!("Loaded {} images from {}", image_list.len(), crab_gallery::IMAGE_DIR);

    let api_key = resolve_api_key();
    match &api_key {
        Some(_) => println!("Upload API key configured (POST /upload)"),
        None => println!("No upload API key set — uploads disabled (use --api-key or CRAB_GALLERY_API_KEY)"),
    }

    let shared_state = Arc::new(AppState {
        tera,
        images: Arc::new(tokio::sync::RwLock::new(images)),
        image_list: Arc::new(tokio::sync::RwLock::new(image_list)),
        preview_cache: Mutex::new(PreviewCache::new()),
        api_key,
    });

    // File watcher keeps the in-memory index in sync (add/remove/modify).
    let _watcher = spawn_image_watcher(
        tokio::runtime::Handle::current(),
        shared_state.images.clone(),
        shared_state.image_list.clone(),
    );

    let router = Router::new()
        .route("/", get(render_index))
        .route("/view/{*path}", get(render_view))
        .route("/style.css", get(render_style))
        .route("/images/{*path}", get(render_image))
        .route("/api/images", get(render_api))
        .route("/upload", post(upload_images))
        // axum's default request body limit is 2MB — raise it for photo
        // uploads (see MAX_UPLOAD_BODY_BYTES).
        .layer(axum::extract::DefaultBodyLimit::max(MAX_UPLOAD_BODY_BYTES))
        .with_state(shared_state);

    println!("Listening on: {}", bind_string);

    let listener = tokio::net::TcpListener::bind(bind_string).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    Ok(())
}

pub fn date_format_filter(value: &Value, args: Kwargs, _: &tera::State) -> TeraResult<String> {
    let date_str = match value.as_str() {
        Some(s) => s,
        None => {
            return Err(tera::Error::message(
                "Filter `date_format` expected a string value",
            ));
        }
    };

    let date = chrono::DateTime::parse_from_rfc3339(date_str)
        .or_else(|_| chrono::DateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S %z"))
        .or_else(|_| chrono::DateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S"))
        .map_err(|e| tera::Error::message(format!("Failed to parse date '{}': {}", date_str, e)))?;

    let format_str = args.get::<&str>("format")?.unwrap();

    let formatted = date.format(&format_str).to_string();
    Ok(formatted)
}
