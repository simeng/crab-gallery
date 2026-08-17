use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use tera::Tera;
use tokio::sync::RwLock;

pub struct AppState {
    pub tera: Tera,
    /// All known images keyed by their canonical `<dir>/<file>` path.
    pub images: Arc<RwLock<HashMap<String, Arc<ImageFile>>>>,
    /// Same images as a list (kept sorted newest-first).
    pub image_list: Arc<RwLock<Vec<Arc<ImageFile>>>>,
    /// LRU cache of pre-encoded preview JPEGs, keyed by (path, width, mtime).
    pub preview_cache: Mutex<PreviewCache>,
    /// API key for the upload endpoint. `None` => uploads disabled.
    pub api_key: Option<String>,
    /// Image folders in flag order; keys look like `<dir>/<file>`.
    pub dirs: Vec<std::path::PathBuf>,
    /// Where `POST /upload` saves files (defaults to the first dir).
    pub upload_dir: std::path::PathBuf,
}

#[derive(Deserialize, Debug)]
pub struct ImageParams {
    /// Request a scaled preview at this width (px). Omit to get the raw source.
    #[serde(default)]
    pub w: Option<i32>,
    /// Explicitly request the raw, unmodified source file. Accepted as a bare
    /// `?orig` (no value) or `?orig=true`; `false`/`0` fall through to normal
    /// handling. (A valueless param can't deserialize into `Option<bool>`.)
    #[serde(default)]
    pub orig: Option<String>,
}

impl ImageParams {
    pub fn wants_original(&self) -> bool {
        self.orig
            .as_deref()
            .map(|s| s != "false" && s != "0")
            .unwrap_or(false)
    }
}

#[derive(Deserialize, Debug, Serialize, Clone, Default)]
pub struct ImageFile {
    pub path: String,
    pub title: Option<String>,
    pub width: i32,
    pub height: i32,
    pub modified_at: Option<DateTime<Local>>,
}

/// Key for the in-memory preview cache. Including the file mtime means a
/// modified source file automatically misses and gets regenerated.
#[derive(Clone, PartialEq, Eq)]
pub struct PreviewKey {
    pub path: String,
    pub width: u32,
    pub mtime_secs: i64,
}

impl Hash for PreviewKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
        self.width.hash(state);
        self.mtime_secs.hash(state);
    }
}

/// Bounded LRU cache of generated preview JPEGs (owned bytes, cheap to share).
pub struct PreviewCache {
    entries: HashMap<PreviewKey, Vec<u8>>,
    order: VecDeque<PreviewKey>,
}

impl PreviewCache {
    pub const CAPACITY: usize = 1024;

    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&self, key: &PreviewKey) -> Option<&[u8]> {
        self.entries.get(key).map(|v| v.as_slice())
    }

    pub fn insert(&mut self, key: PreviewKey, bytes: Vec<u8>) {
        if self.entries.contains_key(&key) {
            // Refresh recency.
            self.order.retain(|k| k != &key);
        } else {
            while self.entries.len() >= Self::CAPACITY {
                if let Some(evicted) = self.order.pop_front() {
                    self.entries.remove(&evicted);
                } else {
                    break;
                }
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, bytes);
    }
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

/// JSON response returned by `POST /upload`.
#[derive(Serialize, Debug)]
pub struct UploadResponse {
    pub saved: Vec<String>,
    pub errors: Vec<String>,
}
