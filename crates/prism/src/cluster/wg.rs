//! The `WireGuard` mesh over netlink (`prism-net`).

use std::sync::Arc;

use super::mesh::Mesh;

/// The mesh implementation, when this build supports it.
pub fn mesh() -> Option<Arc<dyn Mesh>> {
    None
}
