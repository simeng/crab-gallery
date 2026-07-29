pub mod app;
pub mod controllers;

use std::{collections::HashMap, fs::metadata, path::Path, sync::Arc};

use chrono::{DateTime, Local};
use libvips::VipsApp;
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

/// Spawns a background task that watches ./images for new files and updates the shared state.
pub fn spawn_image_watcher(
    app: Arc<VipsApp>,
    images: Arc<tokio::sync::RwLock<HashMap<String, Arc<ImageFile>>>>,
    image_list: Arc<tokio::sync::RwLock<Vec<Arc<ImageFile>>>>,
) {
    tokio::spawn(async move {
        use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

        let mut watcher = match RecommendedWatcher::new(
            move |event: Result<notify::Event, notify::Error>| {
                let now = std::time::Instant::now();
                if let Ok(e) = event {
                    for path in e.paths {
                        if let Some(path_str) = path.to_str() {
                            let ext = path.extension().map_or(String::new(), |e| {
                                e.to_string_lossy().to_ascii_lowercase()
                            });

                            match e.kind {
                                EventKind::Create(_) => {
                                    if ext == "jpg" || ext == "jpeg" || ext == "png" {
                                        println!("New image detected: {}", path_str);
                                        if let Ok(img) = libvips::VipsImage::new_from_file(path_str) {
                                            let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                                            let image = Arc::new(ImageFile {
                                                path: path_str.to_string(),
                                                title: filename,
                                                width: img.get_width(),
                                                height: img.get_height(),
                                                modified_at: Some(now.into()),
                                            });
                                            let mut images = images.write().await;
                                            images.insert(path_str.to_string(), Arc::clone(&image));
                                            let mut image_list = image_list.write().await;
                                            image_list.push(Arc::clone(&image));
                                        } else {
                                            println!("err reading image: {}", path_str);
                                        }
                                    }
                                }
                                EventKind::Remove(_) => {
                                    println!("Image removed: {}", path_str);
                                    if let Some(img) = images.get(path_str) {
                                        let mut images = images.write().await;
                                        images.remove(path_str);
                                        let mut image_list = image_list.write().await;
                                        image_list.retain(|i| i.path != path_str);
                                    }
                                }
                                EventKind::Modify(_) => {
                                    if let Some(img) = images.get(path_str) {
                                        if let Ok(meta) = metadata(path_str) {
                                            let modified: DateTime<Local> = meta.modified().ok().map(|t| t.into()).unwrap();
                                            if let Ok(img) = libvips::VipsImage::new_from_file(path_str) {
                                                let width = img.get_width();
                                                let height = img.get_height();
                                                let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                                                let updated = ImageFile {
                                                    path: img.path().unwrap_or(path_str).to_string(),
                                                    title: filename,
                                                    width,
                                                    height,
                                                    modified_at: Some(modified),
                                                };
                                                let mut images = images.write().await;
                                                images.insert(path_str.to_string(), Arc::new(updated));
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                println!("Failed to create file watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(Path::new("./images"), RecursiveMode::Recursive) {
            println!("Failed to watch ./images: {}", e);
            return;
        }

        println!("File watcher started for ./images");
    });
}
