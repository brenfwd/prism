//! Clustering: replicated membership, the copy-paste join protocol, and the
//! runtime that ties raft, gRPC and the `WireGuard` mesh together.

/// Invite and reply blobs.
pub mod blob;
/// The TCP data plane for volume copies and migration sockets.
pub mod dataplane;
/// The cluster gRPC service.
pub mod grpc;
/// The mesh interface trait.
pub mod mesh;
/// Replicated cluster documents.
pub mod model;
/// Peer channel and address helpers.
pub mod peers;
/// The cluster runtime.
pub mod runtime;
/// The netlink WireGuard mesh.
pub mod wg;

pub use runtime::{Cluster, ClusterConfig};
