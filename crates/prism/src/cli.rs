//! `prism cluster …` and `prism join`: thin clients of the local daemon's API.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::Config;

#[derive(Debug, Subcommand)]
pub enum ClusterCmd {
    /// Turn this node into the hub of a new cluster.
    Init {
        /// Cluster name, shown in the UI.
        #[arg(long)]
        name: String,
        /// Use 3 or 5 voting members instead of a single hub.
        #[arg(long)]
        consensus: bool,
    },
    /// Show cluster status and members.
    Status,
    /// Create an invite for a new node; paste it into `prism join` there.
    Invite {
        /// Suggested name for the new node.
        #[arg(long)]
        name: Option<String>,
        /// Validity in seconds (default 1 hour).
        #[arg(long)]
        ttl: Option<u64>,
        /// Revoke an open invite by ID instead.
        #[arg(long)]
        revoke: Option<String>,
    },
    /// Admit a new node from the reply its `prism join` printed.
    Accept {
        reply: String,
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove a member (its VMs must be moved first).
    Remove { node: String },
    /// Show removed members, whose keys, IDs and addresses are blocked.
    Removed,
}

/// The daemon's API address: a loopback listener if there is one.
fn api_addr(cfg: &Config) -> anyhow::Result<SocketAddr> {
    cfg.http
        .listen
        .iter()
        .find(|a| a.ip().is_loopback())
        .or_else(|| cfg.http.listen.first())
        .copied()
        .context("http.listen is empty")
}

/// One JSON request over HTTP/1.1 to the local daemon.
pub(crate) async fn call(
    cfg: &Config,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<Value> {
    let addr = api_addr(cfg)?;
    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("connecting to prism at {addr}; is `prism serve` running?"))?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} /api/v1{path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    let (head, mut content) = text
        .split_once("\r\n\r\n")
        .context("malformed HTTP response")?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("malformed HTTP status")?;
    let decoded;
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decoded = dechunk(content);
        content = &decoded;
    }
    let value: Value = if content.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(content)?
    };
    if status >= 400 {
        bail!(
            "{}",
            value["detail"]
                .as_str()
                .map_or_else(|| format!("HTTP {status}"), str::to_string)
        );
    }
    Ok(value)
}

fn dechunk(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some((len, tail)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(len.trim(), 16) else {
            break;
        };
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

pub async fn cluster(dir: &Path, cmd: ClusterCmd) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    match cmd {
        ClusterCmd::Init { name, consensus } => {
            let mode = if consensus { "consensus" } else { "hub_spoke" };
            let s = call(
                &cfg,
                "POST",
                "/cluster",
                Some(json!({"name": name, "mode": mode})),
            )
            .await?;
            println!(
                "Created cluster {:?}; this node is the hub at {}.",
                s["cluster"]["name"].as_str().unwrap_or_default(),
                s["members"][0]["mesh_addr"].as_str().unwrap_or_default()
            );
            println!(
                "Next: `prism cluster invite --config {}` and paste the result into `prism join` on the new node.",
                dir.display()
            );
        }
        ClusterCmd::Status => {
            let s = call(&cfg, "GET", "/cluster", None).await?;
            if !s["clustered"].as_bool().unwrap_or(false) {
                println!("Standalone (not in a cluster).");
                return Ok(());
            }
            println!(
                "Cluster {} ({}), this node: {}{}",
                s["cluster"]["name"].as_str().unwrap_or_default(),
                s["cluster"]["mode"].as_str().unwrap_or_default(),
                s["role"].as_str().unwrap_or_default(),
                if s["degraded"].as_bool().unwrap_or(false) {
                    " — DEGRADED (read-only)"
                } else {
                    ""
                }
            );
            for m in s["members"].as_array().into_iter().flatten() {
                println!(
                    "  {:<16} {:<8} {:<26} {}{}",
                    m["name"].as_str().unwrap_or_default(),
                    m["state"].as_str().unwrap_or_default(),
                    m["mesh_addr"].as_str().unwrap_or_default(),
                    m["endpoints"]
                        .as_array()
                        .and_then(|e| e.first())
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    if m["voter"].as_bool().unwrap_or(false) {
                        "  (voter)"
                    } else {
                        ""
                    }
                );
            }
        }
        ClusterCmd::Invite { name, ttl, revoke } => {
            if let Some(id) = revoke {
                call(&cfg, "DELETE", &format!("/cluster/invites/{id}"), None).await?;
                println!("Revoked invite {id}.");
                return Ok(());
            }
            let v = call(
                &cfg,
                "POST",
                "/cluster/invites",
                Some(json!({"name": name, "ttl_secs": ttl})),
            )
            .await?;
            println!("Run this on the new node:\n");
            println!(
                "  prism join --config <dir> {}\n",
                v["blob"].as_str().unwrap_or_default()
            );
            println!(
                "The invite is valid for {} minutes.",
                ttl.unwrap_or(3600) / 60
            );
        }
        ClusterCmd::Accept { reply, yes } => {
            let p = call(
                &cfg,
                "POST",
                "/cluster/accept",
                Some(json!({"reply": reply, "confirm": false})),
            )
            .await?;
            let endpoints = p["endpoints"].as_array().map(|e| {
                e.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            let question = format!(
                "Admit {} ({}, key {})?",
                p["name"].as_str().unwrap_or_default(),
                endpoints
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "behind NAT".into()),
                p["fingerprint"].as_str().unwrap_or_default()
            );
            if !yes && !confirm(&question) {
                println!("Not admitted.");
                return Ok(());
            }
            call(
                &cfg,
                "POST",
                "/cluster/accept",
                Some(json!({"reply": reply, "confirm": true})),
            )
            .await?;
            println!(
                "Admitted {}. It finishes joining by itself.",
                p["name"].as_str().unwrap_or_default()
            );
        }
        ClusterCmd::Removed => {
            let t = call(&cfg, "GET", "/cluster/removed", None).await?;
            let list = t.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("No members have been removed.");
            }
            for m in list {
                println!(
                    "  {}  {}  key {}",
                    m["node_id"].as_str().unwrap_or_default(),
                    m["mesh_addr"].as_str().unwrap_or_default(),
                    m["wg_pubkey"].as_str().unwrap_or_default()
                );
            }
        }
        ClusterCmd::Remove { node } => {
            call(&cfg, "DELETE", &format!("/cluster/members/{node}"), None).await?;
            println!("Removed {node}.");
        }
    }
    Ok(())
}

pub async fn join(dir: &Path, invite: &str) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let r = call(
        &cfg,
        "POST",
        "/cluster/join",
        Some(json!({"invite": invite})),
    )
    .await?;
    println!(
        "Joining cluster {:?} via {}. Paste this into `prism cluster accept` on {} (or the Cluster page):\n",
        r["cluster_name"].as_str().unwrap_or_default(),
        r["sponsor"].as_str().unwrap_or_default(),
        r["sponsor"].as_str().unwrap_or_default()
    );
    println!("  {}\n", r["reply"].as_str().unwrap_or_default());
    println!(
        "Key fingerprint: {} — check it matches what the member shows.",
        r["fingerprint"].as_str().unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn dechunks() {
        assert_eq!(
            super::dechunk("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"),
            "hello world"
        );
    }
}
