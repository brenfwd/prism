//! `prism cluster ...` and `prism join`: thin clients of the local daemon.
//!
//! Each subcommand sends one or two requests to the daemon named by
//! `http.listen` in `prism.toml` (see [`client`]) and prints a short report to
//! stdout. Errors go to stderr through `main`, with a non-zero exit status.
//! The responses are read into the small structs below; a field the daemon
//! leaves out prints as an empty string, `false` or `-`.

mod client;

use std::io::Write as _;
use std::path::Path;

use anyhow::bail;
use axum::http::Method;
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;

use crate::cluster::model::ClusterMode;
use crate::config::Config;

pub(crate) use client::call;

/// Seconds an invite stays valid when `--ttl` is not given.
const DEFAULT_INVITE_TTL_SECS: u64 = 3600;

/// Subcommands of `prism cluster`.
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
    /// Hand the hub role to another member (run on the current hub).
    Promote { node: String },
    /// Show removed members, whose keys, IDs and addresses are blocked.
    Removed,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Status {
    clustered: bool,
    role: String,
    degraded: bool,
    cluster: ClusterInfo,
    members: Vec<Member>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClusterInfo {
    name: String,
    mode: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Member {
    node_id: String,
    name: String,
    state: String,
    mesh_addr: String,
    endpoints: Vec<String>,
    voter: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AcceptPreview {
    name: String,
    endpoints: Vec<String>,
    fingerprint: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Removed {
    node_id: String,
    mesh_addr: String,
    wg_pubkey: String,
    #[serde(rename = "removed_index")]
    log_index: u64,
    pending: Vec<PendingMember>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PendingMember {
    name: String,
    applied_index: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JoinReply {
    cluster_name: String,
    sponsor: String,
    reply: String,
    fingerprint: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InviteReply {
    blob: String,
}

/// Runs one `prism cluster` subcommand against the daemon.
pub async fn cluster(dir: &Path, cmd: ClusterCmd) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    match cmd {
        ClusterCmd::Init { name, consensus } => init(&cfg, dir, &name, consensus).await,
        ClusterCmd::Status => status(&cfg).await,
        ClusterCmd::Invite { name, ttl, revoke } => invite(&cfg, name, ttl, revoke).await,
        ClusterCmd::Accept { reply, yes } => accept(&cfg, &reply, yes).await,
        ClusterCmd::Removed => removed(&cfg).await,
        ClusterCmd::Promote { node } => {
            let id = resolve_member(&cfg, &node).await?;
            call(
                &cfg,
                Method::POST,
                "/cluster/hub",
                Some(json!({"node": id})),
            )
            .await?;
            println!("{node} is now the hub. This node is a spoke and can leave.");
            Ok(())
        }
        ClusterCmd::Remove { node } => {
            let id = resolve_member(&cfg, &node).await?;
            call(
                &cfg,
                Method::DELETE,
                &format!("/cluster/members/{id}"),
                None,
            )
            .await?;
            println!("Removed {node}.");
            Ok(())
        }
    }
}

async fn init(cfg: &Config, dir: &Path, name: &str, consensus: bool) -> anyhow::Result<()> {
    let mode = if consensus {
        ClusterMode::Consensus
    } else {
        ClusterMode::HubSpoke
    };
    let s: Status = serde_json::from_value(
        call(
            cfg,
            Method::POST,
            "/cluster",
            Some(json!({"name": name, "mode": mode})),
        )
        .await?,
    )?;
    let addr = s.members.first().map_or("", |m| m.mesh_addr.as_str());
    println!(
        "Created cluster {:?}; this node is the hub at {addr}.",
        s.cluster.name
    );
    println!(
        "Next: `prism cluster invite --config {}` and paste the result into `prism join` on the new node.",
        dir.display()
    );
    Ok(())
}

async fn status(cfg: &Config) -> anyhow::Result<()> {
    let s: Status = serde_json::from_value(call(cfg, Method::GET, "/cluster", None).await?)?;
    print!("{}", render_status(&s));
    Ok(())
}

fn render_status(s: &Status) -> String {
    use std::fmt::Write as _;
    if !s.clustered {
        return "Standalone (not in a cluster).\n".into();
    }
    let mut out = format!(
        "Cluster {} ({}), this node: {}{}\n",
        s.cluster.name,
        s.cluster.mode,
        s.role,
        if s.degraded {
            " - DEGRADED (read-only)"
        } else {
            ""
        }
    );
    for m in &s.members {
        let _ = writeln!(
            out,
            "  {:<16} {:<8} {:<26} {}{}",
            m.name,
            m.state,
            m.mesh_addr,
            m.endpoints.first().map_or("-", String::as_str),
            if m.voter { "  (voter)" } else { "" }
        );
    }
    out
}

async fn invite(
    cfg: &Config,
    name: Option<String>,
    ttl: Option<u64>,
    revoke: Option<String>,
) -> anyhow::Result<()> {
    if let Some(id) = revoke {
        call(cfg, Method::DELETE, &format!("/cluster/invites/{id}"), None).await?;
        println!("Revoked invite {id}.");
        return Ok(());
    }
    let v: InviteReply = serde_json::from_value(
        call(
            cfg,
            Method::POST,
            "/cluster/invites",
            Some(json!({"name": name, "ttl_secs": ttl})),
        )
        .await?,
    )?;
    println!("Run this on the new node:\n");
    println!("  prism join --config <dir> {}\n", v.blob);
    println!(
        "The invite is valid for {} minutes.",
        ttl.unwrap_or(DEFAULT_INVITE_TTL_SECS) / 60
    );
    Ok(())
}

async fn accept(cfg: &Config, reply: &str, yes: bool) -> anyhow::Result<()> {
    let p: AcceptPreview = serde_json::from_value(
        call(
            cfg,
            Method::POST,
            "/cluster/accept",
            Some(json!({"reply": reply, "confirm": false})),
        )
        .await?,
    )?;
    let endpoints = if p.endpoints.is_empty() {
        "behind NAT".to_string()
    } else {
        p.endpoints.join(", ")
    };
    let question = format!("Admit {} ({endpoints}, key {})?", p.name, p.fingerprint);
    if !yes && !confirm(&question)? {
        println!("Not admitted.");
        return Ok(());
    }
    call(
        cfg,
        Method::POST,
        "/cluster/accept",
        Some(json!({"reply": reply, "confirm": true})),
    )
    .await?;
    println!("Admitted {}. It finishes joining by itself.", p.name);
    Ok(())
}

async fn removed(cfg: &Config) -> anyhow::Result<()> {
    let list: Vec<Removed> =
        serde_json::from_value(call(cfg, Method::GET, "/cluster/removed", None).await?)?;
    if list.is_empty() {
        println!("No members have been removed.");
    }
    for m in list {
        println!(
            "  {}  {}  key {}  (log index {})",
            m.node_id, m.mesh_addr, m.wg_pubkey, m.log_index
        );
        if m.pending.is_empty() {
            println!("    applied by every member");
        }
        for p in m.pending {
            let applied = p
                .applied_index
                .map_or("unreachable".to_string(), |a| format!("at index {a}"));
            println!("    still trusts it: {} ({applied})", p.name);
        }
    }
    Ok(())
}

/// Asks a yes/no question on stdout and reads the answer from stdin.
///
/// Only `y`, `Y` and `yes` count as yes. Fails when stdout or stdin cannot be
/// used, so a closed terminal does not read as a refusal.
fn confirm(question: &str) -> anyhow::Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(is_yes(&line))
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes")
}

/// Maps a member name (or an ID, passed through) to its node ID.
async fn resolve_member(cfg: &Config, node: &str) -> anyhow::Result<String> {
    if node.parse::<uuid::Uuid>().is_ok() {
        return Ok(node.to_string());
    }
    let s: Status = serde_json::from_value(call(cfg, Method::GET, "/cluster", None).await?)?;
    match find_member(&s.members, node) {
        Some(id) => Ok(id.to_string()),
        None => bail!("no cluster member named {node:?}"),
    }
}

fn find_member<'a>(members: &'a [Member], name: &str) -> Option<&'a str> {
    members
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.node_id.as_str())
}

/// Runs `prism join`: asks the local daemon to start joining with `invite`
/// and prints the reply to paste into `prism cluster accept` on the sponsor.
pub async fn join(dir: &Path, invite: &str) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let r: JoinReply = serde_json::from_value(
        call(
            &cfg,
            Method::POST,
            "/cluster/join",
            Some(json!({"invite": invite})),
        )
        .await?,
    )?;
    println!(
        "Joining cluster {:?} via {}. Paste this into `prism cluster accept` on {} (or the Cluster page):\n",
        r.cluster_name, r.sponsor, r.sponsor
    );
    println!("  {}\n", r.reply);
    println!(
        "Key fingerprint: {}. Check it matches what the member shows.",
        r.fingerprint
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, id: &str) -> Member {
        Member {
            name: name.into(),
            node_id: id.into(),
            ..Member::default()
        }
    }

    #[test]
    fn yes_answers() {
        for yes in ["y\n", "Y", " yes \n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["", "n", "no", "YES", "yep"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    #[test]
    fn finds_members_by_name() {
        let members = [member("a", "1"), member("b", "2")];
        assert_eq!(find_member(&members, "b"), Some("2"));
        assert_eq!(find_member(&members, "c"), None);
    }

    #[test]
    fn standalone_status() {
        assert_eq!(
            render_status(&Status::default()),
            "Standalone (not in a cluster).\n"
        );
    }

    #[test]
    fn member_table() {
        let s: Status = serde_json::from_value(json!({
            "clustered": true, "role": "hub", "degraded": true,
            "cluster": {"name": "lab", "mode": "hub_spoke"},
            "members": [{"name": "n1", "state": "active", "mesh_addr": "fd00::1",
                         "endpoints": [], "voter": true}],
        }))
        .unwrap();
        let text = render_status(&s);
        assert!(text.starts_with("Cluster lab (hub_spoke), this node: hub - DEGRADED"));
        assert!(text.contains("n1") && text.contains("-  (voter)"));
    }

    #[test]
    fn cluster_mode_wire_names() {
        assert_eq!(json!(ClusterMode::HubSpoke), "hub_spoke");
        assert_eq!(json!(ClusterMode::Consensus), "consensus");
    }
}
