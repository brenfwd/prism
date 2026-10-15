//! `OpenAPI` 3.1 description of the REST API, served at `/api/v1/openapi.json`.
//!
//! The operation table is the single list of endpoints; the document is built
//! from it at request time, and a test checks every router path is listed.

use axum::Json;
use serde_json::{Map, Value, json};

/// `(method, path, tag, summary)`. Paths use the router's `{param}` syntax.
pub const OPERATIONS: &[(&str, &str, &str, &str)] = &[
    ("get", "/nodes", "nodes", "List nodes"),
    ("get", "/nodes/{node}", "nodes", "Show a node"),
    ("get", "/nodes/{node}/bridges", "nodes", "List host bridges"),
    (
        "get",
        "/nodes/{node}/prereqs",
        "nodes",
        "Check optional host programs",
    ),
    ("get", "/events", "events", "Server-sent event stream"),
    ("get", "/vms", "vms", "List VMs"),
    ("post", "/vms", "vms", "Create a VM"),
    ("get", "/vms/{id}", "vms", "Show a VM"),
    ("patch", "/vms/{id}", "vms", "Update a VM"),
    ("delete", "/vms/{id}", "vms", "Delete a VM"),
    (
        "post",
        "/vms/{id}/actions/{action}",
        "vms",
        "Start, stop, reboot, pause or resume",
    ),
    ("post", "/vms/{id}/disks", "vms", "Add a disk"),
    ("patch", "/vms/{id}/disks/{disk}", "vms", "Update a disk"),
    ("delete", "/vms/{id}/disks/{disk}", "vms", "Remove a disk"),
    ("post", "/vms/{id}/nics", "vms", "Add a NIC"),
    ("delete", "/vms/{id}/nics/{nic}", "vms", "Remove a NIC"),
    (
        "put",
        "/vms/{id}/nics/{nic}/forwards",
        "vms",
        "Set port forwards",
    ),
    ("get", "/vms/{id}/snapshots", "vms", "List snapshots"),
    ("post", "/vms/{id}/snapshots", "vms", "Create a snapshot"),
    (
        "delete",
        "/vms/{id}/snapshots/{name}",
        "vms",
        "Delete a snapshot",
    ),
    (
        "post",
        "/vms/{id}/snapshots/{name}/revert",
        "vms",
        "Revert to a snapshot",
    ),
    (
        "get",
        "/vms/{id}/guest/network",
        "vms",
        "Guest IP addresses",
    ),
    ("post", "/vms/{id}/clone", "vms", "Clone a VM"),
    ("post", "/vms/{id}/move", "vms", "Cold-move a VM"),
    ("post", "/vms/{id}/migrate", "vms", "Live-migrate a VM"),
    ("get", "/vms/{id}/console", "vms", "VNC console (WebSocket)"),
    (
        "get",
        "/vms/{id}/serial",
        "vms",
        "Serial console (WebSocket)",
    ),
    ("get", "/vms/{id}/backups", "backups", "List a VM's backups"),
    ("post", "/vms/{id}/backups", "backups", "Create a backup"),
    ("get", "/backups", "backups", "List all backups"),
    ("get", "/backups/{id}", "backups", "Show a backup"),
    ("delete", "/backups/{id}", "backups", "Delete a backup"),
    (
        "post",
        "/backups/{id}/restore",
        "backups",
        "Restore a backup",
    ),
    ("get", "/nodes/{node}/pools", "storage", "List pools"),
    ("post", "/nodes/{node}/pools", "storage", "Create a pool"),
    (
        "get",
        "/nodes/{node}/pools/{pool}",
        "storage",
        "Show a pool",
    ),
    (
        "delete",
        "/nodes/{node}/pools/{pool}",
        "storage",
        "Delete a pool",
    ),
    (
        "get",
        "/nodes/{node}/pools/{pool}/volumes",
        "storage",
        "List volumes",
    ),
    (
        "post",
        "/nodes/{node}/pools/{pool}/volumes",
        "storage",
        "Create a volume",
    ),
    (
        "get",
        "/nodes/{node}/pools/{pool}/volumes/{volume}",
        "storage",
        "Show a volume",
    ),
    (
        "put",
        "/nodes/{node}/pools/{pool}/volumes/{volume}",
        "storage",
        "Upload a volume",
    ),
    (
        "delete",
        "/nodes/{node}/pools/{pool}/volumes/{volume}",
        "storage",
        "Delete a volume",
    ),
    (
        "get",
        "/nodes/{node}/pools/{pool}/volumes/{volume}/users",
        "storage",
        "VMs using a volume",
    ),
    (
        "post",
        "/nodes/{node}/pools/{pool}/downloads",
        "storage",
        "Download a URL into a pool",
    ),
    ("get", "/networks", "networks", "List networks"),
    ("post", "/networks", "networks", "Create a network"),
    (
        "post",
        "/networks/check",
        "networks",
        "Check a network name and subnet",
    ),
    ("get", "/networks/{id}", "networks", "Show a network"),
    ("patch", "/networks/{id}", "networks", "Update a network"),
    ("delete", "/networks/{id}", "networks", "Delete a network"),
    ("get", "/tasks", "tasks", "List tasks"),
    ("get", "/tasks/{id}", "tasks", "Show a task"),
    ("post", "/tasks/{id}/cancel", "tasks", "Cancel a task"),
    ("get", "/cluster", "cluster", "Cluster status"),
    ("post", "/cluster", "cluster", "Create a cluster"),
    ("get", "/cluster/invites", "cluster", "List invites"),
    ("post", "/cluster/invites", "cluster", "Create an invite"),
    (
        "delete",
        "/cluster/invites/{id}",
        "cluster",
        "Revoke an invite",
    ),
    ("post", "/cluster/accept", "cluster", "Accept a join reply"),
    ("post", "/cluster/join", "cluster", "Join a cluster"),
    ("post", "/cluster/leave", "cluster", "Leave the cluster"),
    ("get", "/cluster/removed", "cluster", "Removed members"),
    (
        "delete",
        "/cluster/members/{node}",
        "cluster",
        "Remove a member",
    ),
    (
        "post",
        "/cluster/members/{node}/voter",
        "cluster",
        "Promote or demote a voter",
    ),
    ("get", "/cluster/mesh", "cluster", "Mesh status"),
    ("get", "/archive/pools", "archive", "List archive pools"),
    (
        "post",
        "/archive/pools",
        "archive",
        "Create an archive pool",
    ),
    (
        "put",
        "/archive/pools/{pool}",
        "archive",
        "Set replica count",
    ),
    (
        "post",
        "/archive/pools/{pool}/repair",
        "archive",
        "Repair replicas",
    ),
    (
        "post",
        "/archive/pools/{pool}/scrub",
        "archive",
        "Scrub files on every node",
    ),
    (
        "post",
        "/archive/pools/{pool}/rebalance",
        "archive",
        "Rebalance files across members",
    ),
    (
        "get",
        "/archive/pools/{pool}/files",
        "archive",
        "List files",
    ),
    (
        "get",
        "/archive/pools/{pool}/files/{path}",
        "archive",
        "Download a file",
    ),
    (
        "put",
        "/archive/pools/{pool}/files/{path}",
        "archive",
        "Upload a file",
    ),
    (
        "delete",
        "/archive/pools/{pool}/files/{path}",
        "archive",
        "Delete a file",
    ),
    (
        "put",
        "/archive/members/{node}/{name}",
        "archive",
        "Set member state",
    ),
    (
        "post",
        "/archive/members/{node}/{name}/drain",
        "archive",
        "Drain a member",
    ),
];

fn path_params(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
        .map(|n| json!({"name": n, "in": "path", "required": true, "schema": {"type": "string"}}))
        .collect()
}

pub fn document() -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for (method, path, tag, summary) in OPERATIONS {
        let item = paths
            .entry((*path).to_string())
            .or_insert_with(|| json!({}));
        item[*method] = json!({
            "tags": [tag],
            "summary": summary,
            "parameters": path_params(path),
            "responses": {
                "default": {
                    "description": "Success, or an RFC 9457 problem",
                    "content": {"application/problem+json": {"schema": {"$ref": "#/components/schemas/Problem"}}}
                }
            }
        });
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Prism API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API of a Prism node. Errors are RFC 9457 problem details."
        },
        "servers": [{"url": "/api/v1"}],
        "paths": paths,
        "components": {"schemas": {"Problem": {
            "type": "object",
            "required": ["type", "title", "status", "code", "detail", "retryable", "degraded"],
            "properties": {
                "type": {"type": "string"},
                "title": {"type": "string"},
                "status": {"type": "integer"},
                "code": {"type": "string"},
                "detail": {"type": "string"},
                "retryable": {"type": "boolean"},
                "degraded": {"type": "boolean"}
            }
        }}}
    })
}

pub async fn serve() -> Json<Value> {
    Json(document())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &[&str] = &[
        include_str!("mod.rs"),
        include_str!("archive.rs"),
        include_str!("archive_ops.rs"),
        include_str!("cluster.rs"),
        include_str!("console.rs"),
        include_str!("host.rs"),
        include_str!("networks.rs"),
        include_str!("pools.rs"),
        include_str!("tasks.rs"),
        include_str!("vms.rs"),
    ];

    #[test]
    fn operations_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (m, p, ..) in OPERATIONS {
            assert!(seen.insert((m, p)), "duplicate {m} {p}");
        }
    }

    #[test]
    fn every_router_path_is_listed() {
        for src in SOURCES {
            for chunk in src.split(".route(").skip(1) {
                let Some(path) = chunk.split('"').nth(1) else {
                    continue;
                };
                if !path.starts_with('/') || path == "/openapi.json" {
                    continue;
                }
                assert!(
                    OPERATIONS.iter().any(|(_, p, ..)| *p == path),
                    "route {path} missing from the OpenAPI table"
                );
            }
        }
    }

    #[test]
    fn document_shape() {
        let doc = document();
        assert_eq!(doc["openapi"], "3.1.0");
        let paths = doc["paths"].as_object().expect("paths");
        assert!(paths["/vms"]["get"].is_object());
        assert!(paths["/vms"]["post"].is_object());
        let params = paths["/vms/{id}"]["get"]["parameters"]
            .as_array()
            .expect("path parameters");
        assert!(params.iter().any(|p| p["name"] == "id"));
    }

    #[test]
    fn path_params_are_extracted() {
        let names: Vec<_> = path_params("/a/{x}/b/{y}")
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["x", "y"]);
    }
}
