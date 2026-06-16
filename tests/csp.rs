//! `csp` feature: the per-request [`CspNonceLayer`] middleware.
//!
//! Driven via `tower::ServiceExt::oneshot` — no port binding.
#![cfg(feature = "csp")]

use axum::{
    body::Body,
    extract::Extension,
    http::{header, Request},
    response::Html,
    routing::get,
    Router,
};
use tower::ServiceExt;
use web_modules::{CspNonce, CspNonceLayer};

/// Pull the nonce value out of a `… 'nonce-<value>' …` policy string.
fn nonce_from_csp(csp: &str) -> String {
    let start = csp.find("'nonce-").expect("a nonce source") + "'nonce-".len();
    let rest = &csp[start..];
    let end = rest.find('\'').expect("a closing quote");
    rest[..end].to_string()
}

async fn body_string(res: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn sets_header_and_substitutes_nonce() {
    let app = Router::new()
        .route("/", get(|| async { Html("<html></html>") }))
        .layer(CspNonceLayer::new(
            "default-src 'none'; script-src 'self' 'nonce-{nonce}'",
        ));

    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let csp = res
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .expect("CSP header set")
        .to_str()
        .unwrap();
    assert!(
        !csp.contains("{nonce}"),
        "placeholder not substituted: {csp}"
    );
    let nonce = nonce_from_csp(csp);
    assert_eq!(nonce.len(), 32, "16 random bytes → 32 hex chars");
    assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn nonce_is_unique_per_request() {
    let app = Router::new()
        .route("/", get(|| async { "x" }))
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'"));

    let header_nonce = |res: &axum::response::Response| {
        nonce_from_csp(
            res.headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap(),
        )
    };

    let one = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let two = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_ne!(header_nonce(&one), header_nonce(&two));
}

#[tokio::test]
async fn nonce_in_extensions_matches_header() {
    async fn handler(Extension(nonce): Extension<CspNonce>) -> String {
        nonce.as_str().to_string()
    }
    let app = Router::new()
        .route("/", get(handler))
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'"));

    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let header_nonce = nonce_from_csp(
        res.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let body_nonce = body_string(res).await;
    assert_eq!(
        body_nonce, header_nonce,
        "the extension nonce must match the header nonce"
    );
}

#[tokio::test]
async fn rewrites_sentinel_in_html_body() {
    let app = Router::new()
        .route(
            "/",
            get(|| async { Html("<script nonce=\"__CSP_NONCE__\">1</script>") }),
        )
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'").rewrite_html_body());

    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let nonce = nonce_from_csp(
        res.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let body = body_string(res).await;
    assert!(
        body.contains(&format!("nonce=\"{nonce}\"")),
        "body = {body}"
    );
    assert!(!body.contains("__CSP_NONCE__"));
}

#[tokio::test]
async fn does_not_rewrite_non_html_body() {
    // A JS asset that happens to contain the sentinel text must pass through unchanged.
    let app = Router::new()
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    "const marker = '__CSP_NONCE__';",
                )
            }),
        )
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'").rewrite_html_body());

    let res = app
        .oneshot(Request::get("/app.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = body_string(res).await;
    assert!(body.contains("__CSP_NONCE__"), "non-HTML must be untouched");
}

#[tokio::test]
async fn leaves_sentinel_when_rewrite_not_enabled() {
    let app = Router::new()
        .route(
            "/",
            get(|| async { Html("<script nonce=\"__CSP_NONCE__\"></script>") }),
        )
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'")); // no .rewrite_html_body()

    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = body_string(res).await;
    assert!(
        body.contains("__CSP_NONCE__"),
        "sentinel left intact unless rewriting is opted into"
    );
}

#[tokio::test]
async fn nested_layers_share_one_nonce() {
    // Two stacked layers (e.g. merged/nested routers) must agree on a single nonce, so the
    // header value still matches the value written into the body.
    let app = Router::new()
        .route(
            "/",
            get(|| async { Html("<script nonce=\"__CSP_NONCE__\"></script>") }),
        )
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'").rewrite_html_body())
        .layer(CspNonceLayer::new("script-src 'nonce-{nonce}'").rewrite_html_body());

    let res = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let nonce = nonce_from_csp(
        res.headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap(),
    );
    let body = body_string(res).await;
    assert!(
        body.contains(&format!("nonce=\"{nonce}\"")),
        "body = {body}"
    );
    assert!(!body.contains("__CSP_NONCE__"));
}
