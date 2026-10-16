//! Async client for the libvirt remote RPC protocol.
//!
//! The client speaks XDR over the daemon's UNIX socket, so the libvirt C
//! library is never linked. The modular daemons listen on sockets such as
//! `/run/libvirt/virtqemud-sock`, and the monolithic `libvirtd` on
//! `/run/libvirt/libvirt-sock`. Only the `remote` program (version 1) and the
//! keepalive program are implemented, and only the procedures Prism uses.
//!
//! # Lifecycle
//!
//! [`Client::connect`] connects to the socket and opens a hypervisor URI.
//! Call [`Client::register_lifecycle_events`] (or
//! [`Client::register_event`]) once, then read events from
//! [`Client::events`]. A background task pings the daemon every 30 seconds
//! and closes the connection after 120 seconds of silence. [`Client::closed`]
//! resolves when the connection ends for any reason, and every call after
//! that returns [`Error::Closed`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), prism_libvirt::Error> {
//! use prism_libvirt::Client;
//!
//! let client = Client::connect("/run/libvirt/virtqemud-sock", "qemu:///system").await?;
//! client.register_lifecycle_events().await?;
//! let mut events = client.events();
//! for dom in client.list_all_domains(0).await? {
//!     println!("{} is {:?}", dom.name, client.get_info(&dom).await?.state);
//! }
//! if let Ok(event) = events.recv().await {
//!     println!("event for {}", event.domain().name);
//! }
//! client.close().await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod client;
mod domain;
mod events;
pub mod migrate;
pub mod ops;
pub mod proto;
pub mod typed;
pub mod xdr;

pub use client::{Client, Stream, StreamReader, StreamWriter};
pub use events::{BlockJobStatus, DomainEvent};
pub use ops::{AddressSource, BlockInfo, BlockJobInfo, DomainChild, GuestInterface, InterfaceAddr};
pub use proto::{
    Domain, DomainInfo, DomainState, LifecycleEvent, MemoryStat, NodeInfo, RemoteError, StateInfo,
};
pub use typed::{Params, TypedParam, TypedValue};

/// Errors returned by the client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The socket failed to connect, read or write.
    #[error("libvirt I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A message from the daemon was malformed, or a value to send does not
    /// fit the protocol limits.
    #[error("libvirt protocol decode error: {0}")]
    Decode(String),
    /// The daemon answered the call with an error.
    #[error("libvirt: {0}")]
    Remote(RemoteError),
    /// The connection is closed, or closed while the call was pending.
    #[error("libvirt connection closed")]
    Closed,
    /// The daemon did not reply to a call within the call timeout.
    #[error("libvirt call timed out")]
    Timeout,
    /// The daemon sent nothing, not even a keepalive reply, for 120 seconds.
    #[error("libvirt keepalive timed out")]
    KeepaliveTimeout,
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
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(36);
    for (i, &b) in u.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 15)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_uuid_with_hyphens() {
        let u = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(format_uuid(&u), "12345678-9abc-def0-0123-456789abcdef");
    }

    #[test]
    fn remote_code_only_for_remote_errors() {
        let remote = Error::Remote(RemoteError {
            code: 42,
            domain: 0,
            message: None,
            level: 0,
        });
        assert_eq!(remote.remote_code(), Some(42));
        assert_eq!(Error::Closed.remote_code(), None);
    }
}
