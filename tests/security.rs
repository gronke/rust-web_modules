//! Security promises of the public [`Frontend`] API, pinned so they can't regress
//! unnoticed. Each test names the guarantee it locks down and drives the real router
//! (`tower::ServiceExt::oneshot`, no port bound), so it exercises the same path a client
//! would hit.
//!
//! Companion to the crate-internal unit tests in `src/serve/serving.rs` (which cover the
//! `has_traversal` / `contained_file` / `is_source_file` primitives these promises rest
//! on) and the broader behavioral suite in `tests/server.rs`.
//!
//! The guarantees:
//!   1. A request can never resolve to a file outside a served root (`..` or a symlink).
//!   2. Source files (`.ts`/`.tsx`/`.mts`/`.scss`/`.tera`) are never served raw — not
//!      even via a case-variant extension, which on a case-insensitive filesystem
//!      (macOS, Windows) would otherwise open the on-disk source. Tests pin this with
//!      literally upper-cased filenames so they reproduce on case-sensitive CI too.
//!   3. The dev router serves only compiled output, and the output doesn't leak source.
//!
//! Needs the `dev` feature (for [`Frontend::dev`]); run under `--all-features`.
#![cfg(feature = "dev")]

use std::path::Path;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;
use web_modules::Frontend;

/// GET `uri` against a clone of `router`; return `(status, content-type, body)`.
async fn get(router: Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let res = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, content_type, body)
}

fn write(path: &Path, content: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn contains(body: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(body).contains(needle)
}

/// Promise 1: a `..` request is refused and the out-of-root file never leaks.
#[tokio::test]
async fn static_router_refuses_traversal_out_of_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.js"), b"inside");
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");
    let app = Frontend::dir(&root).router();

    for uri in [
        "/../secret.txt",
        "/a/../../secret.txt",
        "/%2e%2e/secret.txt",
    ] {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not 200");
        assert!(!contains(&body, "TOPSECRET"), "{uri} leaked the secret");
    }
}

/// Promise 1: a symlink inside a root that points outside it is refused (containment is
/// enforced on the *resolved* path, not just lexically).
#[cfg(unix)]
#[tokio::test]
async fn static_router_refuses_symlink_escape() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    std::fs::create_dir_all(&root).unwrap();
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");
    symlink(tmp.path().join("secret.txt"), root.join("link.txt")).unwrap();
    let app = Frontend::dir(&root).router();

    let (status, _, body) = get(app, "/link.txt").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!contains(&body, "TOPSECRET"));
}

/// Promise 2: the static router never serves a source file raw — including when the
/// request uses a case-variant extension. The files are named with literally upper-cased
/// extensions so this reproduces on a case-sensitive filesystem (Linux CI), where it
/// stands in for the `app.SCSS` → `app.scss` fold a case-insensitive FS would perform.
/// Non-source files with upper-cased extensions are still served (no over-blocking).
#[tokio::test]
async fn static_router_never_serves_source_even_with_uppercase_extension() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("App.SCSS"),
        b"// SECRET-SCSS\n$c: red; a { color: $c }",
    );
    write(&tmp.path().join("Main.TS"), b"export const SECRET_TS = 1;");
    write(&tmp.path().join("Logo.SVG"), b"<svg/>");
    let app = Frontend::dir(tmp.path()).router();

    for (uri, marker) in [("/App.SCSS", "SECRET-SCSS"), ("/Main.TS", "SECRET_TS")] {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must be hidden");
        assert!(!contains(&body, marker), "{uri} leaked raw source");
    }
    // A non-source upper-cased file is unaffected.
    let (status, ct, _) = get(app, "/Logo.SVG").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("svg"));
}

/// Promise 2 holds after URL-prefix stripping too: the guard runs on the path relative
/// to the mount, so a source under a mounted prefix is still hidden.
#[tokio::test]
async fn static_router_hides_source_under_a_mount_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("assets/App.SCSS"),
        b"// SECRET-SCSS\na{color:red}",
    );
    write(&tmp.path().join("assets/logo.svg"), b"<svg/>");
    let app = Frontend::new()
        .mount_dir("/assets", tmp.path().join("assets"))
        .router();

    let (status, _, body) = get(app.clone(), "/assets/App.SCSS").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!contains(&body, "SECRET-SCSS"));
    // A sibling non-source under the same prefix is still served.
    let (status, ..) = get(app, "/assets/logo.svg").await;
    assert_eq!(status, StatusCode::OK);
}

/// Promises 2 & 3: the dev router hides `.scss` sources (including case variants), serves
/// the compiled `.css` instead, and the compiled output carries no source-only markers.
#[tokio::test]
async fn dev_router_hides_scss_source_and_serves_compiled_css() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("app.scss"),
        b"// SECRET-COMMENT\n$c: red; a { color: $c }",
    );
    let app = Frontend::dir(tmp.path()).dev();

    // Source is never reachable directly, by any casing.
    for uri in ["/app.scss", "/app.SCSS", "/APP.SCSS"] {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must be hidden");
        assert!(!contains(&body, "SECRET-COMMENT"), "{uri} leaked raw scss");
    }

    // The compiled target is served, and the `//` comment is gone from the output.
    let (status, ct, body) = get(app, "/app.css").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("css"), "content-type was {ct}");
    assert!(
        contains(&body, "red"),
        "compiled css should contain the value"
    );
    assert!(
        !contains(&body, "SECRET-COMMENT"),
        "compiled css must not echo the scss comment"
    );
}

/// Promises 2 & 3: same shape for TypeScript — `.ts` source hidden (any casing), `.js`
/// compiled target served.
#[tokio::test]
async fn dev_router_hides_ts_source_and_serves_compiled_js() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("main.ts"),
        b"export const value: number = 41 + 1;",
    );
    let app = Frontend::dir(tmp.path()).dev();

    for uri in ["/main.ts", "/main.TS", "/MAIN.TS"] {
        let (status, ..) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must be hidden");
    }

    let (status, ct, _) = get(app, "/main.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("javascript"), "content-type was {ct}");
}
