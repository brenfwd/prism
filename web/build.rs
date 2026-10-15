//! Builds the UI with Bun into `$OUT_DIR/dist`.
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
];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("dist");
    for input in INPUTS {
        println!("cargo::rerun-if-changed={input}");
    }
    println!("cargo::rerun-if-env-changed=BUN");
    println!("cargo::rerun-if-env-changed=PRISM_SKIP_WEB");

    if env::var_os("PRISM_SKIP_WEB").is_some_and(|v| v != "0") {
        std::fs::create_dir_all(&out).expect("creating OUT_DIR/dist");
        std::fs::write(
            out.join("index.html"),
            "<!doctype html><title>Prism</title><p>Built with PRISM_SKIP_WEB: the UI is not included.</p>",
        )
        .expect("writing placeholder");
        return;
    }

    let bun = find_bun();
    run(&bun, &["install", "--frozen-lockfile"], &manifest, &out);
    run(&bun, &["run", "build"], &manifest, &out);
    assert!(
        out.join("index.html").is_file(),
        "bun run build did not produce {}",
        out.join("index.html").display()
    );
}

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

fn run(bun: &Path, args: &[&str], dir: &Path, out: &Path) {
    let status = Command::new(bun)
        .args(args)
        .current_dir(dir)
        .env("PRISM_WEB_OUT_DIR", out)
        .status()
        .unwrap_or_else(|e| panic!("running {} {}: {e}", bun.display(), args.join(" ")));
    assert!(
        status.success(),
        "{} {} failed: {status}",
        bun.display(),
        args.join(" ")
    );
}
