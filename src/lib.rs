//! Pure-Rust, buildless toolchain for **ES modules and Web Components** — no Node,
//! no bundler.
//!
//! It revives the unbundled, native-ESM workflow (vendor each npm package into a
//! `web_modules/` tree + an import map, à la Snowpack) and pairs it with an
//! on-the-fly transform dev server (à la `@web/dev-server`), implemented entirely
//! in Rust on top of [`npm-utils`](https://docs.rs/npm-utils), [oxc] and
//! [`grass`](https://docs.rs/grass). It is **not** a bundler: it emits native ES
//! modules and leaves bare-specifier resolution to the browser's import map.
//!
//! # Modules
//!
//! **Core** (always on):
//! - [`vendor`] — resolve, download and extract npm packages into a `web_modules/`
//!   tree (via `npm-utils`) and accumulate the import map.
//! - [`importmap`] — a deterministic import-map composer (build, merge fragments,
//!   render the `<script type="importmap">`).
//!
//! **Processors** (`processors`, feature-gated, applied to your source/assets):
//! - [`typescript`] *(feature `typescript`)* — TypeScript / modern JS → browser JS via
//!   oxc, with legacy decorators configured for Lit.
//! - [`scss`] *(feature `scss`)* — SCSS → CSS via grass.
//! - [`minify`] *(feature `minify`)* — JS minification via oxc_minifier.
//! - [`dts`] *(`dts`)* — `.d.ts` emission · [`i18n`] *(`i18n`)* — XLIFF merge ·
//!   [`icons`] *(`icons`)* — favicons from a PNG.
//!
//! **Toolchain** (build + serve):
//! - [`build`] — vendor + transform + render into an output dir.
//! - `bundle` *(feature `bundle`)* — bundle an app plus its `node_modules/` (CommonJS and all)
//!   into one browser ESM file via rolldown, for React-class packages that ship only CommonJS.
//! - [`templates`] *(feature `tera`)* — HTML templating (importmap injection).
//! - [`server`] / [`dev`] *(features `axum` / `dev`)* — serve embedded, or compile on
//!   the fly with file-watching + live-reload.
//!
//! [oxc]: https://oxc.rs

mod error;
pub use error::{Error, Result};

// Always-on core, grouped under `core/`: vendoring, import maps, the mount model,
// TypeScript `tsconfig` generation and static-file copying. Re-exported here so callers
// use `web_modules::vendor` etc. — the `core` module itself is private.
mod core;
pub use core::mount::Mount;
pub use core::{importmap, mount, static_files, tsconfig, vendor};

/// Feature-gated source/asset processors, each re-exported at the crate root (e.g.
/// `web_modules::scss`). Grouped to separate "what we apply to your source" from the
/// vendor/import-map core and the build/serve toolchain.
mod processors;

#[cfg(feature = "dts")]
pub use processors::dts;
#[cfg(feature = "i18n")]
pub use processors::i18n;
#[cfg(feature = "icons")]
pub use processors::icons;
#[cfg(feature = "minify")]
pub use processors::minify;
#[cfg(feature = "scss")]
pub use processors::scss;
#[cfg(feature = "typescript")]
pub use processors::typescript;

// Build-time toolchain, grouped under `build/`: the `build` pipeline plus the emit
// helpers `bundle` / `compress` / `templates`, each re-exported at its historical crate
// root path. The `build` module exists whenever any of its members' features is enabled.
#[cfg(any(
    feature = "typescript",
    feature = "bundle",
    feature = "compress",
    feature = "tera"
))]
pub mod build;

#[cfg(feature = "bundle")]
pub use build::bundle;
#[cfg(feature = "compress")]
pub use build::compress;
#[cfg(feature = "tera")]
pub use build::templates;

/// Re-export of [`npm_utils`] as `web_modules::npm` — the vendoring + transitive `node_modules`
/// install engine — behind the `npm` feature. Lets consumers reach the npm API without a separate
/// `npm-utils` dependency: install a tree with `web_modules::npm::install::node_modules`, then
/// bundle it via `web_modules::bundle` (enable the `bundle` feature too).
#[cfg(feature = "npm")]
pub use npm_utils as npm;

// Runtime serving, grouped under `serve/`: the axum `Frontend` router and the dev server,
// over a shared (private) `serving` containment boundary. Re-exported at historical paths.
mod serve;

#[cfg(feature = "dev")]
pub use serve::dev;

#[cfg(feature = "axum")]
pub use serve::server;
#[cfg(feature = "axum")]
pub use serve::server::{serve, Frontend};

/// Re-export of the `include_dir` crate for the [`include_dir::Dir`] type. Use the
/// `include_dir` crate **directly** for the `include_dir!` macro — it emits
/// `include_dir::`-qualified paths that don't resolve through a re-export.
#[cfg(feature = "axum")]
pub use include_dir;
