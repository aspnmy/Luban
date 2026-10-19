//! WebUI —— 内置的静态单页（`GET /` 与 `GET /index.html`）。
//!
//! 定位：对内对接本服务自身的 API（`POST /compress` multipart），
//! 纯前端调自身接口，**不需要服务器缓存**（不落盘、不依赖 `LUBAN_ALLOWED_ROOTS`）。

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

/// 单页 HTML（自包含，无外部资源）
const INDEX_HTML: &str = include_str!("webui.html");

/// GET / 与 GET /index.html —— 返回内置单页
pub async fn handle_index() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/html; charset=utf-8",
        )],
        INDEX_HTML,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_is_self_contained() {
        // 页面必须自带上传表单与调用自身 /compress 的 JS；不允许外部资源
        assert!(INDEX_HTML.contains("POST /compress") || INDEX_HTML.contains("/compress"));
        assert!(INDEX_HTML.contains("<form"));
        assert!(!INDEX_HTML.contains("http://"));
        assert!(!INDEX_HTML.contains("https://"));
        assert!(!INDEX_HTML.contains("<script src="));
        assert!(!INDEX_HTML.contains("<link"));
    }
}
