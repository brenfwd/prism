//! Source-side data path of a migration: the socket directory and the loop
//! that splices QEMU's connections to the destination.

use tokio::net::UnixListener;
use uuid::Uuid;

use crate::cluster::dataplane::{Purpose, migration_dir};
use crate::cluster::model::MemberDoc;

/// Removes the socket directory of a migration. A directory that is
/// already gone is not an error.
pub(super) fn remove_migration_dir(migration: Uuid) {
    if let Err(e) = std::fs::remove_dir_all(migration_dir(migration))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%migration, "could not remove the migration socket directory: {e}");
    }
}

/// Accepts QEMU's connections on a source socket and splices each one to
/// the destination over a data-plane session.
pub(super) async fn splice_loop(
    listener: UnixListener,
    channels: prism_cluster::Channels,
    dst: MemberDoc,
    migration: Uuid,
    which: &'static str,
) {
    while let Ok((mut unix, _)) = listener.accept().await {
        let (channels, dst) = (channels.clone(), dst.clone());
        tokio::spawn(async move {
            let res: anyhow::Result<()> = async {
                let ch = channels.get(&crate::cluster::peers::grpc_addr(&dst))?;
                let purpose = serde_json::to_vec(&Purpose::MigrationSocket {
                    migration,
                    which: which.into(),
                })?;
                let reply = prism_proto::v1::data_plane_client::DataPlaneClient::new(ch)
                    .open(prism_proto::v1::OpenSession { purpose })
                    .await?
                    .into_inner();
                let mut tcp =
                    crate::cluster::dataplane::connect(dst.mesh_addr, &reply.session_id).await?;
                tokio::io::copy_bidirectional(&mut unix, &mut tcp).await?;
                Ok(())
            }
            .await;
            if let Err(e) = res {
                tracing::warn!("migration {which} splice: {e:#}");
            }
        });
    }
}
