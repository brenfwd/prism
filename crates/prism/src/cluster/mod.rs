//! Clustering: replicated membership, the copy-paste join protocol, and the
//! runtime that ties raft, gRPC and the WireGuard mesh together.

pub mod blob;
pub mod grpc;
pub mod mesh;
pub mod model;
pub mod runtime;

pub use runtime::{Cluster, ClusterConfig};
