//! HTTP API, web app and redirects.
//!
//!   GET    /                 web app
//!   POST   /api/links        {url, slug?} -> 201 {slug, url, short_url, edit_key, ...}
//!   GET    /api/links/:slug  -> {slug, url, short_url, created, updated}
//!   PUT    /api/links/:slug  {url}, edit key required -> 200
//!   DELETE /api/links/:slug  edit key required -> 204
//!   GET    /:slug            302 to the long URL
//!
//! The edit key comes back once, on create. It's the only auth: send it as
//! `Authorization: Bearer <key>` (or `X-Edit-Key: <key>`) to edit or delete that link.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;

use crate::store::{sha256_hex, CreateError, Link, Store};

const INDEX: &str = include_str!("../static/index.html");
const MAX_URL_LEN: usize = 4096;
const RESERVED: &[&str] = &["api", "healthz", "static", "assets", "new", "www", "admin"];

pub struct App {
    pub store: Store,
    /// e.g. "https://hop.rip". Without it, short URLs use the request's Host header.
    pub base_url: Option<String>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/links", axum::routing::post(create))
        .route("/api/links/:slug", get(show).put(update).delete(remove))
        .route("/:slug", get(redirect))
        .fallback(|| async { not_found() })
        .with_state(Arc::new(app))
}

type Shared = State<Arc<App>>;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    eprintln!("hop: storage error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Html("<!doctype html><title>Not found</title><p>No such hop. <a href=\"/\">Make one</a>.</p>"))
        .into_response()
}

/// Lowercase letters, digits, `-` and `_`, 1 to 64 characters, not reserved.
pub fn valid_slug(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !RESERVED.contains(&s)
}

fn valid_url(s: &str) -> Result<String, &'static str> {
    if s.len() > MAX_URL_LEN {
        return Err("url is too long");
    }
    let u = url::Url::parse(s.trim()).map_err(|_| "url is not a valid absolute URL")?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("url must be http(s) with a host");
    }
    Ok(u.to_string())
}

fn random_slug(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789"; // no l, o, 0, 1
    let mut rng = rand::thread_rng();
    (0..len).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

fn new_key() -> String {
    let bytes: [u8; 24] = rand::thread_rng().gen();
    hex::encode(bytes)
}

fn short_url(app: &App, headers: &HeaderMap, slug: &str) -> String {
    let base = match &app.base_url {
        Some(b) => b.trim_end_matches('/').to_string(),
        None => {
            let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
            format!("http://{host}")
        }
    };
    format!("{base}/{slug}")
}

fn presented_key(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let x = headers.get("x-edit-key").and_then(|h| h.to_str().ok());
    bearer.or(x).map(|k| k.trim().to_string())
}

fn link_json(app: &App, headers: &HeaderMap, l: &Link) -> serde_json::Value {
    json!({ "slug": l.slug, "url": l.url, "short_url": short_url(app, headers, &l.slug),
            "created": l.created, "updated": l.updated })
}

#[derive(Deserialize)]
struct CreateBody {
    url: String,
    slug: Option<String>,
}

async fn create(State(app): Shared, headers: HeaderMap, Json(body): Json<CreateBody>) -> Response {
    let url = match valid_url(&body.url) {
        Ok(u) => u,
        Err(m) => return err(StatusCode::BAD_REQUEST, m),
    };
    let custom = body.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_lowercase);
    if let Some(s) = &custom {
        if !valid_slug(s) {
            return err(StatusCode::BAD_REQUEST, "slug must be 1-64 of a-z, 0-9, - and _ (and not reserved)");
        }
    }
    let key = new_key();
    let t = now();
    for attempt in 0..12 {
        let slug = match &custom {
            Some(s) => s.clone(),
            None => random_slug(if attempt < 8 { 5 } else { 8 }),
        };
        let link = Link { slug, url: url.clone(), key_hash: sha256_hex(&key), created: t, updated: t };
        match app.store.create(&link) {
            Ok(()) => {
                let mut v = link_json(&app, &headers, &link);
                v["edit_key"] = json!(key);
                return (StatusCode::CREATED, Json(v)).into_response();
            }
            Err(CreateError::Exists) if custom.is_some() => return err(StatusCode::CONFLICT, "that slug is taken"),
            Err(CreateError::Exists) => continue,
            Err(CreateError::Io(e)) => return internal(e),
        }
    }
    err(StatusCode::SERVICE_UNAVAILABLE, "could not find a free slug, try again")
}

/// Loads a link and checks the presented edit key against it.
fn authorized(app: &App, headers: &HeaderMap, slug: &str) -> Result<Link, Response> {
    let link = match app.store.get(slug) {
        Ok(Some(l)) => l,
        Ok(None) => return Err(err(StatusCode::NOT_FOUND, "no such link")),
        Err(e) => return Err(internal(e)),
    };
    match presented_key(headers) {
        None => Err(err(StatusCode::UNAUTHORIZED, "edit key required (Authorization: Bearer <key>)")),
        Some(k) if sha256_hex(&k) == link.key_hash => Ok(link),
        Some(_) => Err(err(StatusCode::FORBIDDEN, "wrong edit key")),
    }
}

async fn show(State(app): Shared, headers: HeaderMap, Path(slug): Path<String>) -> Response {
    let slug = slug.to_lowercase();
    if !valid_slug(&slug) {
        return err(StatusCode::NOT_FOUND, "no such link");
    }
    match app.store.get(&slug) {
        Ok(Some(l)) => Json(link_json(&app, &headers, &l)).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such link"),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct UpdateBody {
    url: String,
}

async fn update(State(app): Shared, headers: HeaderMap, Path(slug): Path<String>, Json(body): Json<UpdateBody>) -> Response {
    let slug = slug.to_lowercase();
    if !valid_slug(&slug) {
        return err(StatusCode::NOT_FOUND, "no such link");
    }
    let mut link = match authorized(&app, &headers, &slug) {
        Ok(l) => l,
        Err(r) => return r,
    };
    link.url = match valid_url(&body.url) {
        Ok(u) => u,
        Err(m) => return err(StatusCode::BAD_REQUEST, m),
    };
    link.updated = now();
    match app.store.replace(&link) {
        Ok(()) => Json(link_json(&app, &headers, &link)).into_response(),
        Err(e) => internal(e),
    }
}

async fn remove(State(app): Shared, headers: HeaderMap, Path(slug): Path<String>) -> Response {
    let slug = slug.to_lowercase();
    if !valid_slug(&slug) {
        return err(StatusCode::NOT_FOUND, "no such link");
    }
    if let Err(r) = authorized(&app, &headers, &slug) {
        return r;
    }
    match app.store.delete(&slug) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal(e),
    }
}

async fn redirect(State(app): Shared, Path(slug): Path<String>) -> Response {
    let slug = slug.to_lowercase();
    if !valid_slug(&slug) {
        return not_found();
    }
    match app.store.get(&slug) {
        Ok(Some(l)) => (StatusCode::FOUND, [(header::LOCATION, l.url), (header::CACHE_CONTROL, "no-store".into())]).into_response(),
        Ok(None) => not_found(),
        Err(e) => internal(e),
    }
}
