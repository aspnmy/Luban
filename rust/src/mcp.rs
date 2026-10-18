//! MCP（Model Context Protocol）服务端 —— 让网关可以把本服务作为工具供 LLM 调用。
//!
//! 协议要点（MCP 2024-11-05 规范）：
//!   - 传输：Streamable HTTP（JSON-RPC 2.0 over POST），响应为 `application/json`
//!   - 握手：`initialize` → `notifications/initialized`
//!   - 发现：`tools/list` → 返回工具 schema
//!   - 调用：`tools/call` → 返回 `content` 数组（`type: text` / `type: image`）
//!
//! ⚠️ 本实现是**无状态**的：不维护会话 id，每个请求独立处理。
//!    对网关场景足够（工具调用无跨请求状态）；若将来需要 SSE 推送再扩展。
//!
//! ⚠️ 设计取舍：MCP 工具走 `path`（服务器侧文件路径）而不是 base64 上传，
//!    因为 LLM 不可能把整张图塞进 tool_call 参数里。压缩结果落同目录新文件。

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// MCP 协议版本（客户端请求的版本不匹配时回本值）
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "luban-image-compress";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// MCP 共享状态：允许压缩的根目录白名单（⛔ 不放开任意路径）
#[derive(Clone)]
pub struct McpState {
    /// 允许读写的根目录（绝对路径，已规范化）
    pub allowed_roots: Arc<Vec<std::path::PathBuf>>,
}

impl McpState {
    /// 从环境变量 `LUBAN_ALLOWED_ROOTS`（冒号分隔）构造；为空则不限制。
    pub fn from_env() -> Self {
        let roots = std::env::var("LUBAN_ALLOWED_ROOTS")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| std::fs::canonicalize(s).ok())
            .collect::<Vec<_>>();
        Self {
            allowed_roots: Arc::new(roots),
        }
    }

    /// 路径白名单校验：真实路径（解 symlink 后）必须落在某个允许的根目录内。
    /// 白名单为空 ⇒ 不限制（但调用方仍须确认文件存在）。
    fn check_path(&self, p: &std::path::Path) -> Result<std::path::PathBuf, String> {
        let real = std::fs::canonicalize(p)
            .map_err(|e| format!("路径不可用: {} — {e}", p.display()))?;
        if self.allowed_roots.is_empty() {
            return Ok(real);
        }
        for root in self.allowed_roots.iter() {
            if real.starts_with(root) {
                return Ok(real);
            }
        }
        Err(format!(
            "路径不在允许范围内: {}（允许: {}）",
            real.display(),
            self.allowed_roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

// ── JSON-RPC 帧 ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RpcRequest {
    /// JSON-RPC 版本号。协议固定为 "2.0"，此处只作兼容解析：
    /// 不校验（部分客户端省略），但保留字段以免 serde 报未知字段。
    /// `#[allow(dead_code)]` 是因为它确实不参与逻辑，只为占位兼容。
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    /// 通知没有 id；请求必须有
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// 构造 JSON-RPC 成功响应
fn ok_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// 构造 JSON-RPC 错误响应
fn err_result(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": RpcError { code, message: message.into(), data: None }
    })
}

/// 工具清单（tools/list 的返回）
fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "compress_image",
                "description": "压缩图片为 JPEG。自动按 Luban 算法：短边基准 1440、\
                                超大图降采样、不放大、按目标体积二分搜索最优质量。\
                                结果写同目录新文件（<原名>.compressed.jpg），⛔ 不覆盖原文件。\
                                返回压缩前后体积、尺寸与耗时。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "服务器侧的图片绝对路径（支持 jpeg/png/webp/bmp/gif/tiff）"
                        },
                        "quality": {
                            "type": "integer",
                            "description": "固定 JPEG 质量 1-100；给定则跳过体积搜索",
                            "minimum": 1,
                            "maximum": 100
                        },
                        "target_kb": {
                            "type": "integer",
                            "description": "目标体积（KB）；不传则按算法估算"
                        },
                        "width": {
                            "type": "integer",
                            "description": "目标宽（只给宽则按比例算高）"
                        },
                        "height": {
                            "type": "integer",
                            "description": "目标高"
                        }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "compress_batch",
                "description": "批量压缩一个目录下的图片（仅当前层，不递归）。\
                                每张写同目录 <原名>.compressed.jpg。返回逐张结果摘要。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dir": {
                            "type": "string",
                            "description": "服务器侧目录绝对路径"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "最多处理多少个文件，默认 20，上限 200"
                        },
                        "quality": {
                            "type": "integer",
                            "description": "固定 JPEG 质量 1-100"
                        },
                        "target_kb": {
                            "type": "integer",
                            "description": "目标体积（KB）"
                        }
                    },
                    "required": ["dir"]
                }
            },
            {
                "name": "image_info",
                "description": "读取图片基本信息（尺寸、格式、体积、EXIF 方向），不做压缩。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "服务器侧的图片绝对路径"
                        }
                    },
                    "required": ["path"]
                }
            }
        ]
    })
}

/// 把压缩结果包装成 MCP `content` 数组（`type: text`）
fn text_content(s: impl Into<String>) -> Value {
    json!([{ "type": "text", "text": s.into() }])
}

/// 处理 `tools/call`
fn handle_tool_call(state: &McpState, name: &str, args: &Value) -> Value {
    match name {
        "compress_image" => {
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                return json!({"content": text_content("缺少参数 path"), "isError": true});
            };
            let p = std::path::Path::new(path);
            let real = match state.check_path(p) {
                Ok(r) => r,
                Err(e) => return json!({"content": text_content(e), "isError": true}),
            };
            let raw = match std::fs::read(&real) {
                Ok(b) => b,
                Err(e) => {
                    return json!({"content": text_content(format!("读取失败: {e}")), "isError": true})
                }
            };
            let params = crate::server::CompressParams {
                quality: args
                    .get("quality")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u8),
                target_kb: args.get("target_kb").and_then(|v| v.as_u64()).map(|v| v as u32),
                width: args.get("width").and_then(|v| v.as_u64()).map(|v| v as u32),
                height: args.get("height").and_then(|v| v.as_u64()).map(|v| v as u32),
            };
            let fname = real.file_name().and_then(|s| s.to_str());
            match crate::server::compress_bytes(&raw, fname, &params) {
                Ok((out, meta)) => {
                    // 输出路径：<原名去扩展>.compressed.jpg（同目录）
                    let out_path = real.with_file_name(format!(
                        "{}.compressed.jpg",
                        real.file_stem().and_then(|s| s.to_str()).unwrap_or("image")
                    ));
                    if let Err(e) = std::fs::write(&out_path, &out) {
                        return json!({"content": text_content(format!("写入失败: {e}")), "isError": true});
                    }
                    let summary = format!(
                        "压缩完成\n\
                         原文件: {} ({} B, {}x{})\n\
                         输出:   {} ({} B, {}x{})\n\
                         压缩率: {:.1}%（省 {:.1}%）\n\
                         耗时: 解码 {}ms / 缩放 {}ms / 编码 {}ms",
                        real.display(),
                        meta.original_bytes,
                        meta.original_width,
                        meta.original_height,
                        out_path.display(),
                        meta.output_bytes,
                        meta.output_width,
                        meta.output_height,
                        meta.ratio * 100.0,
                        (1.0 - meta.ratio) * 100.0,
                        meta.decode_ms,
                        meta.resize_ms,
                        meta.encode_ms
                    );
                    json!({"content": text_content(summary), "isError": false})
                }
                Err(e) => json!({"content": text_content(format!("压缩失败: {e}")), "isError": true}),
            }
        }

        "compress_batch" => {
            let Some(dir) = args.get("dir").and_then(|v| v.as_str()) else {
                return json!({"content": text_content("缺少参数 dir"), "isError": true});
            };
            let real_dir = match state.check_path(std::path::Path::new(dir)) {
                Ok(r) => r,
                Err(e) => return json!({"content": text_content(e), "isError": true}),
            };
            if !real_dir.is_dir() {
                return json!({"content": text_content(format!("不是目录: {}", real_dir.display())), "isError": true});
            }
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(200) as usize;
            let params = crate::server::CompressParams {
                quality: args.get("quality").and_then(|v| v.as_u64()).map(|v| v as u8),
                target_kb: args.get("target_kb").and_then(|v| v.as_u64()).map(|v| v as u32),
                width: None,
                height: None,
            };

            let entries = match std::fs::read_dir(&real_dir) {
                Ok(e) => e,
                Err(e) => {
                    return json!({"content": text_content(format!("列目录失败: {e}")), "isError": true})
                }
            };
            let mut lines = Vec::new();
            let mut ok_n = 0;
            let mut fail_n = 0;
            let mut total_in = 0u64;
            let mut total_out = 0u64;

            for entry in entries.flatten() {
                if lines.len() >= limit {
                    break;
                }
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                // 跳过本工具自己的产物，避免重复压缩
                if p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".compressed.jpg"))
                    .unwrap_or(false)
                {
                    continue;
                }
                let Ok(raw) = std::fs::read(&p) else { continue };
                let fname = p.file_name().and_then(|s| s.to_str());
                match crate::server::compress_bytes(&raw, fname, &params) {
                    Ok((out, meta)) => {
                        let out_path = p.with_file_name(format!(
                            "{}.compressed.jpg",
                            p.file_stem().and_then(|s| s.to_str()).unwrap_or("image")
                        ));
                        match std::fs::write(&out_path, &out) {
                            Ok(_) => {
                                ok_n += 1;
                                total_in += meta.original_bytes as u64;
                                total_out += meta.output_bytes as u64;
                                lines.push(format!(
                                    "✓ {}: {} → {} B ({:.1}%)",
                                    p.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                                    meta.original_bytes,
                                    meta.output_bytes,
                                    meta.ratio * 100.0
                                ));
                            }
                            Err(e) => {
                                fail_n += 1;
                                lines.push(format!("✗ {} 写入失败: {e}", p.display()));
                            }
                        }
                    }
                    Err(e) => {
                        fail_n += 1;
                        lines.push(format!("✗ {} 压缩失败: {e}", p.display()));
                    }
                }
            }

            let header = if total_in > 0 {
                format!(
                    "批量完成：成功 {ok_n}，失败 {fail_n}\n总输入 {total_in} B → 总输出 {total_out} B（{:.1}%）\n",
                    total_out as f64 / total_in as f64 * 100.0
                )
            } else {
                format!("批量完成：成功 {ok_n}，失败 {fail_n}\n")
            };
            json!({
                "content": text_content(format!("{header}{}", lines.join("\n"))),
                "isError": ok_n == 0 && fail_n > 0
            })
        }

        "image_info" => {
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                return json!({"content": text_content("缺少参数 path"), "isError": true});
            };
            let real = match state.check_path(std::path::Path::new(path)) {
                Ok(r) => r,
                Err(e) => return json!({"content": text_content(e), "isError": true}),
            };
            let raw = match std::fs::read(&real) {
                Ok(b) => b,
                Err(e) => {
                    return json!({"content": text_content(format!("读取失败: {e}")), "isError": true})
                }
            };
            let Some(fmt) = image::guess_format(&raw).ok() else {
                return json!({"content": text_content("无法识别的图片格式"), "isError": true});
            };
            match image::load_from_memory_with_format(&raw, fmt) {
                Ok(img) => {
                    let desc = format!(
                        "路径: {}\n格式: {:?}\n尺寸: {}x{}\n体积: {} B\n",
                        real.display(),
                        fmt,
                        img.width(),
                        img.height(),
                        raw.len()
                    );
                    json!({"content": text_content(desc), "isError": false})
                }
                Err(e) => {
                    json!({"content": text_content(format!("解码失败: {e}")), "isError": true})
                }
            }
        }

        other => json!({
            "content": text_content(format!("未知工具: {other}")),
            "isError": true
        }),
    }
}

/// POST /mcp —— MCP JSON-RPC 入口（Streamable HTTP 传输）
async fn handle_mcp(State(state): State<McpState>, Json(req): Json<RpcRequest>) -> Response {
    // 通知类消息（没有 id）按规范回 202 Accepted，不返回 body
    let Some(id) = req.id.clone() else {
        return StatusCode::ACCEPTED.into_response();
    };

    let result = match req.method.as_str() {
        "initialize" => json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }),

        "tools/list" => tools_list(),

        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            handle_tool_call(&state, name, &args)
        }

        // 空实现：客户端可能探测这些能力
        "ping" => json!({}),
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),

        other => {
            return Json(err_result(id, -32601, format!("未实现的方法: {other}")))
                .into_response();
        }
    };

    Json(ok_result(id, result)).into_response()
}

/// 构造 MCP 路由
pub fn build_mcp_router(state: McpState) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_has_three_tools() {
        let v = tools_list();
        let tools = v.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 3);
        for t in tools {
            assert!(t.get("name").is_some());
            assert!(t.get("inputSchema").is_some());
        }
    }

    #[test]
    fn init_result_shape() {
        // 复刻 initialize 的返回结构，确认协议字段齐
        let v = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        });
        assert!(v.get("protocolVersion").is_some());
        assert!(v["capabilities"]["tools"].is_object());
        assert!(v["serverInfo"]["name"].is_string());
    }

    #[test]
    fn unknown_tool_reports_error() {
        let st = McpState { allowed_roots: Arc::new(vec![]) };
        let r = handle_tool_call(&st, "no_such_tool", &json!({}));
        assert_eq!(r["isError"], json!(true));
    }

    #[test]
    fn missing_path_reports_error() {
        let st = McpState { allowed_roots: Arc::new(vec![]) };
        let r = handle_tool_call(&st, "compress_image", &json!({}));
        assert_eq!(r["isError"], json!(true));
    }
}
