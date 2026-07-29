use std::sync::Arc;

use crab_gallery::controllers::{
    render_api, render_image, render_index, render_style, render_view,
};

use crab_gallery::app::AppState;

use axum::{Router, routing::get};
use libvips::VipsApp;
use tera::{Kwargs, Tera, TeraResult, Value};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);

    let mut tera = Tera::default();
    tera.register_filter("date_format", date_format_filter);
    tera.load_from_glob("templates/**/*").unwrap();

    for template_name in tera.get_template_names() {
        println!("Loaded templates: {:?}", template_name);
    }
    println!("Vips version: {}", app.version_string()?);

    let (images, image_list) = crab_gallery::scan_images(&app);
    println!("Loaded {} images", image_list.len());

    let app_arc = Arc::new(app);

    let shared_state = Arc::new(AppState {
        vips: app_arc.clone(),
        tera: tera,
        images: Arc::new(tokio::sync::RwLock::new(images)),
        image_list: Arc::new(tokio::sync::RwLock::new(image_list)),
    });

    crab_gallery::spawn_image_watcher(
        app_arc,
        shared_state.images.clone(),
        shared_state.image_list.clone(),
    );

    let router = Router::new()
        .route("/", get(render_index))
        .route("/view/{*path}", get(render_view))
        .route("/style.css", get(render_style))
        .route("/images/{*path}", get(render_image))
        .route("/api/images", get(render_api))
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
