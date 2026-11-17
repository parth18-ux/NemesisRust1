//! The app shell (`/app`) and its embedded files (`/assets/...`): each is served with its type, the shell carries the
//! content security policy, and the route serves nothing outside the embedded list.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let r = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
    let (status, headers) = (r.status(), r.headers().clone());
    let body = r.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn app_shell_and_assets() {
    std::env::set_var("RUSETECT_RATE_LIMIT", "1000000");
    let app = rusetect_server::build_app(rusetect_server::new_shared());

    let (status, h, html) = get(&app, "/app").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["content-security-policy"], rusetect_server::APP_CSP);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(html.contains("<title>RuseTect Workspace</title>") && html.contains("part of NEMESIS"));
    // the policy allows no inline code, so the shell must carry none
    assert!(!html.contains("<style") && !html.contains("style=\"") && !html.contains("onclick="));
    assert!(html.matches("<script").count() == html.matches("<script src=").count() + html.matches("<script type=\"module\" src=").count());
    // the three font families are unchanged
    for font in ["Instrument+Serif", "Geist:wght@400;500;600", "Geist+Mono"] {
        assert!(html.contains(font), "{font}");
    }

    let assets: Vec<_> = rusetect_server::app_assets().collect();
    assert!(assets.len() >= 7, "{assets:?}");
    for (path, ctype) in &assets {
        let (status, h, body) = get(&app, &format!("/assets/{path}")).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(h["content-type"], *ctype, "{path}");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert!(!body.is_empty(), "{path}");
        // every module a file imports is itself embedded
        for imp in body.split("from \"./").skip(1).chain(body.split("import(\"./").skip(1)) {
            let name = format!("js/{}", imp.split('"').next().unwrap());
            assert!(assets.iter().any(|(p, _)| *p == name), "{path} imports {name}, which is not embedded");
        }
    }
    // every file the shell links is embedded
    for link in html.split("=\"/assets/").skip(1) {
        let name = link.split('"').next().unwrap();
        assert!(assets.iter().any(|(p, _)| *p == name), "{name}");
    }

    for bad in ["/assets/nope.js", "/assets/../index.html", "/assets/%2e%2e/index.html", "/assets/js/../../../models/cascade_config.json", "/assets/"] {
        assert_eq!(get(&app, bad).await.0, StatusCode::NOT_FOUND, "{bad}");
    }
    // the older pages keep inline code, so they must not get the app's policy
    assert!(get(&app, "/console").await.1.get("content-security-policy").is_none());
}
