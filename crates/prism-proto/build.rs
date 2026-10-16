//! Compiles `proto/prism.proto` with `protox` (pure Rust, no `protoc`) and
//! generates tonic client and server code into `OUT_DIR`. The
//! `rerun-if-changed` line re-runs the script when anything under `proto/`
//! changes.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=proto");
    let fds = protox::compile(["proto/prism.proto"], ["proto"])?;
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(fds)?;
    Ok(())
}
