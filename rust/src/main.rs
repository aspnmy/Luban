//! luban-api 服务入口

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 环境变量：HOST / PORT（与 ok-browser 保持一致的约定）
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8900);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!(
        "luban-api {} 已启动 → http://{}（请求体上限 {} MB）",
        env!("CARGO_PKG_VERSION"),
        addr,
        luban_api::server::MAX_BODY_BYTES / 1024 / 1024
    );

    axum::serve(listener, luban_api::server::build_router())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// 收到 SIGINT / SIGTERM 时优雅退出（容器 stop 不会硬杀在途压缩）
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到退出信号，正在关闭…");
}
