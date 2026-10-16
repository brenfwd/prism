//! The error type shared by every module in the crate.

use std::io;

/// Errors returned by every module of this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A kernel request (rtnetlink, generic netlink or nfnetlink) failed.
    #[error("{op}: {source}")]
    Kernel {
        /// The request that failed, such as `add table inet prism`.
        op: String,
        #[source]
        /// The error the kernel returned; its errno is in [`Error::errno`].
        source: io::Error,
    },
    /// A socket or other I/O operation failed.
    #[error("{op}: {source}")]
    Io {
        /// The operation that failed.
        op: String,
        #[source]
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The named interface does not exist.
    #[error("interface {0:?} not found")]
    NoSuchLink(String),
    /// An interface exists under the name but is of another kind.
    #[error("interface {name:?} has kind {found:?}, expected {expected:?}")]
    WrongKind {
        /// The interface name that was asked for.
        name: String,
        /// The kind the caller needed.
        expected: &'static str,
        /// The kind the interface has, or `none` when it has no kind.
        found: String,
    },
    /// A key could not be decoded.
    #[error("invalid WireGuard key: {0}")]
    InvalidKey(String),
    /// A caller-provided value is invalid.
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// The kernel or a peer sent something we could not understand.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// A `Result` that defaults to this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// The OS errno behind a kernel or I/O error, if any.
    pub fn errno(&self) -> Option<i32> {
        match self {
            Self::Kernel { source, .. } | Self::Io { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }

    pub(crate) fn kernel(op: impl Into<String>, source: io::Error) -> Self {
        Self::Kernel {
            op: op.into(),
            source,
        }
    }

    pub(crate) fn io(op: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            op: op.into(),
            source,
        }
    }

    /// Maps an rtnetlink error, keeping the errno when the kernel sent one.
    pub(crate) fn rtnl(op: impl Into<String>, e: rtnetlink::Error) -> Self {
        let source = match e {
            rtnetlink::Error::NetlinkError(msg) => msg.to_io(),
            other => io::Error::other(other.to_string()),
        };
        Self::kernel(op, source)
    }

    pub(crate) fn genl(op: impl Into<String>, e: genetlink::GenetlinkError) -> Self {
        let source = match e {
            genetlink::GenetlinkError::NetlinkError(io) => io,
            other => io::Error::other(other.to_string()),
        };
        Self::kernel(op, source)
    }
}

/// True when an rtnetlink error carries the given errno.
pub(crate) fn rtnl_errno(e: &rtnetlink::Error) -> Option<i32> {
    match e {
        rtnetlink::Error::NetlinkError(msg) => Some(msg.raw_code().abs()),
        _ => None,
    }
}

pub(crate) const ENOENT: i32 = 2;
pub(crate) const ENODEV: i32 = 19;
