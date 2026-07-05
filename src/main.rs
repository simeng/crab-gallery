use std::{fmt::Display, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{Json, Path, Query, State},
    http::{StatusCode, header},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
    routing::{get, post},
};
use libvips::VipsApp;
use mimetype_detector::detect_file;
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

struct AppState {
    vips: VipsApp,
    tera: Tera,
}

#[derive(Deserialize, Debug)]
enum FitOption {
    #[serde(rename = "contain")]
    Contain,
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "fill-max")]
    FillMax,
    #[serde(rename = "stretch")]
    Stretch,
    #[serde(rename = "cover")]
    Cover,
    #[serde(rename = "crop")]
    Crop,
}

#[derive(Deserialize, Debug)]
struct ResizeParams {
    w: Option<u32>,
    h: Option<u32>,
    fit: Option<FitOption>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);

    let mut tera = Tera::new();
    tera.add_template_files(vec![("./templates/index.tera", Some("index"))])?;

    println!("Vips version: {}", app.version_string()?);

    let shared_state = Arc::new(AppState {
        vips: app,
        tera: tera,
    });

    let router = Router::new()
        .route("/", get(render_root))
        .route("/images/{*path}", get(render_image))
        .with_state(shared_state);

    println!("Listening on: {}", bind_string);

    let listener = tokio::net::TcpListener::bind(bind_string).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    Ok(())
}

async fn render_root(State(state): State<Arc<AppState>>) -> HtmlResponse<String> {
    println!("Rendered index");
    let context = Context::new();
    HtmlResponse(state.tera.render("index", &context).unwrap())
}

#[axum::debug_handler]
async fn render_image(
    Path(path): Path<String>,
    Query(resize_params): Query<ResizeParams>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    println!("Loaded image: {}", path);
    println!("Query params: {:?}", resize_params);
    let full_path = std::path::Path::new("./images/").join(path);
    let mime_type = detect_file(&full_path).map_err(|err| {
        println!("error: {}", err);
        StatusCode::NOT_FOUND
    })?;
    println!("Showing mime type: {}", mime_type);
    let content = std::fs::read(full_path).map_err(|err| {
        println!("err: {}", err);
        StatusCode::NOT_FOUND
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_type.to_string())
        .body(Body::from(content))
        .unwrap())
}
