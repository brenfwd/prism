//! Sets `PRISM_VERSION` to "<version> (<short git hash> <build date>)".
//! Outside a git checkout, or without `git` or `date`, it is the bare version.

use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    let version = env!("CARGO_PKG_VERSION");
    let full = match (
        run("git", &["rev-parse", "--short=9", "HEAD"]),
        run("date", &["-u", "+%Y-%m-%d"]),
    ) {
        (Some(hash), Some(date)) => format!("{version} ({hash} {date})"),
        _ => version.to_owned(),
    };
    println!("cargo:rustc-env=PRISM_VERSION={full}");
}
