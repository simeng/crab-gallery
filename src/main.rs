use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser;
use crab_gallery::app::{AppState, PreviewCache};
use crab_gallery::controllers::{
    render_api, render_image, render_index, render_style, render_view, upload_images,
    MAX_UPLOAD_BODY_BYTES,
};
use crab_gallery::{resolve_dirs, scan_images, spawn_image_watcher};

use axum::{Router, routing::{get, post}};
use libvips::VipsApp;
use tera::{Kwargs, Tera, TeraResult, Value};

/// Compiled-in fallback copies of the templates (used when `templates/`
/// is not present on disk next to the binary).
const EMBEDDED_TEMPLATES: &[(&str, &str)] = &[
    ("index.tera", include_str!("../templates/index.tera")),
    ("view.tera", include_str!("../templates/view.tera")),
    ("style.css", include_str!("../templates/style.css")),
];

/// Load templates: prefer the on-disk `templates/` dir so edits are picked up
/// without recompiling; fall back to the compiled-in copies if it is missing.
fn load_templates() -> Tera {
    let mut tera = Tera::default();
    tera.register_filter("date_format", date_format_filter);
    // Tera returns Ok(empty set) when the glob matches nothing (missing dir),
    // so always check which templates actually got registered.
    if tera.load_from_glob("templates/**/*").is_err() {
        eprintln!("failed to load templates/ from disk");
    }
    // Register any template the disk load didn't provide (missing dir or a
    // partially populated one) from the compiled-in copies.
    for (name, source) in EMBEDDED_TEMPLATES {
        if tera.get_template_names().any(|t| t == *name) {
            continue;
        }
        if *name == EMBEDDED_TEMPLATES[0].0 {
            eprintln!("templates/ not found on disk — using compiled-in templates");
        }
        tera.add_raw_template(name, source).unwrap_or_else(|e| {
            panic!("failed to register embedded template {name}: {e}")
        });
    }
    tera
}

/// Fast local image gallery with live file watching.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Image folder(s) to include in the gallery (can be given multiple times).
    #[arg(long, short = 'd', default_value = "./images", value_name = "DIR")]
    dirs: Vec<PathBuf>,

    /// Folder that POST /upload saves into (defaults to the first --dir).
    #[arg(long, value_name = "DIR")]
    upload_dir: Option<PathBuf>,

    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "0.0.0.0", value_name = "HOST")]
    host: String,

    /// Port to bind the HTTP server to.
    #[arg(long, default_value_t = 8033, value_name = "PORT")]
    port: u16,

    /// API key protecting POST /upload (or set CRAB_GALLERY_API_KEY).
    /// If unset, uploads are disabled.
    #[arg(long, value_name = "KEY")]
    api_key: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let dirs = resolve_dirs(&args.dirs);
    if dirs.is_empty() {
        eprintln!("error: no valid image directories given via --dir");
        std::process::exit(1);
    }
    let upload_dir = match args.upload_dir {
        Some(d) => d,
        None => PathBuf::from(dirs[0].display.clone()),
    };

    // Global libvips init (ops are then usable from any worker thread).
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);
    println!("Vips version: {}", app.version_string()?);

    let tera = load_templates();

    let (images, image_list) = scan_images(&dirs);
    println!(
        "Loaded {} images from {}",
        image_list.len(),
        dirs.iter().map(|d| d.display.as_str()).collect::<Vec<_>>().join(", ")
    );

    let api_key = args.api_key.or_else(|| std::env::var("CRAB_GALLERY_API_KEY").ok().filter(|k| !k.is_empty()));
    match &api_key {
        Some(_) => println!("Upload API key configured (POST /upload -> {})", upload_dir.display()),
        None => println!("No upload API key set — uploads disabled (use --api-key or CRAB_GALLERY_API_KEY)"),
    }

    let shared_state = Arc::new(AppState {
        tera,
        images: Arc::new(tokio::sync::RwLock::new(images)),
        image_list: Arc::new(tokio::sync::RwLock::new(image_list)),
        preview_cache: Mutex::new(PreviewCache::new()),
        api_key,
        dirs: dirs.iter().map(|d| PathBuf::from(d.display.clone())).collect(),
        upload_dir,
    });

    // File watcher keeps the in-memory index in sync (add/remove/modify).
    let _watcher = spawn_image_watcher(
        tokio::runtime::Handle::current(),
        shared_state.images.clone(),
        shared_state.image_list.clone(),
        dirs,
    );

    let bind_string = format!("{}:{}", args.host, args.port);
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

    let listener = tokio::net::TcpListener::bind(&bind_string).await.unwrap();
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
