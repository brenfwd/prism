//! Builds the console decoder (`prism-rfb-wasm`, for `wasm32-unknown-unknown`)
//! and then the UI with Bun into `$OUT_DIR/dist`.
//!
//! Set `BUN` to choose the Bun binary. Set `PRISM_SKIP_WEB=1` to skip the
//! build and embed a placeholder page (for Rust-only checks without Bun).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const INPUTS: &[&str] = &[
    "src",
    "public",
    "index.html",
    "package.json",
    "bun.lock",
    "tsconfig.json",
    "vite.config.ts",
    "../crates/prism-rfb-wasm/src",
    "../crates/prism-rfb-wasm/Cargo.toml",
];

const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Variables that describe the outer build and must not leak into the
/// nested wasm build (target-specific flags, clippy's wrapper, ...).
const OUTER_BUILD_ENV: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_PRIMARY_PACKAGE",
];

/// Builds the wasm decoder and the UI, or writes the placeholder page.
fn main() {
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for build scripts"),
    );
    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"));
    let out = out_dir.join("dist");
    for input in INPUTS {
        println!("cargo::rerun-if-changed={input}");
    }
    println!("cargo::rerun-if-env-changed=BUN");
    println!("cargo::rerun-if-env-changed=PRISM_SKIP_WEB");

    if env::var_os("PRISM_SKIP_WEB").is_some_and(|v| v != "0") {
        write_placeholder(&out);
        return;
    }

    let wasm = build_wasm(&manifest, &out_dir);
    let bun = find_bun();
    let envs = [
        ("PRISM_WEB_OUT_DIR", out.as_path()),
        ("PRISM_RFB_WASM", wasm.as_path()),
    ];
    run(&bun, &["install", "--frozen-lockfile"], &manifest, &envs);
    run(&bun, &["run", "build"], &manifest, &envs);
    assert!(
        out.join("index.html").is_file(),
        "bun run build did not produce {}",
        out.join("index.html").display()
    );
}

/// Writes a stub `index.html` into `out` for builds that skip the UI.
fn write_placeholder(out: &Path) {
    std::fs::create_dir_all(out).unwrap_or_else(|e| panic!("creating {}: {e}", out.display()));
    let page = out.join("index.html");
    std::fs::write(
        &page,
        "<!doctype html><title>Prism</title><p>Built with PRISM_SKIP_WEB: the UI is not included.</p>",
    )
    .unwrap_or_else(|e| panic!("writing {}: {e}", page.display()));
}

/// Builds `prism-rfb-wasm` with a nested cargo. It uses its own target
/// directory under `OUT_DIR`: sharing the outer one would deadlock on
/// cargo's build-directory lock.
fn build_wasm(manifest: &Path, out_dir: &Path) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let workspace = manifest.parent().expect("web/ is inside the workspace");
    let target_dir = out_dir.join("wasm-target");
    let mut cmd = Command::new(&cargo);
    cmd.args([
        "build",
        "--release",
        "--locked",
        "-p",
        "prism-rfb-wasm",
        "--target",
        WASM_TARGET,
    ])
    .arg("--manifest-path")
    .arg(workspace.join("Cargo.toml"))
    .arg("--target-dir")
    .arg(&target_dir);
    for var in OUTER_BUILD_ENV {
        cmd.env_remove(var);
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("running {} build for {WASM_TARGET}: {e}", cargo.display()));
    assert!(
        status.success(),
        "building prism-rfb-wasm for {WASM_TARGET} failed ({status}); \
         if the target is missing, run `rustup target add {WASM_TARGET}`"
    );
    let wasm = target_dir
        .join(WASM_TARGET)
        .join("release/prism_rfb_wasm.wasm");
    assert!(wasm.is_file(), "{} was not produced", wasm.display());
    wasm
}

/// Finds the Bun binary: `$BUN`, then `PATH`, then `~/.bun/bin/bun`.
/// Panics with install instructions when none exists.
fn find_bun() -> PathBuf {
    if let Some(b) = env::var_os("BUN") {
        return b.into();
    }
    let on_path = env::var_os("PATH")
        .into_iter()
        .flat_map(|p| env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("bun"))
        .find(|p| p.is_file());
    let in_home = env::var_os("HOME")
        .map(|h| Path::new(&h).join(".bun/bin/bun"))
        .filter(|p| p.is_file());
    on_path.or(in_home).unwrap_or_else(|| {
        panic!("bun not found: install it from https://bun.sh, set BUN=/path/to/bun, or set PRISM_SKIP_WEB=1")
    })
}

/// Runs `bun args` in `dir` with `envs` added, and panics with the exit
/// status if it cannot start or fails. Bun's output goes straight to the
/// build log.
fn run(bun: &Path, args: &[&str], dir: &Path, envs: &[(&str, &Path)]) {
    let status = Command::new(bun)
        .args(args)
        .current_dir(dir)
        .envs(envs.iter().copied())
        .status()
        .unwrap_or_else(|e| panic!("running {} {}: {e}", bun.display(), args.join(" ")));
    assert!(
        status.success(),
        "{} {} failed: {status}",
        bun.display(),
        args.join(" ")
    );
}
