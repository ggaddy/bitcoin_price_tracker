pub(crate) const INDEX_HTML: &str = include_str!("ui.html");

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
