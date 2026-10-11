pub(crate) const INDEX_HTML: &str = include_str!("ui.html");

pub(crate) async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::HeaderValue;
    let is_api = request.uri().path().starts_with("/api/") || request.uri().path() == "/health";
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("content-security-policy", HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    // HTML and assets must revalidate so a deployment cannot mix UI versions.
    // The query version in ui.html also bypasses already-cached legacy assets.
    headers.insert(
        "cache-control",
        HeaderValue::from_static(if is_api { "no-store" } else { "no-cache" }),
    );
    response
}

// Weak validators describe embedded content across transport encodings (e.g. gzip).
// Hash once per process; these are cache validators, not integrity signatures.
fn embedded_asset(
    request: axum::http::HeaderMap,
    body: &'static str,
    content_type: &'static str,
    validator: &'static std::sync::OnceLock<axum::http::HeaderValue>,
) -> axum::response::Response {
    use axum::{
        http::{HeaderValue, StatusCode, header},
        response::IntoResponse,
    };
    use std::hash::{Hash, Hasher};
    let etag = validator.get_or_init(|| {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut hash);
        HeaderValue::from_str(&format!("W/\"{:016x}\"", hash.finish())).unwrap()
    });
    let opaque = etag.to_str().unwrap().trim_start_matches("W/");
    let matches = request.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|tag| {
                let tag = tag.trim();
                tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == opaque
            })
        })
    });
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, content_type)], body).into_response()
    };
    response.headers_mut().insert(header::ETAG, etag.clone());
    response
}

pub(crate) async fn dashboard_css(headers: axum::http::HeaderMap) -> axum::response::Response {
    static ETAG: std::sync::OnceLock<axum::http::HeaderValue> = std::sync::OnceLock::new();
    embedded_asset(
        headers,
        include_str!("dashboard.css"),
        "text/css; charset=utf-8",
        &ETAG,
    )
}

pub(crate) async fn cybercore_css(headers: axum::http::HeaderMap) -> axum::response::Response {
    static ETAG: std::sync::OnceLock<axum::http::HeaderValue> = std::sync::OnceLock::new();
    embedded_asset(
        headers,
        include_str!("vendor/cybercore-0.3.0.min.css"),
        "text/css; charset=utf-8",
        &ETAG,
    )
}

pub(crate) async fn dashboard_js(headers: axum::http::HeaderMap) -> axum::response::Response {
    static ETAG: std::sync::OnceLock<axum::http::HeaderValue> = std::sync::OnceLock::new();
    embedded_asset(
        headers,
        include_str!("dashboard.js"),
        "text/javascript; charset=utf-8",
        &ETAG,
    )
}
