use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use hop_rip::server::{router, App};
use hop_rip::store::Store;

fn app(dir: &std::path::Path) -> axum::Router {
    router(App { store: Store::open(dir).unwrap(), base_url: Some("https://hop.test".into()) })
}

async fn send(app: &axum::Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let location = res.headers().get("location").map(|v| v.to_str().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), location)
}

#[tokio::test]
async fn full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let (s, v, _) = send(&app, "POST", "/api/links", None, Some(json!({"url": "https://example.com/a", "slug": "Bowl"}))).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["slug"], "bowl");
    assert_eq!(v["short_url"], "https://hop.test/bowl");
    let key = v["edit_key"].as_str().unwrap().to_string();

    let (s, _, _) = send(&app, "POST", "/api/links", None, Some(json!({"url": "https://example.com/b", "slug": "bowl"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    let (s, _, loc) = send(&app, "GET", "/bowl", None, None).await;
    assert_eq!(s, StatusCode::FOUND);
    assert_eq!(loc.as_deref(), Some("https://example.com/a"));

    let (s, v, _) = send(&app, "GET", "/api/links/bowl", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v.get("edit_key").is_none() && v.get("key_hash").is_none());

    let (s, _, _) = send(&app, "PUT", "/api/links/bowl", None, Some(json!({"url": "https://example.com/c"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = send(&app, "PUT", "/api/links/bowl", Some("nope"), Some(json!({"url": "https://example.com/c"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v, _) = send(&app, "PUT", "/api/links/bowl", Some(&key), Some(json!({"url": "https://example.com/c"}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["url"], "https://example.com/c");

    let (s, _, _) = send(&app, "DELETE", "/api/links/bowl", Some("nope"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(&app, "DELETE", "/api/links/bowl", Some(&key), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = send(&app, "GET", "/bowl", None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn random_slugs_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    let (s, v, _) = send(&app, "POST", "/api/links", None, Some(json!({"url": "https://example.com/"}))).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["slug"].as_str().unwrap().len(), 5);

    for bad in ["javascript:alert(1)", "ftp://x.com/", "not a url", "/relative"] {
        let (s, _, _) = send(&app, "POST", "/api/links", None, Some(json!({"url": bad}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    for bad in ["api", "../etc", "a.b", "has space"] {
        let (s, _, _) = send(&app, "POST", "/api/links", None, Some(json!({"url": "https://example.com/", "slug": bad}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (s, _, _) = send(&app, "GET", "/healthz", None, None).await;
    assert_eq!(s, StatusCode::OK);
}
