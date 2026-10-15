//! Live-migration setup between Begin and Perform: Prepare on the
//! destination, then the source-side sockets QEMU streams through.

use std::os::unix::fs::PermissionsExt;

use prism_libvirt::TypedParam;
use prism_libvirt::migrate::params as mp;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::Node;
use super::migrate::{PrepareReq, PrepareResp, splice_loop, to_params};
use crate::cluster::dataplane::migration_dir;
use crate::cluster::model::MemberDoc;

/// What Perform needs: the destination's answer, the Perform parameters
/// and the socket splicers (aborted once the migration ends).
pub(super) type Setup = (PrepareResp, Vec<TypedParam>, Vec<JoinHandle<()>>);

impl Node {
    /// Prepares the destination and opens the source-side sockets. The
    /// caller cancels the source's Begin job if this fails.
    pub(super) async fn prepare_migration(
        &self,
        cluster: &crate::cluster::Cluster,
        dst: &MemberDoc,
        migration: Uuid,
        disks: &[String],
        mut req: PrepareReq,
    ) -> anyhow::Result<Setup> {
        // Destination QEMU listens; disk images are created first.
        req.src_pools = self
            .storage
            .list()
            .iter()
            .map(|p| (p.name.clone(), p.path.to_string_lossy().into_owned()))
            .collect();
        let prep: PrepareResp = self
            .call_peer(
                cluster,
                dst,
                &format!("/api/v1/internal/migrations/{migration}/prepare"),
                &req,
            )
            .await?;
        let mut params = to_params(&prep.params);
        params.extend(
            disks
                .iter()
                .map(|t| TypedParam::string(mp::MIGRATE_DISKS, t.clone())),
        );

        // Source-side sockets QEMU connects to, spliced to the destination.
        let dir = migration_dir(migration);
        crate::storage::create_traversable(&dir)?;
        let mut splicers = Vec::new();
        for which in ["mem", "nbd"] {
            let path = dir.join(format!("{which}.sock"));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)?;
            // QEMU runs as its own user.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))?;
            splicers.push(tokio::spawn(splice_loop(
                listener,
                cluster.channels.clone(),
                dst.clone(),
                migration,
                which,
            )));
        }
        Ok((prep, params, splicers))
    }
}
