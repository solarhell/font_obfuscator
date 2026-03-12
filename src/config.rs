use std::env;

#[derive(Clone)]
pub struct FontConfig {
    pub family_name: String,
    pub style_name: String,
    pub copyright: String,
    pub version: String,
    pub vendor_url: String,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family_name: "MyAwesomeFont".into(),
            style_name: "Regular".into(),
            copyright: "Created by solarhell".into(),
            version: "Version 1.0".into(),
            vendor_url: "https://solarhell.com/".into(),
        }
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub port: u16,
    pub font: FontConfig,
    pub base_font_path: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            port: env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(1323),
            font: FontConfig::default(),
            base_font_path: env::var("BASE_FONT_PATH")
                .unwrap_or_else(|_| "base-font/KaiGenGothicCN-Regular.ttf".into()),
        }
    }
}
