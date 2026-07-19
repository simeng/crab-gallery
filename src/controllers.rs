use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
};
use libvips::ops::{self};
use mimetype_detector::detect_file;
use tera::Context;

use crate::app::{AppState, FitOption, ImageFile, ResizeParams};

pub async fn render_api(State(state): State<Arc<AppState>>) -> JsonResponse<Vec<ImageFile>> {
    let thumbnails = state
        .image_list
        .get(0..5)
        .into_iter()
        .flatten()
        .map(|t| (**t).clone())
        .collect();

    JsonResponse(thumbnails)
}

pub async fn render_style(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/css")
        .body(state.tera.render("style.css", &Context::new()).unwrap())
        .unwrap()
}

pub async fn render_index(State(state): State<Arc<AppState>>) -> HtmlResponse<String> {
    println!("Rendered index");
    let mut context = Context::new();
    let mut thumbnails: Vec<Arc<ImageFile>> = state.image_list.clone();
    thumbnails.sort_by_key(|a| std::cmp::Reverse(a.modified_at));

    let thumbnails: Vec<ImageFile> = thumbnails.iter().take(100).map(|t| (**t).clone()).collect();
    context.insert("latest", &thumbnails);
    HtmlResponse(state.tera.render("index.tera", &context).unwrap())
}

pub async fn render_view(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> HtmlResponse<String> {
    println!("Rendered view/");
    println!("List: {:?}", state.image_list);
    let mut context = Context::new();
    let key = format!("./images/{}", path);
    if let Some(image) = state.images.get(&key) {
        context.insert("image", &**image);
    }

    let thumbnails: Vec<ImageFile> = state
        .image_list
        .get(0..5)
        .into_iter()
        .flatten()
        .map(|t| (**t).clone())
        .collect();
    context.insert("thumbnails", &thumbnails);

    HtmlResponse(state.tera.render("view.tera", &context).unwrap())
}

#[axum::debug_handler]
pub async fn render_image(
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
