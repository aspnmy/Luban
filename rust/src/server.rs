//! HTTP API 服务 —— 为 /mnt/e/OK（ok-browser）等内网组件提供图片压缩能力。
//!
//! 端点：
//!   POST /compress          上传图片文件，返回压缩后的 JPEG 字节流
//!   POST /compress/base64   JSON 传 base64，返回 base64（便于纯前端调用）
//!   GET  /health            存活探测
//!   GET  /info              服务能力与参数说明
//!
//! ⚠️ axum 的 body 默认上限 2MB（见 feedback/axum-json-body-limit），
//!    图片动辄 5-20MB ⇒ 必须显式放宽到 [`MAX_BODY_BYTES`]。

use crate::compress::{
    calculate_target, compress_to_format, decide_format, decode_with_orientation, has_alpha,
    resize_to, OutputFormat,
};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Query},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

/// 请求体上限：32MB（留足 20MB 原始图 + multipart 开销）
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// 压缩结果元信息（响应头 X-Luban-* 携带，便于调用方记录）
#[derive(Debug, Serialize)]
pub struct CompressMeta {
    /// 原始字节数
    pub original_bytes: usize,
    /// 输出字节数
    pub output_bytes: usize,
    /// 压缩率（output / original，越小越好）
    pub ratio: f64,
    /// 原始尺寸
    pub original_width: u32,
    pub original_height: u32,
    /// 输出尺寸
    pub output_width: u32,
    pub output_height: u32,
    /// 实际使用的 JPEG 质量
    pub quality: u8,
    /// 实际输出格式（`jpeg` / `png`）
    pub format: String,
    /// 解码耗时（毫秒）
    pub decode_ms: u128,
    /// 缩放到目标尺寸耗时（毫秒）
    pub resize_ms: u128,
    /// 质量搜索 + 编码耗时（毫秒）
    pub encode_ms: u128,
}

/// 查询参数（两个端点共用）
#[derive(Debug, Deserialize, Default)]
pub struct CompressParams {
    /// 固定质量（1-100）；给定则跳过体积搜索
    pub quality: Option<u8>,
    /// 强制目标体积（KB）；不传则按算法估算
    pub target_kb: Option<u32>,
    /// 强制目标宽度（不传则按算法算）
    pub width: Option<u32>,
    /// 强制目标高度
    pub height: Option<u32>,
    /// 集中压缩透明通道（对应原 `Luban.setFocusAlpha`）：
    /// 仅当源图是 PNG 且带 alpha 时生效 ⇒ 输出 PNG、保留透明通道；其余仍输出 JPEG。
    pub focus_alpha: Option<bool>,
}

impl CompressParams {
    /// `focus_alpha` 缺省为 false（与原 Luban 默认值一致）。
    pub fn focus_alpha(&self) -> bool {
        self.focus_alpha.unwrap_or(false)
    }
}

/// 根据请求参数与图片原始尺寸算出 (目标宽, 目标高, 目标体积KB)。
fn resolve_target(
    img_w: u32,
    img_h: u32,
    p: &CompressParams,
) -> (u32, u32, Option<u32>) {
    // 显式指定尺寸 ⇒ 直接用（⛔ 不做不放大检查，调用方明确要这个尺寸）
    if let (Some(w), Some(h)) = (p.width, p.height) {
        return (w.max(2), h.max(2), p.target_kb);
    }

    let t = calculate_target(img_w, img_h).unwrap_or(crate::compress::CompressionTarget {
        width: img_w,
        height: img_h,
        estimated_size_kb: 0,
        is_long_image: false,
        target_size_kb: None,
    });

    // 单边指定：按比例推另一边
    let (mut w, mut h) = (t.width, t.height);
    match (p.width, p.height) {
        (Some(fw), None) if fw > 0 => {
            h = (h as f64 * fw as f64 / w as f64) as u32;
            w = fw;
        }
        (None, Some(fh)) if fh > 0 => {
            w = (w as f64 * fh as f64 / h as f64) as u32;
            h = fh;
        }
        _ => {}
    }

    // 体积目标：显式优先，否则用算法估算（仅长图/普通图都可用作搜索上限）
    let target_kb = p.target_kb.or(Some(t.estimated_size_kb).filter(|v| *v > 0));
    (w.max(2), h.max(2), target_kb)
}

/// 从 multipart 文件名或魔数推断格式。
fn detect_format(bytes: &[u8], filename: Option<&str>) -> Option<ImageFormat> {
    // 优先按魔数（文件名不可信、也可能是 .jpg 实为 PNG）
    if let Ok(f) = image::guess_format(bytes) {
        return Some(f);
    }
    // 回退：按扩展名
    let name = filename?.to_ascii_lowercase();
    if name.ends_with(".jpg") || name.ends_with(".jpeg") {
        Some(ImageFormat::Jpeg)
    } else if name.ends_with(".png") {
        Some(ImageFormat::Png)
    } else if name.ends_with(".webp") {
        Some(ImageFormat::WebP)
    } else if name.ends_with(".bmp") {
        Some(ImageFormat::Bmp)
    } else if name.ends_with(".gif") {
        Some(ImageFormat::Gif)
    } else if name.ends_with(".tif") || name.ends_with(".tiff") {
        Some(ImageFormat::Tiff)
    } else {
        None
    }
}

/// 核心压缩流程：字节 → 压缩后 JPEG 字节 + 元信息。
pub fn compress_bytes(
    raw: &[u8],
    filename: Option<&str>,
    params: &CompressParams,
) -> Result<(Vec<u8>, CompressMeta), String> {
    let fmt = detect_format(raw, filename)
        .ok_or_else(|| "无法识别的图片格式（支持 jpeg/png/webp/bmp/gif/tiff）".to_string())?;

    let t0 = std::time::Instant::now();
    let img = decode_with_orientation(raw, fmt)?;
    let decode_ms = t0.elapsed().as_millis();

    let (ow, oh) = (img.width(), img.height());
    let (tw, th, target_kb) = resolve_target(ow, oh, params);

    let t1 = std::time::Instant::now();
    let resized = resize_to(&img, tw, th);
    let resize_ms = t1.elapsed().as_millis();

    let (rw, rh) = (resized.width(), resized.height());

    let t2 = std::time::Instant::now();
    let format = decide_format(fmt, has_alpha(&resized), params.focus_alpha(), params.quality, None);
    let out = compress_to_format(&resized, format, target_kb, params.quality)?;
    let encode_ms = t2.elapsed().as_millis();

    // 反推实际质量：重新按 low 编码比较不可靠 ⇒ 用输出体积粗估
    let quality = params.quality.unwrap_or(0);

    let meta = CompressMeta {
        original_bytes: raw.len(),
        output_bytes: out.len(),
        ratio: if raw.is_empty() {
            0.0
        } else {
            out.len() as f64 / raw.len() as f64
        },
        original_width: ow,
        original_height: oh,
        output_width: rw,
        output_height: rh,
        quality,
        format: match format {
            OutputFormat::Jpeg => "jpeg".to_string(),
            OutputFormat::Png => "png".to_string(),
        },
        decode_ms,
        resize_ms,
        encode_ms,
    };

    Ok((out, meta))
}

/// 把元信息塞进响应头，方便调用方在不解析 body 的情况下记录指标。
fn meta_headers(meta: &CompressMeta) -> HeaderMap {
    let mut h = HeaderMap::new();
    let pairs = [
        ("x-luban-original-bytes", meta.original_bytes.to_string()),
        ("x-luban-output-bytes", meta.output_bytes.to_string()),
        ("x-luban-ratio", format!("{:.4}", meta.ratio)),
        ("x-luban-original-size", format!("{}x{}", meta.original_width, meta.original_height)),
        ("x-luban-output-size", format!("{}x{}", meta.output_width, meta.output_height)),
        ("x-luban-encode-ms", meta.encode_ms.to_string()),
        ("x-luban-format", meta.format.clone()),
    ];
    for (k, v) in pairs {
        if let (Ok(name), Ok(val)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            header::HeaderValue::from_str(&v),
        ) {
            h.insert(name, val);
        }
    }
    h
}

/// POST /compress —— multipart/form-data，字段名 `file`（或任意字段名，取第一个文件）。
async fn handle_compress(
    Query(params): Query<CompressParams>,
    mut mp: Multipart,
) -> Response {
    let mut found: Option<(Vec<u8>, Option<String>)> = None;

    // 遍历所有字段，取第一个带文件名的字节流
    loop {
        match mp.next_field().await {
            Ok(Some(field)) => {
                let name = field.file_name().map(|s| s.to_string());
                let is_file = name.is_some();   // 先存标记：name 会被 move 进 found
                match field.bytes().await {
                    Ok(b) => {
                        if is_file || found.is_none() {
                            found = Some((b.to_vec(), name));
                        }
                        if is_file {
                            break; // 找到真正的文件字段即可停
                        }
                    }
                    Err(e) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"ok": false, "error": format!("读取字段失败: {e}")})),
                        )
                            .into_response();
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": format!("multipart 解析失败: {e}")})),
                )
                    .into_response();
            }
        }
    }

    let Some((bytes, fname)) = found else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "未收到文件字段（请用 multipart form 上传）"})),
        )
            .into_response();
    };

    match compress_bytes(&bytes, fname.as_deref(), &params) {
        Ok((out, meta)) => {
            let mut resp = (StatusCode::OK, out).into_response();
            *resp.headers_mut() = meta_headers(&meta);
            let ct = match meta.format.as_str() {
                "png" => "image/png",
                _ => "image/jpeg",
            };
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(ct),
            );
            resp
        }
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

/// base64 请求体
#[derive(Debug, Deserialize)]
struct Base64Req {
    /// base64（可带 data:image/...;base64, 前缀）
    data: String,
    /// 原文件名（用于格式回退推断）
    #[serde(default)]
    filename: Option<String>,
}

/// POST /compress/base64 —— JSON 传 base64，返回 base64。
async fn handle_compress_base64(
    Query(params): Query<CompressParams>,
    Json(req): Json<Base64Req>,
) -> Response {
    let payload = req
        .data
        .split_once("base64,")
        .map(|(_, b)| b)
        .unwrap_or(&req.data);

    let raw = match base64_decode(payload) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": format!("base64 解码失败: {e}")})),
            )
                .into_response();
        }
    };

    match compress_bytes(&raw, req.filename.as_deref(), &params) {
        Ok((out, meta)) => Json(json!({
            "ok": true,
            "data": base64_encode(&out),
            "meta": meta,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

/// GET /health
async fn handle_health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "service": "luban-api", "version": env!("CARGO_PKG_VERSION")}))
}

/// GET /info —— 能力与默认参数说明
async fn handle_info() -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "service": "luban-api",
        "version": env!("CARGO_PKG_VERSION"),
        "algorithm": "Luban (ported from master branch Kotlin implementation)",
        "formats_in": ["jpeg", "png", "webp", "bmp", "gif", "tiff"],
        "format_out": "jpeg",
        "params": {
            "quality": "1-100，给定则跳过体积搜索（默认按目标体积二分）",
            "target_kb": "目标体积（KB），不传按算法估算",
            "width": "目标宽（单边给定按比例推另一边）",
            "height": "目标高"
        },
        "defaults": {
            "base_short": 1440,
            "cap_pixels": 10240000,
            "trap_pixels": 40960000,
            "wall_long": 10800
        },
        "endpoints": {
            "POST /compress": "multipart/form-data，字段 file",
            "POST /compress/base64": "JSON {data, filename?}",
            "GET /health": "存活探测",
            "GET /info": "本说明"
        },
        "max_body_bytes": MAX_BODY_BYTES
    }))
}

/// 构造路由（供 main 与测试复用）
///
/// 同时挂上 MCP 端点 `/mcp`（JSON-RPC over HTTP），供网关作为工具服务接入。
pub fn build_router() -> Router {
    let mcp_state = crate::mcp::McpState::from_env();
    Router::new()
        .route("/health", get(handle_health))
        .route("/info", get(handle_info))
        .route("/compress", post(handle_compress))
        .route("/compress/base64", post(handle_compress_base64))
        .route("/", get(crate::webui::handle_index))
        .route("/index.html", get(crate::webui::handle_index))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .merge(crate::mcp::build_mcp_router(mcp_state))
}

// ── 极简 base64（避免为两个函数引入额外依赖）──────────────────────────

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64_CHARS[((n >> 18) & 63) as usize] as char);
        out.push(B64_CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_CHARS[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_CHARS[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut table = HashMap::with_capacity(64);
    for (i, c) in B64_CHARS.iter().enumerate() {
        table.insert(*c, i as u8);
    }

    let clean: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();

    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks(4) {
        if chunk.len() < 2 {
            return Err("base64 长度非法".into());
        }
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            let v = *table
                .get(b)
                .ok_or_else(|| format!("base64 含非法字符: {}", *b as char))?;
            n |= (v as u32) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for s in [b"a".as_slice(), b"ab", b"abc", b"abcd", b"hello world!"] {
            let enc = base64_encode(s);
            let dec = base64_decode(&enc).unwrap();
            assert_eq!(dec, s.to_vec(), "roundtrip failed for {:?}", s);
        }
    }

    #[test]
    fn meta_ratio_computed() {
        // 构造一张纯色 PNG，压缩后应显著变小
        let mut buf = Vec::new();
        let img = image::RgbImage::from_pixel(800, 600, image::Rgb([120, 130, 140]));
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        let (out, meta) = compress_bytes(&buf, Some("t.png"), &CompressParams::default()).unwrap();
        assert!(!out.is_empty());
        assert!(meta.output_bytes > 0);
        assert!(meta.ratio > 0.0);
    }
}
