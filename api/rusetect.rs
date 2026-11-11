//! Vercel entry point (Rust runtime): the whole RuseTect API and its pages as one function.
//!
//! The models, pages and fixtures are embedded in the server library (`rusetect_server::ensure_root`) and unpacked
//! to /tmp on a cold start, so the function does not depend on which repository files Vercel bundles. State is in
//! memory per instance: demo state such as the console replay can reset between requests.
use tower::ServiceBuilder;
use vercel_runtime::axum::VercelLayer;
use vercel_runtime::Error;

#[tokio::main]
async fn main() -> Result<(), Error> {
    rusetect_server::ensure_root()?;
    let shared = tokio::task::spawn_blocking(rusetect_server::new_shared).await?;
    let app = ServiceBuilder::new().layer(VercelLayer::new()).service(rusetect_server::build_app(shared));
    vercel_runtime::run(app).await
}
