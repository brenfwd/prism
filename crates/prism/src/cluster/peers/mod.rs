//! Member-to-member services that make any node able to serve any request:
//! event fan-in, HTTP forwarding and console tunnelling.

mod console;
mod events;
mod tunnel;

use std::pin::Pin;
use std::sync::{Arc, Weak};

use futures::Stream;
use tonic::Status;

pub use console::{ConsoleService, open_remote_console};
pub use events::{EventsService, fan_in};
pub use tunnel::{TunnelService, Tunnelled, forward_http};

use super::model::MemberDoc;
use super::runtime::Cluster;
use crate::node::Node;

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

pub fn grpc_addr(m: &MemberDoc) -> String {
    format!("[{}]:{}", m.mesh_addr, prism_proto::GRPC_PORT)
}

fn node_of(cluster: &Cluster) -> Result<Arc<Node>, Status> {
    cluster
        .node
        .get()
        .and_then(Weak::upgrade)
        .ok_or_else(|| Status::unavailable("node is starting"))
}
