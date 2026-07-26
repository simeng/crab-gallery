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

use crate::app::{AppState, FitOption, ImageFile, ResizeParams, ViewParams};

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
    Query(_view_params): Query<ViewParams>,
    State(state): State<Arc<AppState>>,
) -> HtmlResponse<String> {
    println!("Rendered view/");
    println!("List: {:?}", state.image_list);
    let mut context = Context::new();
    let key = format!("./images/{}", path);
    let mut current_index: Option<usize> = None;
    let mut sorted_list: Vec<Arc<ImageFile>> = state.image_list.clone();
    sorted_list.sort_by(|a, b| {
        let a_time = a.modified_at.unwrap_or(chrono::Local::now());
        let b_time = b.modified_at.unwrap_or(chrono::Local::now());
        b_time.cmp(&a_time)
    });
    if let Some(image) = state.images.get(&key) {
        context.insert("image", &**image);
        for (i, img) in sorted_list.iter().enumerate() {
            let img_title = img.title.clone().unwrap_or_default();
            let img_key = format!("./images/{}", img_title);
            if img_key == key {
                current_index = Some(i);
                break;
            }
        }
    }

    // Handle navigation (next/prev buttons)
    let current_idx = current_index.unwrap_or(0);

    let thumbnails: Vec<ImageFile> = {
        // Always show 5 images centered around current_idx: 2 before + current + 2 after
        // At boundaries, expand to show as many as possible (always including current image)
        let start = if current_idx > 1 { current_idx - 2 } else { 0 };
        let end = if current_idx < 2 { 5 } else { (current_idx + 3).min(sorted_list.len()) };
        sorted_list
            .get(start..end)
            .into_iter()
            .flatten()
            .map(|t| (**t).clone())
            .collect()
    };

    let sorted_list_images: Vec<ImageFile> = sorted_list.iter().map(|t| (**t).clone()).collect();
    context.insert("thumbnails", &thumbnails);
    context.insert("sorted_list", &sorted_list_images);
    if let Some(last_img) = sorted_list.last() {
        context.insert("last_img", &(**last_img).clone());
    }
    
    // Calculate which thumbnail index should be active (center of the 5 thumbnails)
    // Thumbnails start at `start` index in sorted_list, so current image is at:
    // active_thumb_idx = (current_idx - start + 1) for 1-indexed loop
    let start = if current_idx > 1 { current_idx - 2 } else { 0 };
    let active_thumb_idx = (current_idx - start + 1) as i32;
    
    // Calculate previous and next image titles for navigation buttons
    let prev_idx = if current_idx > 0 { current_idx - 1 } else { 0 };
    let next_idx_val = if current_idx + 1 < sorted_list.len() { current_idx + 1 } else { 0 };
    
    let prev_img = sorted_list.get(prev_idx).map(|i| (**i).clone());
    let next_img = sorted_list.get(next_idx_val).map(|i| (*i).clone());
    
    context.insert("current_index", &(current_idx as i32));
    if let Some(prev_img) = prev_img {
        let prev_title = prev_img.title.clone().unwrap_or_default();
        context.insert("prev_img", &prev_title);
    }
    if let Some(next_img) = next_img {
        let next_title = next_img.title.clone().unwrap_or_default();
        context.insert("next_img", &next_title);
    }
    context.insert("active_thumb_idx", &active_thumb_idx as &i32);
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
