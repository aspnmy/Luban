# Luban_rust Branch

This branch exists to **provide an image-compression MCP service for LLM gateways**: when a model in the gateway pipeline receives an image and needs it compressed, this service is the backend it calls.

The algorithm is derived from the reverse-engineered compression strategy of Luban (鲁班) by the original author, reimplemented in Rust. Because the consumer is an LLM / MCP client, the **Android-side interfaces are intentionally not reproduced** — there is no Gradle dependency, no Java/Kotlin API, no sample app. Only an HTTP API and an MCP service are exposed.

Ported from the Kotlin implementation on this repository's `master` branch, aligned point by point: 1440 short-edge baseline, long-image reversal, 1/4 short-edge reduction for huge images, no upscaling, 10.24M pixel cap, even-number alignment, empirical size coefficients, and JPEG quality binary search.

The project lives in the `rust/` subdirectory of this branch (Cargo project + Dockerfile).

Current version: **0.2.1**

## Tribute and Provenance

> **This project pays tribute to the original author of [Curzibn/Luban](https://github.com/Curzibn/Luban).**
> Luban's reverse-engineered "WeChat Moments–style" compression strategy is the source of every algorithmic idea in this project. Without the original author open-sourcing that strategy, this Rust version would not exist.

- **Algorithm provenance**: the compression strategy originates from the open-sourced **Curzibn/Luban**, and this branch ports it from the Kotlin implementation on this repository's `master` branch
- **Copyright**: the code, documentation, and artifacts on this branch are copyrighted by the maintainers of this repository — the implementation language, deployment shape, and public interfaces (HTTP / MCP) have all been rewritten, making it an independent work rather than a derivative of upstream code
- Released under the upstream Apache-2.0 license; copyright remains with the maintainers of this repository

---

## Layout

```
rust/
├── Cargo.toml
├── Dockerfile
└── src/
    ├── main.rs       entry point
    ├── lib.rs        module exports
    ├── compress.rs   algorithm core: target sizing / decode / resize / EXIF rotation / JPEG & PNG encoding
    ├── server.rs     HTTP API (axum)
    └── mcp.rs        MCP service (Streamable HTTP)
```

## Compression Strategy (aligned with master)

| Rule | Description |
|---|---|
| Baseline short edge | 1440 |
| Long image | when the long edge ≥ threshold and aspect ratio > threshold, the **long** edge is fixed at 1440 instead |
| Huge image | when pixel count exceeds the threshold, the short edge drops to 1/4 of the original |
| No upscaling | target dimensions never exceed the source |
| Pixel cap | total target pixels are capped |
| Even alignment | width and height are rounded to even numbers, minimum 2 |
| Size estimation | target pixels × empirical coefficient (four tiers) |
| Very narrow long image | a floor on target size, to avoid over-compression |
| JPEG quality search | try 95 first; if over target, binary search [5, 95] for the highest quality still under target |
| Resize filter | Lanczos3 |
| EXIF | all 8 orientation values are normalized |

### PNG alpha channel (`focus_alpha`)

Mirrors the original Luban `setFocusAlpha` semantics — **it is not a "output PNG" switch**:

| Condition | Output |
|---|---|
| PNG source + actually has alpha + `focus_alpha=true` | **PNG**, alpha preserved |
| PNG with alpha, but `focus_alpha` not enabled | JPEG |
| PNG but fully opaque (`focus_alpha` has no effect) | JPEG |
| Non-PNG source | JPEG |
| A fixed `quality` is specified | JPEG |

PNG has no quality parameter, so the target size is searched by compression level (Best / Default / Fast) instead.

## HTTP API

The service listens on `0.0.0.0:8900` by default (override with `HOST` / `PORT`).

| Method | Path | Description |
|---|---|---|
| GET | `/health` | health check, returns service name and version |
| GET | `/info` | runtime information |
| POST | `/compress` | upload an image as multipart, returns the compressed byte stream |
| POST | `/compress/base64` | base64 image in JSON, base64 result out |

### Query parameters

| Parameter | Type | Description |
|---|---|---|
| `quality` | 1-100 | fixed quality; skips the size search |
| `target_kb` | integer | force a target size; otherwise estimated by the algorithm |
| `width` | integer | force target width |
| `height` | integer | force target height |
| `focus_alpha` | boolean | compress the alpha channel; default `false` |

Responses carry `x-luban-format` (`jpeg` / `png`) and per-stage timings.

### Examples

```sh
# health check
curl http://127.0.0.1:8900/health

# upload and compress
curl -X POST "http://127.0.0.1:8900/compress?focus_alpha=true" \
     -F "file=@/path/to/image.png" -o out.png

# base64 mode
curl -X POST "http://127.0.0.1:8900/compress/base64?quality=80" \
     -H 'Content-Type: application/json' \
     -d '{"data":"<base64>","filename":"a.jpg"}'
```

## MCP Service

Endpoint: `POST /mcp`, following MCP 2024-11-05 Streamable HTTP (JSON-RPC 2.0 over POST, **stateless**, no session id).

### Tools

| Tool | Description |
|---|---|
| `compress_image` | compress a single image; writes `<name>.compressed.jpg` / `.compressed.png` next to the source, **never overwrites the original** |
| `compress_batch` | compress a whole directory (`limit` defaults to 20, max 200), skipping already-compressed outputs |
| `image_info` | read-only: format / dimensions / file size |

All three tools take a **path**, never a base64 upload (a full image does not fit in a model context).

`compress_image` accepts `quality` / `target_kb` / `width` / `height` / `focus_alpha`.

### Path allowlist

Every path argument is resolved (symlinks followed) and must fall under `LUBAN_ALLOWED_ROOTS` (colon-separated); otherwise it is rejected.

## Deployment

```sh
# build
docker build -t luban-api:0.2.1 rust/

# run
docker run -d --name luban-api --restart unless-stopped \
  -p 8900:8900 \
  -v /your/images:/your/images \
  -e LUBAN_ALLOWED_ROOTS=/your/images \
  -e HOST=0.0.0.0 -e PORT=8900 \
  luban-api:0.2.1
```

Image published to the internal registry: `10.168.3.165:5000/aspnmy/luban-api:0.2.1` (also `latest`).

## Environment variables

| Variable | Description |
|---|---|
| `HOST` | listen address, default `0.0.0.0` |
| `PORT` | listen port, default `8900` |
| `LUBAN_ALLOWED_ROOTS` | allowed root directories, colon-separated |
| `TZ` | timezone |

## Development

```sh
# unit tests
cargo test

# run locally
cargo run --release
```

## Changelog

| Version | Changes |
|---|---|
| 0.2.1 | restore `setFocusAlpha` semantics: PNG alpha-channel compression (PNG with alpha stays PNG), `focus_alpha` parameter, available over both MCP and HTTP |
| 0.2.0 | MCP Streamable HTTP with three tools; path allowlist; resize branch verified against real images |
| 0.1.0 | algorithm core port and basic HTTP API |

---
