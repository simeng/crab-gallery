pub mod app;
pub mod controllers;
use crate::app::ImageFile;

use std::collections::HashMap;
use std::fs::metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock as TokioRwlock;

use chrono::{DateTime, Local};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use walkdir::{DirEntry, WalkDir};

/// One configured image folder: how it looks in keys/URLs (`display`) and its
/// canonical on-disk location (used for watcher event matching).
#[derive(Clone, Debug)]
pub struct ImageDir {
    pub display: String,
    pub canonical: PathBuf,
}

/// Normalize configured `--dir` values. Missing dirs are skipped with a warning.
pub fn resolve_dirs(dirs: &[PathBuf]) -> Vec<ImageDir> {
    let mut out = Vec::new();
    for d in dirs {
        let display = d.to_string_lossy().into_owned();
        match std::fs::canonicalize(d) {
            Ok(canonical) => out.push(ImageDir {
                // Keys keep the dir as the user typed it (without trailing '/').
                display: display.trim_end_matches('/').to_string(),
                canonical,
            }),
            Err(e) => eprintln!("warning: image dir '{}' not found or not a directory ({}); skipping", display, e),
        }
    }
    out
}

/// Map a watcher event path (absolute) to our canonical `<dir>/<file>` key.
pub fn relative_image_key(p: &Path, dirs: &[ImageDir]) -> Option<String> {
    for dir in dirs {
        if let Ok(rel) = p.strip_prefix(&dir.canonical) {
            let rel_str = rel.to_str()?;
            return Some(format!("{}/{}", dir.display, rel_str));
        }
    }
    None
}

pub fn is_supported_image_ext(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            lower == "jpg" || lower == "jpeg" || lower == "png"
        })
        .unwrap_or(false)
}

/// Make an uploaded filename safe to use directly on disk: keep only the
/// basename, allow [A-Za-z0-9._-], map everything else (incl. spaces) to `_`.
pub fn sanitize_filename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "upload".to_string()
    } else {
        out
    }
}

/// Pick a non-colliding target path inside `dir` (timestamp suffix).
pub fn unique_image_path(dir: &str, filename: &str) -> String {
    let candidate = format!("{}/{}", dir.trim_end_matches('/'), filename);
    if !std::path::Path::new(&candidate).exists() {
        return candidate;
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename);
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext {
        Some(ext) => format!("{}/{}_{}.{}", dir.trim_end_matches('/'), stem, ts, ext),
        None => format!("{}/_{}", candidate, ts),
    }
}

/// Load image dimensions + metadata into an `ImageFile` (path = the key).
/// NOTE: libvips types are !Send — call this from a synchronous context or
/// inside a single spawn_blocking closure; never across async task boundaries.
pub fn load_image_info(path_str: &str) -> Option<Arc<ImageFile>> {
    let img = libvips::VipsImage::new_from_file(path_str).ok()?;
    let width = img.get_width();
    let height = img.get_height();
    drop(img);
    let filename = Path::new(path_str)
        .file_name()
        .map(|e| e.to_string_lossy().into_owned());
    let modified_at = metadata(path_str).ok().and_then(|m| m.modified().ok().map(Into::<DateTime<Local>>::into));
    Some(Arc::new(ImageFile {
        path: path_str.to_string(),
        title: filename,
        width,
        height,
        modified_at,
    }))
}

/// Synchronous scan of all configured dirs for the initial load.
pub fn scan_images(dirs: &[ImageDir]) -> (HashMap<String, Arc<ImageFile>>, Vec<Arc<ImageFile>>) {
    let mut image_list: Vec<Arc<ImageFile>> = vec![];

    for dir in dirs {
        for i in WalkDir::new(&dir.canonical) {
            let entry: DirEntry = match i { Ok(e) => e, Err(_) => continue };
            let path = entry.path();
            if !entry.file_type().is_file() || !is_supported_image_ext(path.to_str().unwrap_or("")) {
                continue;
            }
            let Some(rel) = path.strip_prefix(&dir.canonical).ok().and_then(|r| r.to_str()) else {
                continue;
            };
            let key = format!("{}/{}", dir.display, rel);
            match load_image_info(&key) {
                Some(img) => image_list.push(img),
                None => println!("err reading image: {}", key),
            }
        }
    }

    sort_image_list(&mut image_list);
    let images = image_list
        .iter()
        .map(|img| (img.path.clone(), Arc::clone(img)))
        .collect();
    (images, image_list)
}

pub fn sort_image_list(list: &mut Vec<Arc<ImageFile>>) {
    list.sort_by(|a, b| {
        let ta = a.modified_at.map(|d| d.naive_utc());
        let tb = b.modified_at.map(|d| d.naive_utc());
        tb.cmp(&ta)
    });
}

/// Spawn a file watcher over ALL configured dirs that handles
/// add/remove/modify events without rescanning.
///
/// IMPORTANT: do NOT replace this with an interval-based rescan/poll loop.
/// We rely on notify's OS-level events so only changed files are touched.
pub fn spawn_image_watcher(
    handle: Handle,
    images: Arc<TokioRwlock<HashMap<String, Arc<ImageFile>>>>,
    image_list: Arc<TokioRwlock<Vec<Arc<ImageFile>>>>,
    dirs: Vec<ImageDir>,
) -> RecommendedWatcher {
    let watch_dirs = dirs.clone();
    let mut watcher = notify::recommended_watcher(move |res| {
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
                    if !is_supported_image_ext(path.to_str().unwrap_or("")) {
                        continue;
                    }
                    let Some(path_str) = relative_image_key(path, &watch_dirs) else {
                        eprintln!("Skipping event outside configured dirs: {}", path.display());
                        continue;
                    };
                    // Load dims synchronously (vips is not Send-safe). Retry a
                    // few times in case the file is still being written.
                    let mut image_file = None;
                    for _retry in 0..5 {
                        if let Some(img) = load_image_info(&path_str) {
                            image_file = Some(img);
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    let Some(image_file) = image_file else {
                        eprintln!("Failed to load new image after retries: {}", path.display());
                        continue;
                    };
                    let images2 = Arc::clone(&images);
                    let image_list2 = Arc::clone(&image_list);
                    let title = image_file.title.clone().unwrap_or_default();
                    let w = image_file.width;
                    let h = image_file.height;
                    handle.spawn(async move {
                        let mut map = images2.write().await;
                        let mut list = image_list2.write().await;
                        // Idempotent: uploads already insert this path directly.
                        list.retain(|i| i.path != image_file.path);
                        map.insert(image_file.path.clone(), Arc::clone(&image_file));
                        list.push(image_file);
                        sort_image_list(&mut list);
                        eprintln!("Added image: {} ({}x{})", title, w, h);
                    });
                }
            }
            EventKind::Remove(_) => {
                for path in &evt.paths {
                    let Some(title) = path.file_name().map(|e| e.to_string_lossy().into_owned()) else { continue };
                    let images2 = Arc::clone(&images);
                    let image_list2 = Arc::clone(&image_list);
                    handle.spawn(async move {
                        let mut map = images2.write().await;
                        let mut list = image_list2.write().await;
                        let matched: Vec<String> = map
                            .iter()
                            .filter(|(_, img)| img.title.as_deref() == Some(title.as_str()))
                            .map(|(k, _)| k.clone())
                            .collect();
                        for key in &matched {
                            map.remove(key);
                            list.retain(|img| &img.path != key);
                        }
                        eprintln!("Removed image: {}", title);
                    });
                }
            }
            EventKind::Modify(_) => {
                let Some(p) = evt.paths.first() else { return };
                if !is_supported_image_ext(p.to_str().unwrap_or("")) {
                    return;
                }
                // Refresh mtime + dims when they change.
                let Some(path_str) = relative_image_key(p, &watch_dirs) else { return };
                let modified_at = std::fs::metadata(&path_str)
                    .ok()
                    .and_then(|m| m.modified().ok().map(Into::<DateTime<Local>>::into));
                let Some(new_dims) = libvips::VipsImage::new_from_file(&path_str).ok().map(|img| {
                    let (w, h) = (img.get_width(), img.get_height());
                    drop(img);
                    (w, h)
                }) else {
                    return;
                };

                let images2 = Arc::clone(&images);
                let image_list2 = Arc::clone(&image_list);
                handle.spawn(async move {
                    let mut map = images2.write().await;
                    let mut list = image_list2.write().await;
                    if let Some(img) = map.get(&path_str) {
                        if new_dims != (img.width, img.height) || modified_at != img.modified_at {
                            let new_image = Arc::new(ImageFile {
                                path: path_str.clone(),
                                title: img.title.clone(),
                                width: new_dims.0,
                                height: new_dims.1,
                                modified_at,
                            });
                            map.insert(path_str.clone(), Arc::clone(&new_image));
                            list.retain(|i| i.path != path_str);
                            list.push(new_image);
                            sort_image_list(&mut list);
                            eprintln!("Updated image: {} ({}x{})", path_str, new_dims.0, new_dims.1);
                        }
                    }
                });
            }
            _ => {} // Ignore other event types (e.g., Access, Other)
        }
    })
    .expect("Failed to create file watcher");

    for dir in &dirs {
        watcher
            .watch(&dir.canonical, RecursiveMode::Recursive)
            .unwrap_or_else(|e| eprintln!("failed to watch {}: {}", dir.display, e));
    }

    watcher
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{PreviewCache, PreviewKey};

    #[test]
    fn sanitize_strips_paths_and_bad_chars() {
        assert_eq!(sanitize_filename("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("my cool photo (1).JPG"), "my_cool_photo__1_.JPG");
        assert_eq!(sanitize_filename(""), "upload");
    }

    fn key(path: &str) -> PreviewKey {
        PreviewKey { path: path.into(), width: 100, mtime_secs: 0 }
    }

    #[test]
    fn cache_stores_and_replaces() {
        let mut cache = PreviewCache::new();
        assert!(cache.get(&key("a")).is_none());
        cache.insert(key("a"), vec![1]);
        cache.insert(key("b"), vec![2]);
        assert_eq!(cache.get(&key("a")), Some(&[1][..]));
        assert_eq!(cache.get(&key("b")), Some(&[2][..]));
        // Re-inserting replaces bytes and does not grow the cache.
        cache.insert(key("a"), vec![9]);
        assert_eq!(cache.get(&key("a")), Some(&[9][..]));
    }

    #[test]
    fn unique_path_avoids_collisions() {
        std::fs::create_dir_all("./images").ok();
        let target = unique_image_path("./images", "exists_98765.jpg");
        std::fs::write(&target, b"x").ok();
        let unique = unique_image_path("./images", "exists_98765.jpg");
        assert_ne!(unique, target);
        let _ = std::fs::remove_file(target);
    }
}
