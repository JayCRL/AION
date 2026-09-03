//! AION Web 服务：提供静态文件服务。

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use tower_http::cors::CorsLayer;

/// 启动 Web 服务
pub async fn run(port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/logo.png", get(logo_handler))
        .layer(CorsLayer::permissive());

    let addr = format!("0.0.0.0:{}", port);
    println!();
    println!("   ▄▄▄");
    println!("  ██████    AION Web UI");
    println!("  ██  ████   正在启动...");
    println!("   ▀████▀");
    println!();
    println!("  🌐 http://localhost:{}", port);
    println!();

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}

async fn logo_handler() -> Response {
    let bytes = include_bytes!("../static/logo.png");
    Response::builder()
        .header("Content-Type", "image/png")
        .body(axum::body::Body::from(bytes.to_vec()))
        .unwrap()
}
