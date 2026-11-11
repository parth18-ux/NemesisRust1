//! Vercel entry point (Rust runtime): the whole RuseTect API and its pages as one function.
//!
//! The models, pages and fixtures are embedded in the binary and unpacked to /tmp on a cold start, so the function
//! does not depend on which repository files Vercel bundles. State is in memory per instance: demo state such as the
//! console replay can reset between requests, as it did on the Python deployment.
use tower::ServiceBuilder;
use vercel_runtime::axum::VercelLayer;
use vercel_runtime::Error;

macro_rules! assets {
    ($($p:literal),* $(,)?) => { &[$(($p, include_bytes!(concat!("../", $p)) as &[u8])),*] };
}

const ASSETS: &[(&str, &[u8])] = assets![
    "service/models/cascade_config.json", "service/models/l0_config.json", "service/models/l1_gatekeeper.json",
    "service/models/l2_xgb.json", "service/models/l2_lgbm.json", "service/models/l0_onboarding_lgbm.json",
    "service/models/message_model.json", "service/models/typing_verifier.json", "service/models/benchmark_results.json",
    "service/models/sample_applications.json", "service/models/demo_stream.csv.gz",
    "service/static/index.html", "service/static/console.html", "service/static/investigate.html", "service/static/report.html", "service/static/research.html",
    "service/static/favicon.svg", "fixtures/mule_replay.json", "results/message_model_eval.json",
];

fn unpack() -> std::io::Result<std::path::PathBuf> {
    let root = std::env::temp_dir().join("rusetect");
    for (path, bytes) in ASSETS {
        let dst = root.join(path);
        if dst.metadata().map(|m| m.len() == bytes.len() as u64).unwrap_or(false) {
            continue;
        }
        std::fs::create_dir_all(dst.parent().unwrap())?;
        std::fs::write(dst, bytes)?;
    }
    Ok(root)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::var("RUSETECT_ROOT").is_err() {
        std::env::set_var("RUSETECT_ROOT", unpack()?);
    }
    let shared = tokio::task::spawn_blocking(rusetect_server::new_shared).await?;
    let app = ServiceBuilder::new().layer(VercelLayer::new()).service(rusetect_server::build_app(shared));
    vercel_runtime::run(app).await
}
