pub mod app;
pub mod controllers;

use std::{collections::HashMap, fs::metadata, path::{PathBuf, Path}, sync::Arc};

use tokio::sync::RwLock as TokioRwLock;

use chrono::{DateTime, Local};
use libvips::VipsApp;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::{DirEntry, WalkDir};

use crate::app::ImageFile;

/// Scans the images directory and returns a HashMap and Vec of ImageFile entries.
pub fn scan_images(_app: &VipsApp) -> (HashMap<String, Arc<ImageFile>>, Vec<Arc<ImageFile>>) {
    let mut images: HashMap<String, Arc<ImageFile>> = HashMap::new();
    let mut image_list: Vec<Arc<ImageFile>> = vec![];

    for i in WalkDir::new("./images") {
        let entry: DirEntry = match i {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if entry.file_type().is_file()
            && path.extension().map_or(false, |e| {
                e.to_ascii_lowercase() == "jpg"
                    || e.to_ascii_lowercase() == "jpeg"
                    || e.to_ascii_lowercase() == "png"
            })
        {
            if let Some(path_str) = path.to_str() {
                if let Ok(img) = libvips::VipsImage::new_from_file(path_str) {
                    let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                    if let Ok(meta) = metadata(path_str) {
                        let modified_at: Option<DateTime<Local>> =
                            meta.modified().ok().map(|t| t.into());

                        image_list.push(Arc::new(ImageFile {
                            path: path_str.to_string(),
                            title: filename,
                            width: img.get_width(),
                            height: img.get_height(),
                            modified_at,
                        }));
                    }
                } else {
                    println!("err reading image: {}", path_str);
                }
            }
        }
    }

    for image in &image_list {
        images.insert(image.path.clone(), Arc::clone(image));
    }

    (images, image_list)
}

/// Spawn an image watcher that monitors the images directory for changes.
pub fn spawn_image_watcher(
    _app: Arc<VipsApp>,
    _images: Arc<TokioRwLock<HashMap<String, Arc<ImageFile>>>>,
    _image_list: Arc<TokioRwLock<Vec<Arc<ImageFile>>>>,
) -> (notify::RecommendedWatcher,) {
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| -> () {
            // Notify requires synchronous callback (FnMut), not async
            // Spawn async tasks here for state updates
            if let Ok(event) = result {
                if matches!(event.kind, EventKind::Remove(_) | EventKind::Modify(_)) {
                    if let Some(path_str) = event.paths.first().map(|p| p.to_string_lossy().to_string()) {
                        tokio::task::spawn(async move {
                            // Only handle image files
                            let ext = Path::new(&path_str).extension()
                                .and_then(|e| e.to_str())
                                .map(|s| s.to_lowercase());
                            
                            if ext.as_deref() == Some("jpg") 
                                || ext.as_deref() == Some("jpeg") 
                                || ext.as_deref() == Some("png") {
                                
                                let path_buf = PathBuf::from(&path_str);
                                let filename = path_buf.file_name().map(|e| e.to_string_lossy().into_owned());
                                
                                // Spawn blocking task to load image (VipsImage is not Send)
                                tokio::task::spawn_blocking(move || {
                                    libvips::VipsImage::new_from_file(&path_str).ok()
                                        .map(|img| ImageFile {
                                            path: path_str.clone(),
                                            title: filename,
                                            width: img.get_width(),
                                            height: img.get_height(),
                                            modified_at: None,
                                        })
                                }).await.ok();
                            }
                        });
                    }
                }
            }
        },
        Config::default(),
    ).expect("Error creating file watcher");

    // Watch all nested directories in ./images
    for i in WalkDir::new("./images") {
        let entry: DirEntry = match i {
            Ok(e) => e,
            Err(_) => continue,
        };
        
        if let Some(path_str) = entry.path().to_str() {
            watcher
                .watch(
                    &PathBuf::from(path_str),
                    RecursiveMode::Recursive,
                )
                .ok();
        }
    }

    // Return the watcher to main.rs
    (watcher,)
}
