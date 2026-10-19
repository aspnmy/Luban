# Luban_rust 分支

本分支为 **LLM 网关提供图片压缩的 MCP 服务**而生：网关侧的模型读到图片后需要回调压缩，这里就是那个压缩后端。

算法取自鲁班（Luban）作者的反向推算压缩策略，用 Rust 重构实现。因为是给 LLM / MCP 调用，**不还原安卓侧的接口**——没有 Gradle 依赖、没有 Java/Kotlin API、没有示例 App，只对外提供 HTTP API 与 MCP 服务。

移植自本仓 `master` 分支的 Kotlin 实现，逐条对齐：短边 1440 基准、超长图反转、超大图短边降 1/4、不放大、1024 万像素上限、偶数对齐、体积经验系数、JPEG 质量二分搜索。

工程位于本分支 `rust/` 子目录（Cargo 工程 + Dockerfile）。

当前版本：**0.2.3**

## 致敬与出处

> **本项目致敬 [Curzibn/Luban](https://github.com/Curzibn/Luban)（鲁班）的原作者。**
> 鲁班的「仿微信朋友圈」反向推算压缩策略，是本项目全部算法思路的来源。没有原作者把这套策略开源，就没有这个 Rust 版本。

- **算法出处**：压缩策略源自原作者开源的 **Curzibn/Luban**，本分支移植自本仓库 `master` 分支的 Kotlin 实现
- **版权归属**：本分支的代码、文档与产物版权归本仓维护者——实现语言、部署形态、对外接口（HTTP / MCP）均已重写，是独立作品，并非上游代码的衍生
- 沿用上游 Apache-2.0 许可发布，著作权仍归本仓维护者

---

## 目录结构

```
rust/
├── Cargo.toml
├── Dockerfile
└── src/
    ├── main.rs       启动入口
    ├── lib.rs        模块导出
    ├── compress.rs   算法内核：目标尺寸计算 / 解码 / 缩放 / EXIF 转正 / JPEG 与 PNG 编码
    ├── server.rs     HTTP API（axum）
    ├── mcp.rs        MCP 服务（Streamable HTTP）
    ├── webui.rs      内置 WebUI 路由（页面经 include_str! 编进二进制）
    └── webui.html    内置 WebUI 单页（自包含，无外部资源）
```
## 压缩举例
![压缩质量35](images/17924256525210.png)

![压缩前](images/vlcsnap-2026-10-19-23h47m22s024.png)
![压缩后](images/v_0001.mp4_thumb.jpg)
## 压缩策略（与 master 一致）

| 规则 | 说明 |
|---|---|
| 基准短边 | 1440 |
| 超长图 | 长边 ≥ 一定值且宽高比 > 阈值时，改为**长边**定 1440 |
| 超大图 | 像素数超过阈值时，短边降到原短边的 1/4 |
| 不放大 | 目标尺寸不会超过原图 |
| 像素上限 | 目标像素总数封顶 |
| 偶数对齐 | 宽高一律取偶数，最小 2 |
| 体积估算 | 目标像素数 × 经验系数（四档） |
| 极窄长图 | 保底目标体积，避免压得过狠 |
| JPEG 质量搜索 | 先试 95，超标则在 [5, 95] 二分找「仍不超目标」的最大质量 |
| 缩放滤波 | Lanczos3 |
| EXIF | 8 种 Orientation 全部转正 |

### PNG 透明通道（`focus_alpha`）

对应原 Luban 的 `setFocusAlpha` 语义，**不是「输出 PNG」的开关**：

| 条件 | 输出 |
|---|---|
| PNG 源 + 确实带 alpha + `focus_alpha=true` | **PNG**，保留透明通道 |
| PNG 带 alpha，但 `focus_alpha` 未开 | JPEG |
| PNG 但全不透明（`focus_alpha` 无效） | JPEG |
| 非 PNG 源 | JPEG |
| 指定了固定 `quality` | JPEG |

PNG 没有质量参数，因此改为按压缩级别（Best / Default / Fast）搜索目标体积。

## HTTP API

服务默认监听 `0.0.0.0:8900`（可用环境变量 `HOST` / `PORT` 覆盖）。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/` | 内置 WebUI 页面（自包含单页，直接调用本服务 `/compress`） |
| GET | `/index.html` | 同上 |
| GET | `/health` | 健康检查，返回服务名与版本 |
| GET | `/info` | 运行信息 |
| POST | `/compress` | multipart 上传图片，返回压缩后的文件流 |
| POST | `/compress/base64` | JSON 传 base64 图片，返回 base64 结果 |

### 查询参数

| 参数 | 类型 | 说明 |
|---|---|---|
| `quality` | 1-100 | 固定质量，给定则跳过体积搜索 |
| `target_kb` | 整数 | 强制目标体积，不传则按算法估算 |
| `width` | 整数 | 强制目标宽度 |
| `height` | 整数 | 强制目标高度 |
| `focus_alpha` | 布尔 | 集中压缩透明通道，默认 `false` |

响应头会带上 `x-luban-format`（`jpeg` / `png`）与各阶段耗时。

### 示例

```sh
# 健康检查
curl http://127.0.0.1:8900/health

# 上传压缩
curl -X POST "http://127.0.0.1:8900/compress?focus_alpha=true" \
     -F "file=@/path/to/image.png" -o out.png

# base64 方式
curl -X POST "http://127.0.0.1:8900/compress/base64?quality=80" \
     -H 'Content-Type: application/json' \
     -d '{"data":"<base64>","filename":"a.jpg"}'
```

## WebUI

镜像内置一个自包含单页，浏览器直接打开 `http://<host>:8900/` 即可用，**不需要额外部署前端、不依赖 CDN**。

- 页面对接的是**本服务自身的 `POST /compress`**（multipart），不引入第二个后端
- 支持固定质量 / 目标体积 / 强制宽高 / `focus_alpha` 四个参数
- 展示压缩前后体积、输出/原始比例、尺寸、格式与编码耗时，可分别下载原图与结果
- **不写服务器磁盘**：页面只接收接口返回的字节流，与 `LUBAN_ALLOWED_ROOTS` 无关（配了也不会被 WebUI 触发落盘）

页面随二进制一起发布（`include_str!`），改页面须重新编译镜像。

## MCP 服务

端点：`POST /mcp`，遵循 MCP 2024-11-05 Streamable HTTP（JSON-RPC 2.0 over POST，**无状态**，不维护会话 id）。

### 工具列表

| 工具 | 说明 |
|---|---|
| `compress_image` | 压缩单张图片，结果写入同目录 `<原名>.compressed.jpg` / `.compressed.png`，**不覆盖原文件** |
| `compress_batch` | 批量压缩某个目录（`limit` 默认 20、上限 200），自动跳过已压缩产物 |
| `image_info` | 只读查询：格式 / 尺寸 / 体积 |

三个工具一律收**路径**，不使用 base64 上传（大图塞不进模型上下文）。

`compress_image` 支持 `quality` / `target_kb` / `width` / `height` / `focus_alpha` 参数。

### 磁盘缓存开关 `LUBAN_ALLOWED_ROOTS`

服务默认**不落盘**：压缩结果直接从接口返回，服务器不留任何图片副本。

| 配置 | 行为 |
|---|---|
| **未配置**（默认） | `compress_image` 在 `content` 里返回图片内容（base64）+ 摘要；本地不产生文件。`compress_batch` 返回错误，提示改用单张 |
| **已配置**（冒号分隔的目录） | 启用磁盘模式：路径须落在这些根目录内，压缩结果写为 `<原名>.compressed.jpg/.png`，返回文件路径 + 摘要 |

典型用法：普通/免费调用不配置（不为服务器制造图片副本）；需要为客户保留备份兜底时再配置（VIP 场景）。

> ⚠️ **两种部署形式不可混用，先确认你要哪一种再起容器：**
>
> | 用途 | 必须做的 | 漏了会怎样 |
> |---|---|---|
> | **只做 API / MCP 转压（默认，推荐）** | 不设 `LUBAN_ALLOWED_ROOTS`，也**不用挂载任何图片目录** | 无影响；结果始终从接口返回 |
> | **需要服务器缓存 / 客户端兜底回取（VIP）** | ① 设 `LUBAN_ALLOWED_ROOTS`；② **同时把这几个目录挂进容器**；③ 目录须对容器用户 `uid 10001` 可写 | 缺②→ 容器内看不到图，报「路径不可用」；缺③ 或属主不对→ 报 `Permission denied`，压缩结果写不进去 |
>
> **调用方式也随之不同：**
> - 不落盘模式：模型/客户端**一次性拿到图片内容**，自己保存；服务端无任何残留，重复调用会重新压缩。
> - 磁盘模式：调用后要**自行去白名单目录取回** `<原名>.compressed.jpg`；同一张图重复调用会覆盖上一次的产物（不加序号、不留历史版本）。
>
> **不要把两种混着用**：例如只设了 `LUBAN_ALLOWED_ROOTS` 而没挂载目录、或挂了目录却忘了设变量——前者会让所有路径校验失败，后者会让批量工具直接报「需要配置 LUBAN_ALLOWED_ROOTS」。

## 部署

```sh
# 构建
docker build -t luban-api:0.2.3 rust/

# 运行（默认：不落盘）
docker run -d --name luban-api --restart unless-stopped \
  -p 8900:8900 -e HOST=0.0.0.0 -e PORT=8900 \
  luban-api:0.2.3

# 运行（磁盘模式：额外加挂载与白名单）
docker run -d --name luban-api --restart unless-stopped \
  -p 8900:8900 \
  -v /your/images:/your/images \
  -e LUBAN_ALLOWED_ROOTS=/your/images \
  -e HOST=0.0.0.0 -e PORT=8900 \
  luban-api:0.2.3
```

镜像已推送至内网私库：`10.168.3.165:5000/aspnmy/luban-api:0.2.3`（同 `latest`）。

## 环境变量

| 变量 | 说明 |
|---|---|
| `HOST` | 监听地址，默认 `0.0.0.0` |
| `PORT` | 监听端口，默认 `8900` |
| `LUBAN_ALLOWED_ROOTS` | 磁盘缓存开关。留空 = 不落盘（默认）；填目录 = 启用磁盘模式，多个用冒号分隔 |
| `TZ` | 时区 |

## 开发与验证

```sh
# 单元测试
cargo test

# 本地运行
cargo run --release
```

## 版本历史

| 版本 | 变更 |
|---|---|
| 0.2.3 | 内置 WebUI 单页（`GET /`、`/index.html`，随二进制发布、零外部资源、直连自身 `/compress`、不落盘） |
| 0.2.2 | `LUBAN_ALLOWED_ROOTS` 改为可选磁盘缓存开关：留空 ⇒ MCP 直接返回图片字节、服务器不落盘；配置 ⇒ 落盘白名单目录。编排文件改为可选挂载 |
| 0.2.1 | 还原 `setFocusAlpha` 语义：PNG 透明通道集中压缩（带 alpha 的 PNG 输出 PNG）、`focus_alpha` 参数、MCP/HTTP 双通道支持 |
| 0.2.0 | MCP Streamable HTTP 三工具；路径白名单；缩放分支实测通过 |
| 0.1.0 | 算法内核移植与基础 HTTP API |

---
