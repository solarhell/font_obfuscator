use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct EncryptRequest {
    pub plaintext: String,
    pub shadowtext: String,
    pub only_ttf: bool,
}

#[derive(Deserialize)]
pub struct EncryptPlusRequest {
    pub plaintext: String,
    pub only_ttf: bool,
}

#[derive(Serialize)]
pub struct CommonResponse<T: Serialize> {
    pub message: String,
    pub hint: String,
    pub response: Option<T>,
}

pub fn success_response<T: Serialize>(data: T) -> CommonResponse<T> {
    CommonResponse {
        message: "success".into(),
        hint: String::new(),
        response: Some(data),
    }
}

pub fn error_response<T: Serialize>(hint: &str) -> CommonResponse<T> {
    CommonResponse {
        message: "error".into(),
        hint: hint.into(),
        response: None,
    }
}

#[derive(Serialize)]
pub struct EncryptResponse {
    pub base64ed: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct EncryptPlusResponse {
    pub base64ed: HashMap<String, String>,
    pub html_entities: HashMap<String, String>,
}
