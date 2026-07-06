use std::{fmt::Display, fs::FileType, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{Json, Path, Query, State},
    http::{StatusCode, header},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
    routing::{get, post},
};
use libvips::{
    VipsApp, VipsImage,
    ops::{self, ResizeOptions},
};
use mimetype_detector::detect_file;
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};
use walkdir::WalkDir;

use crate::FitOption::Contain;

struct AppState {
    vips: VipsApp,
    tera: Tera,
}

#[derive(Deserialize, Debug)]
enum FitOption {
    #[serde(rename = "contain")]
    Contain,
    /*
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
    */
}

#[derive(Deserialize, Debug)]
struct ResizeParams {
    w: Option<i32>,
    h: Option<i32>,
    fit: Option<FitOption>,
}

#[derive(Deserialize, Debug)]
struct ImageFile {
    path: String,
    title: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);

    let mut tera = Tera::default();
    // tera.add_template_files(vec![("./templates/index.tera", Some("index"))])?;
    tera.load_from_glob("templates/**/*").unwrap();

    for template_name in tera.get_template_names() {
        println!("Loaded templates: {:?}", template_name);
    }
    println!("Vips version: {}", app.version_string()?);

    let image_files = WalkDir::new("./images");
    let mut images: Vec<ImageFile> = vec![];

    for i in image_files {
        let entry = i?;
        let path = entry.path();
        let filename = path
            .file_name()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap();
        if entry.file_type().is_file() && filename.ends_with(".jpg") {
            images.push(ImageFile {
                path: String::from(path.to_str().unwrap()),
                title: Some(filename),
            });
        }
    }
    println!("images: {:?}", images);

    let shared_state = Arc::new(AppState {
        vips: app,
        tera: tera,
    });

    let router = Router::new()
        .route("/", get(render_index))
        .route("/view/{*path}", get(render_view))
        .route("/style.css", get(render_style))
        .route("/images/{*path}", get(render_image))
        .with_state(shared_state);

    println!("Listening on: {}", bind_string);

    let listener = tokio::net::TcpListener::bind(bind_string).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    Ok(())
}

async fn render_style(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/css")
        .body(state.tera.render("style.css", &Context::new()).unwrap())
        .unwrap()
}

async fn render_index(State(state): State<Arc<AppState>>) -> HtmlResponse<String> {
    println!("Rendered index");
    let context = Context::new();
    HtmlResponse(state.tera.render("index.tera", &context).unwrap())
}

async fn render_view(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> HtmlResponse<String> {
    println!("Rendered view/");
    let mut context = Context::new();
    context.insert("image", &path);
    HtmlResponse(state.tera.render("view.tera", &context).unwrap())
}

#[axum::debug_handler]
async fn render_image(
    Path(path): Path<String>,
    Query(resize_params): Query<ResizeParams>,
    State(_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    println!("Loaded image: {}", path);
    println!("Query params: {:?}", resize_params);
    let full_path = std::path::Path::new("./images/").join(path);
    let mime_type = detect_file(&full_path).map_err(|err| {
        println!("error: {}", err);
        StatusCode::NOT_FOUND
    })?;
    println!("Showing mime type: {}", mime_type);
    match resize_params.fit {
        Some(FitOption::Contain) => {
            println!("Fit: contain");
            if let Some(path_str) = full_path.to_str() {
                let thumb = ops::thumbnail(path_str, resize_params.w.unwrap())
                    .map_err(|_| StatusCode::NOT_FOUND)?;
                let buf = ops::jpegsave_buffer(&thumb).map_err(|err| {
                    println!("err: {}", err);
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, mime_type.to_string())
                    .body(Body::from(buf))
                    .unwrap());
            } else {
                ()
            }
        }
        None => (),
    }
    let content = std::fs::read(&full_path).map_err(|err| {
        println!("err: {}", err);
        StatusCode::NOT_FOUND
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_type.to_string())
        .body(Body::from(content))
        .unwrap())
}
