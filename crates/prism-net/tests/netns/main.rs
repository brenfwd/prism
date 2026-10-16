//! Root-only integration tests. Each test builds its own network namespaces and removes them.
//!
//! They are `#[ignore]`d so `cargo test` stays unprivileged. On a testbed node:
//!
//! ```sh
//! cargo zigbuild --tests --target x86_64-unknown-linux-musl -p prism-net
//! sudo ./netns-<hash> --ignored --test-threads=1
//! ```
//!
//! Needs `ip`, `nft`, `wg`, `ping`, `nc`, `setpriv`, `busybox udhcpc`, `dig` and `python3`.

mod common;
mod dhcp;
mod dns;
mod guard;
mod mesh;
mod nft;
mod vlan;
mod vxlan;
