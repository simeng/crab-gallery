pub mod app;
pub mod controllers;
use crate::app::ImageFile;

use std::collections::HashMap;
use std::fs::metadata;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock as TokioRwlock;

use chrono::{DateTime, Local};
use libvips::VipsApp;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::{DirEntry, WalkDir};

/// Synchronous scan for initial load (called from main before tokio runtime is active)
pub fn scan_images(_app: &VipsApp) -> (HashMap<String, Arc<ImageFile>>, Vec<Arc<ImageFile>>) {
    let mut images: HashMap<String, Arc<ImageFile>> = HashMap::new();
    let mut image_list: Vec<Arc<ImageFile>> = vec![];

    for i in WalkDir::new("./images") {
        let entry: DirEntry = match i { Ok(e) => e, Err(_) => continue };
        let path = entry.path();
        if entry.file_type().is_file() && path.extension().map_or(false, |e| {
            e.to_ascii_lowercase() == "jpg" || e.to_ascii_lowercase() == "jpeg" || e.to_ascii_lowercase() == "png"
        }) {
            if let Some(path_str) = path.to_str() {
                if let Ok(img) = libvips::VipsImage::new_from_file(path_str) {
                    let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                    if let Ok(meta) = metadata(path_str) {
                        let modified_at: Option<DateTime<Local>> = meta.modified().ok().map(Into::into);
                        image_list.push(Arc::new(ImageFile { path: path_str.to_string(), title: filename, width: img.get_width(), height: img.get_height(), modified_at }));
                    }
                } else { println!("err reading image: {}", path_str); }
            }
        }
    }

    for image in &image_list { images.insert(image.path.clone(), Arc::clone(image)); }
    (images, image_list)
}

/// Spawn a file watcher that handles add/remove/modify events without rescanning.
pub fn spawn_image_watcher(
    _app: Arc<VipsApp>,
    handle: Handle,
    images: Arc<TokioRwlock<HashMap<String, Arc<ImageFile>>>>,
    image_list: Arc<TokioRwlock<Vec<Arc<ImageFile>>>>,
) -> RecommendedWatcher {
    let mut watcher = notify::recommended_watcher(
        move |res| {
            let handle = handle.clone();
            let images = Arc::clone(&images);
            let image_list = Arc::clone(&image_list);

            let evt: notify::Event = match res {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("File watcher error: {}", e);
                    return;
                }
            };

            match &evt.kind {
                EventKind::Create(_) => {
                    for path in &evt.paths {
                        let is_image = path.extension().map_or(false, |e| {
                            let lower = e.to_ascii_lowercase();
                            lower == "jpg" || lower == "jpeg" || lower == "png"
                        });
                        if !is_image {
                            continue;
                        }
                        // Load image synchronously (VipsImage is not Send-safe)
                        // Retry loading in case libvips has a stale file cache from the previous load
                        let path_str_raw = match path.to_str() {
                            Some(s) => s,
                            None => {
                                eprintln!("Invalid UTF-8 in path: {}", path.display());
                                continue;
                            }
                        };
                        let mut img = None;
                        for _retry in 0..5 {
                            img = libvips::VipsImage::new_from_file(path_str_raw).ok();
                            if img.is_some() {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        let img = match img {
                            Some(img) => img,
                            None => {
                                eprintln!("Failed to load new image after retries: {}", path.display());
                                continue;
                            }
                        };
                        let filename = path.file_name().map(|e| e.to_string_lossy().into_owned());
                        let modified_at = path.metadata().ok().and_then(|m| {
                            m.modified().ok().map(Into::<DateTime<Local>>::into)
                        });
                        let path_str = match path.to_str() {
                            Some(s) => s.to_string(),
                            None => continue,
                        };
                        let image_file = Arc::new(ImageFile {
                            path: path_str.clone(),
                            title: filename,
                            width: img.get_width(),
                            height: img.get_height(),
                            modified_at,
                        });
                        let images2 = Arc::clone(&images);
                        let image_list2 = Arc::clone(&image_list);
                        let title = image_file.title.clone().unwrap_or_default();
                        let w = image_file.width;
                        let h = image_file.height;
                        let path_str2 = path_str.clone();
                        let image_file2 = Arc::clone(&image_file);
                        drop(img);
                        handle.spawn(async move {
                            let mut map = images2.write().await;
                            let mut list = image_list2.write().await;
                            map.insert(path_str2.clone(), Arc::clone(&image_file2));
                            list.push(image_file2);
                            list.sort_by(|a, b| {
                                let ta = a.modified_at.map(|d| d.naive_utc());
                                let tb = b.modified_at.map(|d| d.naive_utc());
                                tb.cmp(&ta)
                            });
                            eprintln!("Added image: {} ({}, {}x{})", title, path_str2, w, h);
                        });
                    }
                }
                EventKind::Remove(_) => {
                    for path in &evt.paths {
                        let title = path.file_name().map(|e| e.to_string_lossy().into_owned());
                        let image_path = path.to_str().map(|s| s.to_string());
                        let images2 = Arc::clone(&images);
                        let image_list2 = Arc::clone(&image_list);
                        let title2 = title.clone();
                        handle.spawn(async move {
                            let mut map = images2.write().await;
                            let mut list = image_list2.write().await;
                            if let Some(p) = &image_path {
                                map.remove(p);
                                list.retain(|img| img.path != *p);
                                eprintln!("Removed image: {}", title2.unwrap_or_default());
                            }
                        });
                    }
                }
                EventKind::Modify(_) => {
                    // Extract width/height synchronously since VipsImage is not Send
                    let (new_width, new_height) = match &evt.paths.first() {
                        Some(p) => {
                            let is_image = p.extension().map_or(false, |e| {
                                let lower = e.to_ascii_lowercase();
                                lower == "jpg" || lower == "jpeg" || lower == "png"
                            });
                            if !is_image {
                                return;
                            } else if let Ok(new_img) = libvips::VipsImage::new_from_file(
                                p.to_str().unwrap_or(""),
                            ) {
                                (new_img.get_width(), new_img.get_height())
                            } else {
                                return;
                            }
                        }
                        None => return,
                    };


                    let image_path = match &evt.paths.first() {
                        Some(p) => p.to_str().map(|s| s.to_string()),
                        None => None,
                    };

                    if let Some(path_str) = image_path {
                        let images2 = Arc::clone(&images);
                        let modified_at = std::fs::metadata(&path_str)
                            .ok()
                            .and_then(|m| m.modified().ok().map(Into::<DateTime<Local>>::into));
                        handle.spawn(async move {
                            let map = images2.read().await;
                            if let Some(img) = map.get(&path_str) {
                                if new_width != img.width || new_height != img.height {
                                    let filename = img.title.clone();
                                    let new_image = Arc::new(ImageFile {
                                        path: path_str.clone(),
                                        title: filename,
                                        width: new_width,
                                        height: new_height,
                                        modified_at,
                                    });
                                    let mut wmap = images2.write().await;
                                    wmap.insert(path_str.clone(), Arc::clone(&new_image));
                                    eprintln!("Dimensions changed for: {}", path_str);
                                }
                            }
                        });
                    }
                }
                _ => {} // Ignore other event types (e.g., Access, Other)
            }
        },
    ).expect("Failed to create file watcher");

    watcher.watch(&PathBuf::from("./images"), RecursiveMode::Recursive).ok();

    watcher
}
