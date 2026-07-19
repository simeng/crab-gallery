use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Local};
use libvips::VipsApp;
use serde::{Deserialize, Serialize};
use tera::Tera;

pub struct AppState {
    pub vips: VipsApp,
    pub tera: Tera,
    pub images: HashMap<String, Arc<ImageFile>>,
    pub image_list: Vec<Arc<ImageFile>>,
}

#[derive(Deserialize, Debug, Serialize)]
pub enum FitOption {
    #[serde(rename = "contain")]
    Contain,
    /*
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "fill-max")]
    FillMax,
    #[serde(rename = "stretch")]
    Stretch,
    #[serde(rename = "cover")]
    Cover,
    #[serde(rename = "crop")]
    Crop,
    */
}

#[derive(Deserialize, Debug, Serialize)]
pub struct ResizeParams {
    pub w: Option<i32>,
    pub h: Option<i32>,
    pub fit: Option<FitOption>,
}

#[derive(Deserialize, Debug, Serialize, Clone, Default)]
pub struct ImageFile {
    pub path: String,
    pub title: Option<String>,
    pub width: i32,
    pub height: i32,
    pub modified_at: Option<DateTime<Local>>,
}
