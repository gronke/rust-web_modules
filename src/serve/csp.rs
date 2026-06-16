//! Per-request **CSP nonce** middleware (feature `csp`).
//!
//! A fresh, unpredictable nonce is minted for every request, inserted into the request
//! extensions as [`CspNonce`] (so handlers and template renderers can read it), and used
//! to render a `Content-Security-Policy` response header from a policy *template*. This
//! lets you authorise inline `<script>` / `<style>` with `'nonce-…'` instead of
//! `'unsafe-inline'` — the difference between a CSP that actually constrains injected
//! script and one that doesn't.
//!
//! Two delivery paths, usable together:
//!
//! - **Dynamic markup** (a template engine rendering per request): read the nonce from
//!   the request extensions and emit `nonce="…"` on each inline tag. The same value is
//!   already in the response header.
//! - **Static markup** (embedded `include_dir!` trees baked at build time, which can't
//!   interpolate a per-request value): bake a fixed sentinel — [`DEFAULT_SENTINEL`] — into
//!   the markup (e.g. `nonce="__CSP_NONCE__"`) and enable
//!   [`rewrite_html_body`](CspNonceLayer::rewrite_html_body); the layer swaps the sentinel
//!   for the live nonce in `text/html` responses.
//!
//! Nonces apply only to `<script>` / `<style>` *elements* — not inline event-handler
//! attributes (`onclick="…"`). Those still need `'unsafe-inline'` (or removal), so a
//! nonce-only policy requires moving such handlers into nonce'd scripts.
//!
//! ```ignore
//! use web_modules::{serve, Frontend, CspNonceLayer};
//! # async fn run(dist: &'static web_modules::include_dir::Dir<'static>) -> std::io::Result<()> {
//! let app = Frontend::embedded(dist).auto().layer(
//!     CspNonceLayer::new(
//!         "default-src 'none'; script-src 'self' 'nonce-{nonce}'; \
//!          style-src 'self' 'nonce-{nonce}'; img-src 'self' data:; font-src 'self'",
//!     )
//!     .rewrite_html_body(),
//! );
//! serve(app, "127.0.0.1:8080".parse().unwrap()).await
//! # }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, Response, StatusCode},
};
use tower_layer::Layer;
use tower_service::Service;

/// Placeholder replaced with the live nonce in `text/html` bodies when body rewriting
/// is enabled via [`CspNonceLayer::rewrite_html_body`].
pub const DEFAULT_SENTINEL: &str = "__CSP_NONCE__";

/// The `{nonce}` token replaced with the per-request nonce in the policy template.
const POLICY_PLACEHOLDER: &str = "{nonce}";

/// Upper bound (bytes) on a `text/html` body the layer will buffer to rewrite. Larger
/// HTML responses are passed through untouched — the nonce header is still set, only the
/// body sentinel substitution is skipped. App HTML is far below this; the cap just bounds
/// memory against a pathological response.
const MAX_HTML_REWRITE: usize = 4 * 1024 * 1024;

/// Number of random bytes per nonce (rendered as `2 * N` lowercase hex chars). 16 bytes
/// = 128 bits of entropy, comfortably above the CSP recommendation.
const NONCE_BYTES: usize = 16;

/// The per-request CSP nonce, stored in request extensions.
///
/// Read it in an axum handler via `Extension(nonce): Extension<CspNonce>`, or from a
/// `Request` via `req.extensions().get::<CspNonce>()` — e.g. to inject `nonce="…"` while
/// rendering a template.
#[derive(Clone, Debug)]
pub struct CspNonce(pub String);

impl CspNonce {
    /// The nonce value: a lowercase hex token. Use it as the `nonce` attribute value and
    /// it will match the `'nonce-…'` source in the header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CspNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Tower [`Layer`] that mints a per-request CSP nonce, exposes it via [`CspNonce`] in the
/// request extensions, and sets a `Content-Security-Policy` header rendered from a policy
/// template. Optionally rewrites a sentinel in `text/html` bodies (see
/// [`rewrite_html_body`](Self::rewrite_html_body)).
#[derive(Clone)]
pub struct CspNonceLayer {
    policy_template: Arc<str>,
    /// `Some(sentinel)` enables body rewriting in `text/html` responses.
    rewrite_sentinel: Option<Arc<str>>,
}

impl CspNonceLayer {
    /// Build the layer from a CSP policy template. Every occurrence of `{nonce}` is
    /// replaced with the per-request nonce when the header is rendered, e.g.
    /// `"script-src 'self' 'nonce-{nonce}'; style-src 'self' 'nonce-{nonce}'"`.
    pub fn new(policy_template: impl Into<String>) -> Self {
        Self {
            policy_template: Arc::from(policy_template.into()),
            rewrite_sentinel: None,
        }
    }

    /// Also replace the [`DEFAULT_SENTINEL`] in `text/html` response bodies with the
    /// per-request nonce — for statically rendered markup that can't interpolate the live
    /// value. See [`rewrite_html_body_with`](Self::rewrite_html_body_with) for a custom token.
    pub fn rewrite_html_body(self) -> Self {
        self.rewrite_html_body_with(DEFAULT_SENTINEL)
    }

    /// Like [`rewrite_html_body`](Self::rewrite_html_body) but with a custom sentinel.
    pub fn rewrite_html_body_with(mut self, sentinel: impl Into<String>) -> Self {
        self.rewrite_sentinel = Some(Arc::from(sentinel.into()));
        self
    }
}

impl<S> Layer<S> for CspNonceLayer {
    type Service = CspNonceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CspNonceService {
            inner,
            policy_template: self.policy_template.clone(),
            rewrite_sentinel: self.rewrite_sentinel.clone(),
        }
    }
}

/// The [`Service`] produced by [`CspNonceLayer`].
#[derive(Clone)]
pub struct CspNonceService<S> {
    inner: S,
    policy_template: Arc<str>,
    rewrite_sentinel: Option<Arc<str>>,
}

impl<S> Service<Request<Body>> for CspNonceService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        // Reuse a nonce already minted upstream (e.g. a nested instance of this layer) so
        // every layer in the stack agrees on one value — otherwise an outer layer would set
        // a header nonce that doesn't match the one written into the body. Mint a fresh one
        // only when none is present.
        let nonce = match req.extensions().get::<CspNonce>() {
            Some(existing) => existing.0.clone(),
            None => {
                let minted = generate_nonce();
                req.extensions_mut().insert(CspNonce(minted.clone()));
                minted
            }
        };

        let header_value =
            HeaderValue::from_str(&self.policy_template.replace(POLICY_PLACEHOLDER, &nonce)).ok();
        let rewrite_sentinel = self.rewrite_sentinel.clone();

        // tower idiom: the *clone* we move into the future is the instance that was
        // `poll_ready`-ed; leave a fresh clone in `self` for the next poll/call cycle.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let mut res = inner.call(req).await?;
            if let Some(hv) = header_value {
                res.headers_mut()
                    .insert(header::CONTENT_SECURITY_POLICY, hv);
            }
            if let Some(sentinel) = rewrite_sentinel {
                if is_html(&res) {
                    res = rewrite_body(res, &sentinel, &nonce).await;
                }
            }
            Ok(res)
        })
    }
}

/// Whether the response declares an HTML content type.
fn is_html(res: &Response<Body>) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().starts_with("text/html"))
}

/// Buffer a `text/html` body and replace `sentinel` with `nonce`. Leaves the body
/// untouched if it isn't valid UTF-8 or doesn't contain the sentinel.
async fn rewrite_body(res: Response<Body>, sentinel: &str, nonce: &str) -> Response<Body> {
    let (mut parts, body) = res.into_parts();
    match axum::body::to_bytes(body, MAX_HTML_REWRITE).await {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) if text.contains(sentinel) => {
                let replaced = text.replace(sentinel, nonce);
                // Length changed — drop the stale Content-Length so the server recomputes
                // it from the new fixed-size body.
                parts.headers.remove(header::CONTENT_LENGTH);
                Response::from_parts(parts, Body::from(replaced))
            }
            _ => Response::from_parts(parts, Body::from(bytes)),
        },
        // Body exceeded the buffer cap (or a stream error). The body is consumed and can't
        // be reconstructed; fail closed rather than serve a truncated page. App HTML never
        // approaches the cap, so this is unreachable in practice.
        Err(_) => {
            parts.status = StatusCode::INTERNAL_SERVER_ERROR;
            parts.headers.remove(header::CONTENT_LENGTH);
            Response::from_parts(parts, Body::empty())
        }
    }
}

/// Mint a nonce: `NONCE_BYTES` of OS CSPRNG entropy, lowercase-hex encoded (a valid CSP
/// `base64-value` token, so no base64 dependency is needed).
fn generate_nonce() -> String {
    let mut buf = [0u8; NONCE_BYTES];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    let mut out = String::with_capacity(NONCE_BYTES * 2);
    for byte in buf {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

#[inline]
fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + (nibble - 10)) as char,
    }
}
