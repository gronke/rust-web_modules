//! `web-modules` CLI: `dev` (dev server), `compile` (compile root(s) to an output dir), `build`
//! (the full vendor→transform→render pipeline into a deployable `dist/`), `vendor` (vendor npm
//! into `web_modules/` + import map), `ci` (pure-Rust `npm ci`), and `npm` (delegates to
//! npm-utils' `add`/`install`/`upgrade`/…). Requires the `cli` feature; the opt-in `env` feature
//! adds `WEB_MODULES_*` environment-variable config to `build`.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
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
    /// Dev server: compile TS/SCSS on the fly, watch the tree, live-reload.
    Dev {
        /// Source root(s), merged (first match wins). Defaults to the cwd.
        roots: Vec<PathBuf>,
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
    /// Compile source root(s) into an output tree (TS→JS, SCSS→CSS, static files copied).
    ///
    /// Multiple roots are merged (first root wins on a path conflict), exactly as `dev`
    /// serves them. Emits a directory tree, not a bundle; for the full
    /// vendor+transform+render embed pipeline use the `web_modules::build` helper.
    Compile {
        /// Source root(s), merged (first match wins). Defaults to the current dir. With `--out`
        /// omitted, the *last* path is taken as the output directory.
        roots: Vec<PathBuf>,
        /// Output directory (clearer than a trailing positional, and the unambiguous form when
        /// passing more than one root).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also compile SCSS.
        #[arg(long)]
        scss: bool,
        /// Minify emitted `.js`.
        #[arg(long)]
        minify: bool,
    },
    /// Build a complete, deployable `dist/` - the full vendor→transform→render pipeline.
    ///
    /// Vendor npm into `web_modules/`, transform TS, compile SCSS, copy static files, vendor the
    /// transform runtime, and render `index.html` with the import map injected - the CLI surface of
    /// `web_modules::build::build()`. Packages come from positional `name@range` specs and/or the
    /// `dependencies` of `--manifest` package.json(s), exactly like `vendor`. The page renders from
    /// inline `--html` (its literal `{importmap}` is replaced) or, with `--template`, a Tera template
    /// (rendered with an `importmap` variable). With the opt-in `env` feature each flag also reads a
    /// `WEB_MODULES_*` environment variable; an explicit flag wins over the variable.
    Build {
        /// Source directory: TypeScript, SCSS, HTML and other static files.
        #[cfg_attr(
            feature = "env",
            arg(long, env = "WEB_MODULES_SRC", default_value = "web")
        )]
        #[cfg_attr(not(feature = "env"), arg(long, default_value = "web"))]
        src: PathBuf,
        /// Output (`dist/`) directory.
        #[cfg_attr(
            feature = "env",
            arg(long, env = "WEB_MODULES_OUT", default_value = "dist")
        )]
        #[cfg_attr(not(feature = "env"), arg(long, default_value = "dist"))]
        out: PathBuf,
        /// URL prefix the vendored modules are served at. Under a GitHub *project* page (served at
        /// `/<repo>/`) pass `/<repo>/web_modules`.
        #[cfg_attr(
            feature = "env",
            arg(long, env = "WEB_MODULES_MOUNT", default_value = "/web_modules")
        )]
        #[cfg_attr(not(feature = "env"), arg(long, default_value = "/web_modules"))]
        mount: String,
        /// Inline `index.html`; `{importmap}` is replaced with the import-map `<script>`. Keep
        /// entry scripts RELATIVE (`./app.js`) so the page works under a subpath. Ignored when
        /// `--template` is given.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_HTML", default_value_t = DEFAULT_HTML.to_owned()))]
        #[cfg_attr(not(feature = "env"), arg(long, default_value_t = DEFAULT_HTML.to_owned()))]
        html: String,
        /// Tera template file, rendered with an `importmap` variable, instead of `--html`.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_TEMPLATE"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        template: Option<PathBuf>,
        /// Minify the emitted `.js`.
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_MINIFY", default_value = "false",
            num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set, value_parser = parse_bool))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        minify: bool,
        /// Write `<file>.gz` sidecars (requires the `compress` feature; ignored without it).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_GZIP", default_value = "false",
            num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set, value_parser = parse_bool))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        gzip: bool,
        /// Also vendor the `dependencies` of this `package.json` (repeatable).
        #[cfg_attr(feature = "env", arg(long, env = "WEB_MODULES_MANIFEST"))]
        #[cfg_attr(not(feature = "env"), arg(long))]
        manifest: Vec<PathBuf>,
        /// Packages as `name` or `name@range` (e.g. `lit@^3`). Optional when `--manifest` is given.
        #[cfg_attr(
            feature = "env",
            arg(env = "WEB_MODULES_PACKAGES", value_delimiter = ' ')
        )]
        packages: Vec<String>,
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

/// Resolve `compile`'s `[roots…]` + optional `--out` into `(source roots, output dir)`.
/// `--out` wins; otherwise the last positional path is the output. Remaining paths are the
/// source roots, defaulting to the current dir. Errors when no output can be determined.
fn resolve_compile_io(
    mut roots: Vec<PathBuf>,
    out: Option<PathBuf>,
) -> Res<(Vec<PathBuf>, PathBuf)> {
    let out = match out {
        Some(out) => out,
        None => roots
            .pop()
            .ok_or("compile: give an output directory - a trailing path or `--out <dir>`")?,
    };
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    Ok((roots, out))
}

/// Parse one positional vendor spec: `name`, `name@range`, or `@scope/name@range`; the range
/// `@` is the last one, so a leading scope `@` is preserved.
fn parse_spec(p: &str) -> PackageSpec {
    match p.rfind('@') {
        Some(i) if i > 0 => PackageSpec::npm(&p[..i], &p[i + 1..]),
        _ => PackageSpec::npm(p, "*"),
    }
}

/// Permissive boolean parser for the `env`-driven `--minify`/`--gzip` flags. A plain `bool` arg is
/// presence-only (`SetTrue`), so `WEB_MODULES_MINIFY=false` would *enable* it; routing the value
/// through here instead honours an explicit `false`. Accepts the GitHub Actions booleans
/// (`true`/`false`) plus the usual `1/0`, `yes/no`, `on/off`, case-insensitively.
#[cfg(feature = "env")]
fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!(
            "expected a boolean (true/false, 1/0, yes/no, on/off), got {other:?}"
        )),
    }
}

/// Build vendor's spec set from positional `name@range` specs plus each `--manifest`
/// package.json's `dependencies`. A positional spec wins over a same-named manifest entry.
/// Errors when neither source yields a package.
fn build_vendor_specs(packages: &[String], manifests: &[PathBuf]) -> Res<Vec<PackageSpec>> {
    let mut specs: Vec<PackageSpec> = packages
        .iter()
        .map(String::as_str)
        .map(parse_spec)
        .collect();
    // Each `--manifest` package.json's `dependencies`, via the same helper build scripts use.
    for path in manifests {
        specs.extend(web_modules::vendor::specs_from_package_json(path)?);
    }
    if specs.is_empty() {
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
        Command::Dev { mut roots, addr } => {
            if roots.is_empty() {
                roots.push(PathBuf::from("."));
            }
            web_modules::dev::serve(roots, addr).await?;
        }
        Command::Compile {
            roots,
            out,
            scss,
            minify,
        } => {
            let (roots, out) = resolve_compile_io(roots, out)?;
            // SCSS `@use`/`@import` load paths span every root, matching the dev server.
            let load_paths: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
            let (mut modules, mut stylesheets, mut copied) = (0, 0, 0);
            // Compile last-to-first so the first root's files win on a path conflict (the order
            // `dev` resolves overlapping roots in).
            for root in roots.iter().rev() {
                modules += web_modules::typescript::compile_directory(root, &out)?;
                if scss {
                    stylesheets += web_modules::scss::compile_directory(root, &out, &load_paths)?;
                }
                // Carry across everything the processors don't transform (HTML, images, JSON, …)
                // so the output is a complete, servable tree — not just the compiled modules.
                copied += web_modules::static_files::copy_static(root, &out)?;
            }
            if minify {
                web_modules::minify::minify_directory(&out)?;
            }
            println!(
                "compiled {modules} module(s){} + copied {copied} static file(s) from {} root(s) → {}{}",
                if scss {
                    format!(", {stylesheets} stylesheet(s)")
                } else {
                    String::new()
                },
                roots.len(),
                out.display(),
                if minify { " (minified)" } else { "" },
            );
        }
        Command::Build {
            src,
            out,
            mount,
            html,
            template,
            minify,
            gzip,
            manifest,
            packages,
        } => {
            // Same spec assembly as `vendor`: positional specs plus each `--manifest`'s
            // dependencies, with a positional winning over a same-named manifest entry.
            let specs = build_vendor_specs(&packages, &manifest)?;
            // `--gzip` only does anything with the `compress` feature compiled in; be loud rather
            // than silently dropping it.
            #[cfg(not(feature = "compress"))]
            if gzip {
                eprintln!(
                    "web-modules: --gzip ignored - built without the `compress` feature \
                     (reinstall with `--features cli,compress`)"
                );
            }
            // The full pipeline (vendor + transform + render), shared with consumer build scripts.
            web_modules::build::build(&web_modules::build::BuildOptions {
                specs: &specs,
                src: &src,
                out: &out,
                mount: &mount,
                html: &html,
                template: template.as_deref(),
                // `Output` is `#[non_exhaustive]`, so build it via the constructor, not a literal.
                output: web_modules::build::Output::new(minify, gzip),
            })?;
            println!(
                "built dist → {} ({} package spec(s), mount {mount}{}{})",
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
            let specs = build_vendor_specs(&packages, &manifest)?;
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

    #[test]
    fn compile_io_two_paths_last_is_output() {
        let (roots, out) = resolve_compile_io(vec!["web".into(), "dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from("web")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_many_roots_last_is_output() {
        let (roots, out) =
            resolve_compile_io(vec!["a".into(), "b".into(), "dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_single_path_defaults_source_to_cwd() {
        let (roots, out) = resolve_compile_io(vec!["dist".into()], None).unwrap();
        assert_eq!(roots, [PathBuf::from(".")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_out_flag_keeps_all_positionals_as_roots() {
        let (roots, out) =
            resolve_compile_io(vec!["a".into(), "b".into()], Some("dist".into())).unwrap();
        assert_eq!(roots, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_out_flag_without_roots_defaults_to_cwd() {
        let (roots, out) = resolve_compile_io(vec![], Some("dist".into())).unwrap();
        assert_eq!(roots, [PathBuf::from(".")]);
        assert_eq!(out, PathBuf::from("dist"));
    }

    #[test]
    fn compile_io_no_output_is_an_error() {
        assert!(resolve_compile_io(vec![], None).is_err());
    }

    #[test]
    fn parse_spec_handles_bare_scoped_and_ranged() {
        assert_eq!(parse_spec("lit").name(), "lit");
        assert_eq!(parse_spec("lit@^3").name(), "lit");
        assert_eq!(parse_spec("@lit/context").name(), "@lit/context");
        assert_eq!(parse_spec("@lit/context@^1").name(), "@lit/context");
    }

    #[test]
    fn vendor_specs_requires_a_source() {
        assert!(build_vendor_specs(&[], &[]).is_err());
    }

    #[test]
    fn vendor_specs_from_positional_specs() {
        let specs = build_vendor_specs(&["lit@^3".into(), "@lit/context@^1".into()], &[]).unwrap();
        assert_eq!(names(&specs), ["lit", "@lit/context"]);
    }

    #[test]
    fn vendor_specs_reads_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"ms":"^2.1.3"}}"#).unwrap();
        let specs = build_vendor_specs(&[], std::slice::from_ref(&manifest)).unwrap();
        assert_eq!(names(&specs), ["ms"]);
    }

    #[test]
    fn vendor_specs_positional_wins_over_manifest_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("package.json");
        std::fs::write(&manifest, r#"{"dependencies":{"lit":"^2","ms":"^2"}}"#).unwrap();
        let specs =
            build_vendor_specs(&["lit@^3".into()], std::slice::from_ref(&manifest)).unwrap();
        // lit (positional, first) then ms (manifest); the manifest's lit is deduped out.
        assert_eq!(names(&specs), ["lit", "ms"]);
    }

    #[test]
    fn build_parses_positional_specs_and_flags() {
        let cli = Cli::try_parse_from([
            "web-modules",
            "build",
            "lit@^3",
            "@lit/context@^1",
            "--src",
            "web",
            "--out",
            "dist",
            "--mount",
            "/repo/web_modules",
            "--minify",
        ])
        .unwrap();
        match cli.command {
            Command::Build {
                src,
                out,
                mount,
                minify,
                packages,
                ..
            } => {
                assert_eq!(src, PathBuf::from("web"));
                assert_eq!(out, PathBuf::from("dist"));
                assert_eq!(mount, "/repo/web_modules");
                assert!(minify);
                assert_eq!(packages, ["lit@^3", "@lit/context@^1"]);
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_defaults_are_web_dist_and_flags_off() {
        let cli = Cli::try_parse_from(["web-modules", "build", "lit@^3"]).unwrap();
        match cli.command {
            Command::Build {
                src,
                out,
                mount,
                minify,
                gzip,
                ..
            } => {
                assert_eq!(src, PathBuf::from("web"));
                assert_eq!(out, PathBuf::from("dist"));
                assert_eq!(mount, "/web_modules");
                assert!(!minify && !gzip, "minify/gzip default off");
            }
            _ => panic!("expected Build"),
        }
    }

    #[cfg(feature = "env")]
    #[test]
    fn parse_bool_accepts_common_forms() {
        for t in ["1", "true", "TRUE", "Yes", "on"] {
            assert_eq!(parse_bool(t), Ok(true), "{t:?} should be true");
        }
        for f in ["0", "false", "FALSE", "No", "off"] {
            assert_eq!(parse_bool(f), Ok(false), "{f:?} should be false");
        }
        assert!(parse_bool("maybe").is_err());
    }

    #[cfg(feature = "env")]
    #[test]
    fn build_minify_env_false_is_honored() {
        // The bool-via-env gotcha: a presence-only `SetTrue` flag would read the var's mere
        // existence as `true`. The parsed-value form must honour the *value*. No `--minify` on the
        // argv, so the env var is the only source. (No other test writes this var, so no race.)
        std::env::set_var("WEB_MODULES_MINIFY", "false");
        let parsed = Cli::try_parse_from(["web-modules", "build", "lit@^3"]);
        std::env::remove_var("WEB_MODULES_MINIFY");
        match parsed.unwrap().command {
            Command::Build { minify, .. } => {
                assert!(!minify, "WEB_MODULES_MINIFY=false must stay false")
            }
            _ => panic!("expected Build"),
        }
    }
}
