use std::{collections::HashMap, fs::metadata, sync::Arc};

use crab_gallery::controllers::{
    render_api, render_image, render_index, render_style, render_view,
};

use crab_gallery::app::{AppState, ImageFile};

use axum::{Router, routing::get};
use chrono::{DateTime, Local};
use libvips::VipsApp;
use tera::{Kwargs, Tera, TeraResult, Value};
use walkdir::{DirEntry, WalkDir};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_string = "0.0.0.0:8033";
    let app = VipsApp::new("crab-gallery", false).expect("Cannot init libvips");
    app.concurrency_set(2);

    let mut tera = Tera::default();
    // tera.add_template_files(vec![("./templates/index.tera", Some("index"))])?;
    tera.register_filter("date_format", date_format_filter);
    tera.load_from_glob("templates/**/*").unwrap();

    for template_name in tera.get_template_names() {
        println!("Loaded templates: {:?}", template_name);
    }
    println!("Vips version: {}", app.version_string()?);

    let image_files = WalkDir::new("./images");
    let mut images: HashMap<String, Arc<ImageFile>> = HashMap::new();
    let mut image_list: Vec<Arc<ImageFile>> = vec![];

    for i in image_files {
        let entry: DirEntry = i?;
        let path = entry.path();
        if entry.file_type().is_file()
            && path.extension().map_or(false, |e| {
                e.to_ascii_lowercase() == "jpg"
                    || e.to_ascii_lowercase() == "jpeg"
                    || e.to_ascii_lowercase() == "png"
            })
        {
            if let Some(path_str) = path.to_str() {
                match libvips::VipsImage::new_from_file(path.to_str().unwrap()) {
                    Ok(i) => {
                        let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                        let meta = metadata(path_str)?;
                        let modified_at: Option<DateTime<Local>> =
                            meta.modified().ok().map(|t| t.into());

                        image_list.push(Arc::new(ImageFile {
                            path: path_str.to_string(),
                            title: filename,
                            width: i.get_width(),
                            height: i.get_height(),
                            modified_at,
                        }));
                    }
                    Err(e) => println!("err: {}", e),
                }
            }
        }
    }
    for image in &image_list {
        images.insert(image.path.clone(), Arc::clone(image));
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

pub fn date_format_filter(value: &Value, args: Kwargs, _: &tera::State) -> TeraResult<String> {
    // 1. Extract the incoming value (accepts string or numeric timestamp)
    let date_str = match value.as_str() {
        Some(s) => s,
        None => {
            return Err(tera::Error::message(
                "Filter `date_format` expected a string value",
            ));
        }
    };

    // 2. Parse the string into a DateTime object
    let date = DateTime::parse_from_rfc3339(date_str)
        .or_else(|_| DateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S %z"))
        .or_else(|_| DateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S")) // Fallback local representation
        .map_err(|e| tera::Error::message(format!("Failed to parse date '{}': {}", date_str, e)))?;

    // 3. Extract the `format` argument from the filter
    let format_str = args.get::<&str>("format")?.unwrap();

    // 4. Format and return
    let formatted = date.format(&format_str).to_string();
    Ok(formatted)
}
