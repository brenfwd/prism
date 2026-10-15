//! Native async client for the libvirt remote RPC protocol.
//!
//! Talks XDR over the daemon's UNIX socket (`virtqemud-sock` or
//! `libvirt-sock`) so the libvirt C library is never linked.

mod client;
pub mod proto;
pub mod xdr;

pub use client::{Client, DomainEvent, Stream, StreamReader, StreamWriter};
pub use proto::{Domain, DomainInfo, DomainState, LifecycleEvent, NodeInfo, RemoteError};

/// Errors returned by the client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("libvirt I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("libvirt protocol decode error: {0}")]
    Decode(String),
    #[error("libvirt: {0}")]
    Remote(RemoteError),
    #[error("libvirt connection closed")]
    Closed,
}

impl Error {
    /// The libvirt error code, if this is an error returned by the daemon.
    pub fn remote_code(&self) -> Option<i32> {
        match self {
            Self::Remote(e) => Some(e.code),
            _ => None,
        }
    }
}

/// Formats a UUID as the canonical hyphenated lowercase string.
pub fn format_uuid(u: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(36);
    for (i, b) in u.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{b:02x}");
    }
    s
}
