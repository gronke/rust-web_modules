//! `web-modules` CLI: `dev` (dev server), `build` (compile source root(s) into a deployable output
//! tree — the static counterpart of `dev`, vendoring npm only when packages are given), `vendor`
//! (vendor npm into `web_modules/` + import map), `ci` (pure-Rust `npm ci`), and `npm` (delegates
//! to npm-utils' `add`/`install`/`upgrade`/…). Requires the `cli` feature; the opt-in `env` feature
//! adds `WEB_MODULES_*` environment-variable config to `build`.
//!
//! Each compiler processor (typescript, scss, tera, minify, gzip) contributes its own `--<name>` /
//! `--no-<name>` toggle — and any `--<name>-…` flags — to `build` and `dev` (assembled via
//! [`CompilerConfig`]); a global `--no-default-features` turns the default-on set (typescript,
//! scss, tera) off so they can be re-enabled individually.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use web_modules::vendor::{vendor, PackageSpec};

/// This binary's fallible return, `()` by default.
type Res<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `build`'s default inline `index.html`. The entry script is RELATIVE (`./app.js`) so the page
/// also loads under a subpath (e.g. a GitHub *project* page served at `/<repo>/`). The literal
/// `{importmap}` is replaced with the generated import-map `<script>`.
const DEFAULT_HTML: &str = "<!doctype html>{importmap}<script type=module src=./app.js></script>";

#[derive(Parser)]
#[command(
    name = "web-modules",
    version,
    about = "Buildless web frontend toolchain (no Node)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Dev server: compile TS/SCSS on the fly, render `*.tera`, watch the tree, live-reload.
    Dev {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
        #[command(flatten)]
        compiler: CompilerConfig,
    },
    /// Build a deployable output tree — the static counterpart of `dev`.
    ///
    /// Compiles the source root(s) — TS→JS, SCSS→CSS, `*.tera`→rendered target, static files
    /// copied — into `--out`, exactly as `dev` serves them, and renders `index.html` with the
    /// import map injected. Vendoring is **optional**: pass `--package name@range` and/or
    /// `--manifest package.json` to vendor npm into `web_modules/`; with neither, a non-vendored
    /// tree just compiles statically. With the opt-in `env` feature each flag also reads a
    /// `WEB_MODULES_*` environment variable; an explicit flag wins.
    Build {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Output directory (required).
        #[arg(long)]
        out: PathBuf,
        /// URL prefix the vendored modules are served at. Under a GitHub *project* page (served at
        /// `/<repo>/`) pass `/<repo>/web_modules`.
        #[cfg_attr(
            feature = "env",
            arg(long, env = "WEB_MODULES_MOUNT", default_value = "/web_modules")
        )]
        #[cfg_attr(not(feature = "env"), arg(long, default_value = "/web_modules"))]
        mount: String,
        /// Fallback inline `index.html` (used only when the tree has no `index.html`); `{importmap}`
        /// is replaced with the import-map `<script>`. Keep entry scripts RELATIVE (`./app.js`).
        /// Ignored when `--template` is given.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_HTML", default_value_t = DEFAULT_HTML.to_owned()))]
        #[cfg_attr(not(feature = "env"), arg(long, default_value_t = DEFAULT_HTML.to_owned()))]
        html: String,
        /// Fallback Tera template file for `index.html`, rendered with an `importmap` variable,
        /// instead of `--html` (same fallback rule: used only when the tree has no `index.html`).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_TEMPLATE"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        template: Option<PathBuf>,
        /// Also vendor the `dependencies` of this `package.json` (repeatable).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_MANIFEST"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        manifest: Vec<PathBuf>,
        /// Package to vendor, as `name` or `name@range` (e.g. `lit@^3`); repeatable. Optional —
        /// omit (with no `--manifest`) for a non-vendored, static-only build.
        #[cfg_attr(
            feature = "env",
            arg(
                long = "package",
                value_name = "SPEC",
                env = "WEB_MODULES_PACKAGES",
                value_delimiter = ' '
            )
        )]
        #[cfg_attr(not(feature = "env"), arg(long = "package", value_name = "SPEC"))]
        packages: Vec<String>,
        #[command(flatten)]
        compiler: CompilerConfig,
    },
    /// Vendor npm packages into web_modules/ + an import map.
    ///
    /// Packages come from positional `name@range` specs and/or the `dependencies` of
    /// `--manifest` package.json(s).
    Vendor {
        /// Output directory (the `web_modules/` root).
        #[arg(long, default_value = "web_modules")]
        out: PathBuf,
        /// URL prefix the output is served at.
        #[arg(long, default_value = "/web_modules")]
        mount: String,
        /// Write the import map JSON here (default: stdout).
        #[arg(long)]
        importmap: Option<PathBuf>,
        /// Also vendor the `dependencies` of this `package.json` (repeatable).
        #[arg(long)]
        manifest: Vec<PathBuf>,
        /// Packages as `name` or `name@range` (e.g. `lit@^3`). Optional when `--manifest` is given.
        packages: Vec<String>,
    },
    /// Install a package-lock.json's exact tree into node_modules/ - a pure-Rust npm ci.
    ///
    /// devDependencies included, each tarball's sha512 integrity verified, platform-mismatched
    /// optional deps skipped, and `node_modules/.bin/` shims created. Installs a project's Node
    /// test tooling (Playwright, `tsc`) with no npm - only the Node runtime is then needed.
    Ci {
        /// Project directory containing `package-lock.json` (default: current dir).
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Run an npm-utils command (add · install · ci · upgrade · …).
    ///
    /// `web-modules npm add lit@^3` is exactly `cargo npm-utils add lit@^3`.
    #[command(disable_help_flag = true)]
    Npm {
        /// Arguments forwarded verbatim to npm-utils (e.g. `add lit@^3`, `install`, `ci`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

/// The compiler processors' assembled CLI surface, flattened into `build` and `dev` so the two
/// share one input profile. Each processor contributes its own `--<name>` / `--no-<name>` toggle
/// (plus any `--<name>-…` flags); `--no-default-features` turns the default-on set off.
///
/// The binary requires the `cli` feature, which forces `typescript`, `scss`, `minify` and `tera`
/// on, so those are unconditional here; only `gzip` (the `compress` feature) is optional.
#[derive(Args, Debug)]
struct CompilerConfig {
    /// Disable every default-on compiler feature (typescript, scss, tera); re-enable individually
    /// with `--typescript` / `--scss` / `--tera`.
    #[arg(long)]
    no_default_features: bool,
    #[command(flatten)]
    typescript: web_modules::typescript::TypescriptArgs,
    #[command(flatten)]
    scss: web_modules::scss::ScssArgs,
    #[command(flatten)]
    tera: web_modules::templates::TeraArgs,
    #[command(flatten)]
    minify: web_modules::minify::MinifyArgs,
    #[cfg(feature = "compress")]
    #[command(flatten)]
    gzip: web_modules::compress::GzipArgs,
}

/// The resolved toggles + tuning, independent of which features were compiled in — what `build`
/// and `dev` actually consume.
struct ResolvedCompiler {
    typescript: bool,
    scss: bool,
    tera: bool,
    minify: bool,
    gzip: bool,
    ts_decorators: web_modules::typescript::Decorators,
    extra_scss_load_paths: Vec<PathBuf>,
}

impl CompilerConfig {
    /// Fold each processor's toggle against its compiled-in default and `--no-default-features`
    /// (typescript/scss/tera default on; minify/gzip default off).
    fn resolve(&self) -> ResolvedCompiler {
        let nd = self.no_default_features;
        ResolvedCompiler {
            typescript: self.typescript.enabled(true, nd),
            scss: self.scss.enabled(true, nd),
            tera: self.tera.enabled(true, nd),
            minify: self.minify.enabled(false, nd),
            #[cfg(feature = "compress")]
            gzip: self.gzip.enabled(false, nd),
            #[cfg(not(feature = "compress"))]
            gzip: false,
            ts_decorators: self.typescript.config.decorators.into(),
            extra_scss_load_paths: self.scss.config.load_paths.clone(),
        }
    }
}

impl ResolvedCompiler {
    /// Map to the build pipeline's [`Processors`](web_modules::build::Processors). `#[non_exhaustive]`,
    /// so built from `default()` and assigned (minify/gzip live in `Output`, not here).
    fn into_processors(self) -> web_modules::build::Processors {
        let mut p = web_modules::build::Processors::default();
        p.typescript = self.typescript;
        p.scss = self.scss;
        p.tera = self.tera;
        p.ts_decorators = self.ts_decorators;
        p.extra_scss_load_paths = self.extra_scss_load_paths;
        p
    }

    /// Map to the dev server's [`DevConfig`](web_modules::dev::DevConfig) (no minify/gzip — those
    /// are build *output* options).
    fn into_dev_config(self) -> web_modules::dev::DevConfig {
        let mut c = web_modules::dev::DevConfig::default();
        c.typescript = self.typescript;
        c.scss = self.scss;
        c.tera = self.tera;
        c.ts_decorators = self.ts_decorators;
        c.extra_scss_load_paths = self.extra_scss_load_paths;
        c
    }
}

/// Default `roots` to the current dir when none were given (matching the dev server and the old
/// `compile` command).
fn roots_or_cwd(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    roots
}

/// Parse one positional vendor spec: `name`, `name@range`, or `@scope/name@range`; the range
/// `@` is the last one, so a leading scope `@` is preserved.
fn parse_spec(p: &str) -> PackageSpec {
    match p.rfind('@') {
        Some(i) if i > 0 => PackageSpec::npm(&p[..i], &p[i + 1..]),
        _ => PackageSpec::npm(p, "*"),
    }
}

/// Build vendor's spec set from positional/`--package` specs plus each `--manifest` package.json's
/// `dependencies`. A positional spec wins over a same-named manifest entry. `vendor` requires a
/// non-empty result (`require_nonempty = true`); `build` allows none (a non-vendored, static build).
fn build_vendor_specs(
    packages: &[String],
    manifests: &[PathBuf],
    require_nonempty: bool,
) -> Res<Vec<PackageSpec>> {
    let mut specs: Vec<PackageSpec> = packages
        .iter()
        .map(String::as_str)
        .map(parse_spec)
        .collect();
    // Each `--manifest` package.json's `dependencies`, via the same helper build scripts use.
    for path in manifests {
        specs.extend(web_modules::vendor::specs_from_package_json(path)?);
    }
    if require_nonempty && specs.is_empty() {
        return Err("vendor: give package specs (e.g. lit@^3) or --manifest <package.json>".into());
    }
    // A positional spec wins over a same-named manifest entry (dedupe, keeping the first).
    let mut seen = std::collections::HashSet::new();
    specs.retain(|s| seen.insert(s.name().to_string()));
    Ok(specs)
}

#[tokio::main]
async fn main() -> Res {
    match Cli::parse().command {
        Command::Dev {
            roots,
            addr,
            compiler,
        } => {
            let config = compiler.resolve().into_dev_config();
            web_modules::dev::serve_with(roots_or_cwd(roots), addr, config).await?;
        }
        Command::Build {
            roots,
            out,
            mount,
            html,
            template,
            manifest,
            packages,
            compiler,
        } => {
            // `build` vendors only when given packages/manifest; otherwise it compiles a
            // non-vendored tree statically (empty spec set, no error).
            let specs = build_vendor_specs(&packages, &manifest, false)?;
            let resolved = compiler.resolve();
            let (minify, gzip) = (resolved.minify, resolved.gzip);
            let output = web_modules::build::Output::new(minify, gzip);
            let processors = resolved.into_processors();
            let roots = roots_or_cwd(roots);
            web_modules::build::build(&web_modules::build::BuildOptions {
                specs: &specs,
                roots: &roots,
                out: &out,
                mount: &mount,
                html: &html,
                template: template.as_deref(),
                processors,
                output,
            })?;
            println!(
                "built {} root(s) → {} ({} package spec(s), mount {mount}{}{})",
                roots.len(),
                out.display(),
                specs.len(),
                if minify { ", minified" } else { "" },
                if gzip { ", gzipped" } else { "" },
            );
        }
        Command::Vendor {
            out,
            mount,
            importmap,
            manifest,
            packages,
        } => {
            let specs = build_vendor_specs(&packages, &manifest, true)?;
            let map = vendor(&out, &mount, &specs)?;
            match importmap {
                Some(path) => {
                    map.write_to(&path)?;
                    println!("wrote import map → {}", path.display());
                }
                None => println!("{}", map.to_json()),
            }
        }
        Command::Ci { dir } => {
            // `npm ci`, in pure Rust — no npm. (npm-utils is a direct dependency, so the bin
            // calls it without the `bundle`-gated re-export.)
            let installed =
                npm_utils::install::from_lockfile(&dir.join("package-lock.json"), &dir)?;
            println!(
                "installed {} package(s) → {} (npm ci, in Rust - no npm)",
                installed.len(),
                dir.join("node_modules").display()
            );
        }
        Command::Npm { args } => {
            // Delegate to npm-utils' own CLI, so `web-modules npm add lit@^3` is exactly
            // `npm-utils add lit@^3`. The leading token stands in for argv[0] (clap takes the
            // displayed program name from npm-utils' own `#[command(name = …)]`).
            npm_utils::cli::run(std::iter::once(OsString::from("npm-utils")).chain(args))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(specs: &[PackageSpec]) -> Vec<&str> {
        specs.iter().map(PackageSpec::name).collect()
    }

    /// Parse a `build` invocation (with a dummy `--out`) and resolve its compiler config.
    fn resolve_build(extra: &[&str]) -> ResolvedCompiler {
        let argv: Vec<&str> = [&["web-modules", "build", "--out", "out"][..], extra].concat();
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Build { compiler, .. } => compiler.resolve(),
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn parse_spec_handles_bare_scoped_and_ranged() {
        assert_eq!(parse_spec("lit").name(), "lit");
        assert_eq!(parse_spec("lit@^3").name(), "lit");
        assert_eq!(parse_spec("@lit/context").name(), "@lit/context");
        assert_eq!(parse_spec("@lit/context@^1").name(), "@lit/context");
    }

    #[test]
    fn vendor_specs_requires_a_source_when_asked() {
        // `vendor` requires a source; `build` (require_nonempty = false) allows none.
        assert!(build_vendor_specs(&[], &[], true).is_err());
        assert!(build_vendor_specs(&[], &[], false).unwrap().is_empty());
    }

    #[test]
    fn vendor_specs_from_positional_specs() {
        let specs =
            build_vendor_specs(&["lit@^3".into(), "@lit/context@^1".into()], &[], true).unwrap();
        assert_eq!(names(&specs), ["lit", "@lit/context"]);
    }

    #[test]
    fn vendor_specs_reads_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"ms":"^2.1.3"}}"#).unwrap();
        let specs = build_vendor_specs(&[], std::slice::from_ref(&manifest), true).unwrap();
        assert_eq!(names(&specs), ["ms"]);
    }

    #[test]
    fn vendor_specs_positional_wins_over_manifest_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"lit":"^2","ms":"^2"}}"#).unwrap();
        let specs =
            build_vendor_specs(&["lit@^3".into()], std::slice::from_ref(&manifest), true).unwrap();
        // lit (positional, first) then ms (manifest); the manifest's lit is deduped out.
        assert_eq!(names(&specs), ["lit", "ms"]);
    }

    #[test]
    fn build_requires_out() {
        assert!(
            Cli::try_parse_from(["web-modules", "build", "web"]).is_err(),
            "--out is required"
        );
    }

    #[test]
    fn build_defaults_roots_to_cwd() {
        let cli = Cli::try_parse_from(["web-modules", "build", "--out", "dist"]).unwrap();
        match cli.command {
            Command::Build { roots, .. } => {
                assert!(roots.is_empty(), "no positional roots given");
                assert_eq!(roots_or_cwd(roots), [PathBuf::from(".")]);
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_parses_positional_roots_and_packages() {
        let cli = Cli::try_parse_from([
            "web-modules",
            "build",
            "web",
            "shared",
            "--out",
            "dist",
            "--mount",
            "/repo/web_modules",
            "--package",
            "lit@^3",
            "--package",
            "@lit/context@^1",
            "--minify",
        ])
        .unwrap();
        match cli.command {
            Command::Build {
                roots,
                out,
                mount,
                packages,
                compiler,
                ..
            } => {
                assert_eq!(roots, [PathBuf::from("web"), PathBuf::from("shared")]);
                assert_eq!(out, PathBuf::from("dist"));
                assert_eq!(mount, "/repo/web_modules");
                assert_eq!(packages, ["lit@^3", "@lit/context@^1"]);
                assert!(compiler.resolve().minify, "--minify opts in");
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn toggles_default_on_set_and_opt_in() {
        let d = resolve_build(&[]);
        assert!(d.typescript && d.scss && d.tera, "ts/scss/tera default on");
        assert!(!d.minify && !d.gzip, "minify/gzip default off");
        assert!(resolve_build(&["--minify"]).minify, "--minify opts in");
    }

    #[test]
    fn toggles_no_scss_disables_regardless_of_order() {
        assert!(!resolve_build(&["--no-scss"]).scss);
        // `--no-<name>` wins over `--<name>`, in either order.
        assert!(!resolve_build(&["--no-scss", "--scss"]).scss);
        assert!(!resolve_build(&["--scss", "--no-scss"]).scss);
    }

    #[test]
    fn no_default_features_disables_then_reenables() {
        let nd = resolve_build(&["--no-default-features"]);
        assert!(
            !nd.typescript && !nd.scss && !nd.tera,
            "--no-default-features turns the default-on set off"
        );
        let one = resolve_build(&["--no-default-features", "--scss"]);
        assert!(one.scss, "--scss re-enables after --no-default-features");
        assert!(!one.typescript && !one.tera, "the others stay off");
    }

    #[test]
    fn typescript_decorators_default_lit() {
        use web_modules::typescript::Decorators;
        assert_eq!(resolve_build(&[]).ts_decorators, Decorators::Lit);
        assert_eq!(
            resolve_build(&["--typescript-decorators", "standard"]).ts_decorators,
            Decorators::Standard
        );
    }
}
