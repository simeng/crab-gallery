use std::{collections::HashMap, fmt::Display, fs::FileType, sync::Arc};

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
use walkdir::{DirEntry, WalkDir};

use crate::FitOption::Contain;

struct AppState {
    vips: VipsApp,
    tera: Tera,
    images: HashMap<String, ImageFile>,
    image_list: Vec<String>,
}

#[derive(Deserialize, Debug, Serialize)]
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

#[derive(Deserialize, Debug, Serialize)]
struct ResizeParams {
    w: Option<i32>,
    h: Option<i32>,
    fit: Option<FitOption>,
}

#[derive(Deserialize, Debug, Serialize, Clone)]
struct ImageFile {
    path: String,
    title: Option<String>,
    width: i32,
    height: i32,
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
    let mut images: HashMap<String, ImageFile> = HashMap::new();
    let mut image_list: Vec<String> = vec![];

    for i in image_files {
        let entry: DirEntry = i?;
        let path = entry.path();
        if entry.file_type().is_file()
            && path.extension().map_or(false, |e| e == "jpg" || e == "JPG")
        {
            if let Some(path_str) = path.to_str() {
                match libvips::VipsImage::new_from_file(path.to_str().unwrap()) {
                    Ok(i) => {
                        let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                        let key = path_str.to_string();
                        image_list.push(key);
                        let key = path_str.to_string();
                        images.insert(
                            key,
                            ImageFile {
                                path: path_str.to_string(),
                                title: filename,
                                width: i.get_width(),
                                height: i.get_height(),
                            },
                        );
                    }
                    Err(e) => println!("err: {}", e),
                }
            }
        }
    }
    println!("images: {:?}", images);

    let shared_state = Arc::new(AppState {
        vips: app,
        tera: tera,
        images: images,
        image_list: image_list,
    });

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

async fn render_api(State(state): State<Arc<AppState>>) -> JsonResponse<Vec<ImageFile>> {
    let thumbnails = state
        .image_list
        .get(0..5)
        .into_iter()
        .flatten()
        .filter_map(|s| state.images.get(s))
        .cloned()
        .collect();

    JsonResponse(thumbnails)
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
    println!("List: {:?}", state.image_list);
    let mut context = Context::new();
    let key = format!("./images/{}", path);
    if let Some(image) = state.images.get(&key) {
        context.insert("image", &image);
    }

    let thumbnails: Vec<ImageFile> = state
        .image_list
        .get(0..5)
        .into_iter()
        .flatten()
        .filter_map(|e| state.images.get(e))
        .cloned()
        .collect();
    context.insert("thumbnails", &thumbnails);

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
