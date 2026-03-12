mod config;
mod core;
mod model;
mod utils;

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use tower_http::cors::CorsLayer;

use crate::config::AppConfig;
use crate::model::*;

struct AppState {
    config: AppConfig,
    font_data: Vec<u8>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env();
    let font_data = std::fs::read(&config.base_font_path).unwrap_or_else(|e| {
        // Try relative to executable
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_default();
        let alt_path = exe_dir.join(&config.base_font_path);
        std::fs::read(&alt_path).unwrap_or_else(|_| {
            panic!("无法读取基础字体文件 '{}': {}", config.base_font_path, e);
        })
    });

    let addr = format!("{}:{}", config.listen_addr, config.port);
    tracing::info!("服务启动于 {}", addr);

    let state = Arc::new(AppState { config, font_data });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/encrypt", post(encrypt))
        .route("/api/encrypt-plus", post(encrypt_plus))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> &'static str {
    "it works"
}

async fn encrypt(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EncryptRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let filename = uuid::Uuid::new_v4().to_string();
    let output_dir = PathBuf::from("output");

    let obfuscate_result = if req.keep_all {
        crate::core::obfuscate_full(
            &req.plaintext,
            &req.shadowtext,
            &state.font_data,
            &output_dir,
            &filename,
            req.only_ttf,
        )
    } else {
        crate::core::obfuscate(
            &req.plaintext,
            &req.shadowtext,
            &state.font_data,
            &state.config.font,
            &output_dir,
            &filename,
            req.only_ttf,
        )
    };

    match obfuscate_result {
        Ok(result) => {
            let mut base64ed = std::collections::HashMap::new();
            for (format, path) in &result.files {
                match utils::base64_binary(path) {
                    Ok(b64) => {
                        base64ed.insert(format.clone(), b64);
                    }
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(
                                serde_json::to_value(error_response::<()>(&e.to_string())).unwrap(),
                            ),
                        );
                    }
                }
                // Clean up temp file
                let _ = std::fs::remove_file(path);
            }

            let resp = EncryptResponse { base64ed };
            (
                StatusCode::OK,
                Json(serde_json::to_value(success_response(resp)).unwrap()),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(serde_json::to_value(error_response::<()>(&e.to_string())).unwrap()),
        ),
    }
}

async fn encrypt_plus(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EncryptPlusRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let filename = uuid::Uuid::new_v4().to_string();
    let output_dir = PathBuf::from("output");

    match crate::core::obfuscate_plus(
        &req.plaintext,
        &state.font_data,
        &state.config.font,
        &output_dir,
        &filename,
        req.only_ttf,
    ) {
        Ok(result) => {
            let mut base64ed = std::collections::HashMap::new();
            for (format, path) in &result.files {
                match utils::base64_binary(path) {
                    Ok(b64) => {
                        base64ed.insert(format.clone(), b64);
                    }
                    Err(e) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(
                                serde_json::to_value(error_response::<()>(&e.to_string())).unwrap(),
                            ),
                        );
                    }
                }
                let _ = std::fs::remove_file(path);
            }

            let resp = EncryptPlusResponse {
                base64ed,
                html_entities: result.html_entities,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(success_response(resp)).unwrap()),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(serde_json::to_value(error_response::<()>(&e.to_string())).unwrap()),
        ),
    }
}
