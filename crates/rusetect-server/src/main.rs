//! RuseTect API server.
//!   rusetect-server [--host 0.0.0.0] [--port 8000]      (or HOST / PORT environment variables)
//! Reads models and pages from the repository root (RUSETECT_ROOT, or found from the working directory).
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rusetect_server::check_config()?;
    rusetect_server::ensure_root()?;
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let host = arg("--host").or_else(|| std::env::var("HOST").ok()).unwrap_or_else(|| "0.0.0.0".into());
    let port: u16 = arg("--port").or_else(|| std::env::var("PORT").ok()).and_then(|p| p.parse().ok()).unwrap_or(8000);
    let shared = tokio::task::spawn_blocking(rusetect_server::new_shared).await?;
    let app = rusetect_server::build_app(shared);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("{} API listening on http://{addr}", rusetect::branding::NAME);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await?;
    Ok(())
}
