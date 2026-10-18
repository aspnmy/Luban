# ⛔ 不加 `# syntax=docker/dockerfile:1` —— 该指令会让 BuildKit 去 docker.io 拉
#    dockerfile 前端（14MB），本机内网访问 docker.io 极慢（实测 100s+ 仍未完成）。
#    本文件只用 dockerfile 基础语法，不需要前端镜像。

# ── 构建阶段 ───────────────────────────────────────────────────────────────
# 内网 rust-builder（1.97.1）作构建载体 —— 与验收基准一致。
FROM 10.168.3.123:30002/aspnmy/rust-builder:v1.97.1-clippy AS builder

WORKDIR /build
COPY Cargo.toml ./
# 先编空壳以缓存依赖层（Cargo.toml 不变则依赖不重编）
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs \
 && cargo build --release 2>/dev/null || true
RUN rm -rf src

COPY src ./src
RUN cargo build --release --bin luban-api

# ── 运行阶段 ───────────────────────────────────────────────────────────────
# 纯 Rust 二进制但 glibc 动态链接 ⇒ 用 debian-slim 底座。
# ⚠️ 走内网 docker.io 代理通道（30002/docker-proxy-2），⛔ 不直连 docker.io。
FROM 10.168.3.123:30002/docker-proxy-2/library/debian:12-slim

ENV TZ=Asia/Shanghai
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && ln -snf /usr/share/zoneinfo/$TZ /etc/localtime \
 && echo "$TZ" > /etc/timezone \
 && rm -rf /var/lib/apt/lists/*

# 非 root 运行（与 ok-browser 的 okuser 对齐：uid 10001）
RUN useradd -u 10001 -m -s /usr/sbin/nologin luban

COPY --from=builder /build/target/release/luban-api /usr/local/bin/luban-api

ENV HOST=0.0.0.0
ENV PORT=8900

USER luban
EXPOSE 8900

# 健康检查：GET /health 返 200
# ⚠️ 镜像里没有 curl / wget（debian-slim 默认不带）⇒ 用 /dev/tcp 裸探测，
#    ⛔ 不要写 `luban-api --health`（该参数不存在，进程会直接报错退出）。
HEALTHCHECK --interval=30s --timeout=4s --start-period=5s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8900 && printf "GET /health HTTP/1.0\r\n\r\n" >&3 && head -1 <&3 | grep -q 200'

ENTRYPOINT ["/usr/local/bin/luban-api"]
