use std::sync::Arc;

use axum::{
    Router,
    extract::{Json, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use libvips::VipsApp;
use serde::{Deserialize, Serialize};

struct AppState {
    vips: VipsApp,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);

    println!("Vips version: {}", app.version_string()?);

    let shared_state = Arc::new(AppState { vips: app });

    let router = Router::new()
        .route("/", get(render_root))
        .with_state(shared_state);

    println!("Listening on: {}", bind_string);

    let listener = tokio::net::TcpListener::bind(bind_string).await.unwrap();
    axum::serve(listener, router).await.unwrap();

    Ok(())
}

async fn render_root(State(state): State<Arc<AppState>>) -> &'static str {
    "Crab gallery v0.001"
}
