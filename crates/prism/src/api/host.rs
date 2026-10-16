//! Host facts the UI needs for choosing things: bridges to attach NICs to.

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use serde::Serialize;

use super::{ApiError, AppState, check_node};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/nodes/{node}/bridges", get(bridges))
        .route("/nodes/{node}/prereqs", get(prereqs))
}

#[derive(Debug, Serialize)]
struct Bridge {
    name: String,
    up: bool,
    /// Created by Prism for one of its networks.
    prism: bool,
    /// Member interfaces (ports) enslaved to the bridge.
    ports: Vec<String>,
}

fn list_bridges(prism: &[String]) -> std::io::Result<Vec<Bridge>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.join("bridge").is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let up = std::fs::read_to_string(dir.join("operstate"))
            .is_ok_and(|s| matches!(s.trim(), "up" | "unknown"));
        let mut ports: Vec<String> = std::fs::read_dir(dir.join("brif"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        ports.sort();
        out.push(Bridge {
            prism: prism.contains(&name),
            name,
            up,
            ports,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

async fn bridges(
    State(node): State<AppState>,
    Path(n): Path<String>,
) -> Result<axum::Json<Vec<Bridge>>, ApiError> {
    check_node(&node, &n)?;
    let prism: Vec<String> = node.network_bridges().into_values().collect();
    let list = tokio::task::spawn_blocking(move || list_bridges(&prism))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(anyhow::Error::from)?;
    Ok(axum::Json(list))
}

/// Which optional host programs are installed, with what to do about it.
async fn prereqs(
    State(node): State<AppState>,
    Path(n): Path<String>,
) -> Result<axum::Json<Vec<crate::prereqs::Prereq>>, ApiError> {
    check_node(&node, &n)?;
    let list = tokio::task::spawn_blocking(crate::prereqs::all)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(axum::Json(list))
}
