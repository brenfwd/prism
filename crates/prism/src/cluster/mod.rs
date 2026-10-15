//! Clustering: replicated membership, the copy-paste join protocol, and the
//! runtime that ties raft, gRPC and the `WireGuard` mesh together.

pub mod blob;
pub mod dataplane;
pub mod grpc;
pub mod mesh;
pub mod model;
pub mod peers;
pub mod runtime;
pub mod wg;

pub use runtime::{Cluster, ClusterConfig};
