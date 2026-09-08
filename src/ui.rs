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
    if is_api {
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
    }
    response
}

pub(crate) async fn dashboard_css() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("dashboard.css"),
    )
}

pub(crate) async fn cybercore_css() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("vendor/cybercore-0.3.0.min.css"),
    )
}

pub(crate) async fn dashboard_js() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        include_str!("dashboard.js"),
    )
}
