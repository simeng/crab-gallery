use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Json, Path, Query, State},
    http::{StatusCode, header},
    response::{Html as HtmlResponse, IntoResponse, Json as JsonResponse, Response},
    routing::{get, post},
};
use libvips::VipsApp;
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

struct AppState {
    vips: VipsApp,
    tera: Tera,
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
        .route("/image/{*path}", get(render_image))
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

async fn render_image(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    println!("Loaded image: {}", path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(""))
        .unwrap()
}
