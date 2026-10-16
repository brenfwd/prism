//! Destination half of a live migration: preparing the incoming domain and
//! finishing or cancelling it. The source node reaches these through the
//! internal migration endpoints.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prism_libvirt::TypedParam;
use prism_libvirt::migrate::params as mp;
use uuid::Uuid;

use super::migrate::{FinishReq, FinishResp, PrepareReq, PrepareResp, to_params};
use super::migrate_splice::remove_migration_dir;
use super::{Node, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::cluster::dataplane::migration_dir;
use crate::spec::DiskDevice;
use crate::storage::Pool;

impl Node {
    /// Destination: create images, the seed ISO and the socket directory,
    /// then `Prepare3Params`. Images created here are deleted again if any
    /// later step fails.
    pub async fn migration_prepare(&self, migration: Uuid, req: PrepareReq) -> Result<PrepareResp> {
        let c = self.client()?;
        let pool = self.storage.get(&req.pool)?;
        let created = self.create_incoming_images(&pool, &req).await?;
        match self.prepare_incoming(&c, &pool, migration, &req).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                for n in &created {
                    pool.delete_volume_best_effort(n);
                }
                Err(e)
            }
        }
    }

    /// Creates the empty images the source will fill over NBD. On failure
    /// the ones already created are removed.
    async fn create_incoming_images(&self, pool: &Pool, req: &PrepareReq) -> Result<Vec<String>> {
        let mut created = Vec::new();
        for img in &req.images {
            let made = pool
                .create_image(
                    &self.cfg.libvirt.qemu_img,
                    &img.name,
                    &img.format,
                    img.size,
                    img.backing.as_deref(),
                )
                .await;
            match made {
                Ok(_) => created.push(img.name.clone()),
                Err(e) => {
                    for n in &created {
                        pool.delete_volume_best_effort(n);
                    }
                    return Err(e.into());
                }
            }
        }
        Ok(created)
    }

    /// Writes the seed ISO, creates the socket directory and calls
    /// `Prepare3Params` with the domain XML rewritten for this node.
    async fn prepare_incoming(
        &self,
        c: &prism_libvirt::Client,
        pool: &Pool,
        migration: Uuid,
        req: &PrepareReq,
    ) -> Result<PrepareResp> {
        let seed = self.seed_iso(&req.vm)?;
        let dir = migration_dir(migration);
        // Parents must be traversable by QEMU's user.
        crate::storage::create_traversable(&dir).map_err(anyhow::Error::from)?;
        // QEMU (its own user) creates the listening sockets here.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .map_err(anyhow::Error::from)?;

        let mut xml = rewrite_pool_paths(&req.begin_xml, req.src_pools.values(), &pool.path);
        // Revert leaves the seed CD-ROM on qcow2 overlays that never travel;
        // point it at the plain ISO just written here.
        if let Some(seed) = seed {
            xml = super::seed_xml::flatten_seed_cdroms(&xml, &seed.to_string_lossy());
        }
        let mem = format!("unix://{}", dir.join("mem.sock").display());
        let nbd = format!("unix://{}", dir.join("nbd.sock").display());
        // Finish looks the incoming domain up by DEST_NAME; libvirt's own
        // client always sets it, so Prism must too.
        let pairs = vec![
            (mp::DEST_NAME.to_string(), req.vm.name.clone()),
            (mp::DEST_XML.to_string(), xml.clone()),
            (mp::PERSIST_XML.to_string(), xml),
            (mp::URI.to_string(), mem),
            (mp::DISKS_URI.to_string(), nbd),
        ];
        let cookie = B64.decode(&req.cookie).map_err(anyhow::Error::from)?;
        let mut params = to_params(&pairs);
        params.extend(
            req.vm
                .disks
                .iter()
                .filter(|d| d.device == DiskDevice::Disk)
                .map(|d| TypedParam::string(mp::MIGRATE_DISKS, d.target.clone())),
        );
        let (cookie_out, _uri) = c.migrate_prepare3(&params, &cookie, req.flags).await?;
        Ok(PrepareResp {
            cookie: B64.encode(cookie_out),
            params: pairs,
        })
    }

    /// Destination: `Finish3Params`; cleans up on cancel.
    pub async fn migration_finish(&self, migration: Uuid, req: FinishReq) -> Result<FinishResp> {
        let c = self.client()?;
        let cookie = B64.decode(&req.cookie).map_err(anyhow::Error::from)?;
        let res = c
            .migrate_finish3(&to_params(&req.params), &cookie, req.flags, req.cancelled)
            .await;
        remove_migration_dir(migration);
        match res {
            Ok((_, cookie_out)) if !req.cancelled => {
                if let Some(vm) = &req.vm {
                    self.import_migrated_snapshots(vm.id, &req.pool, vm, &req.snapshot_xml)
                        .await;
                }
                Ok(FinishResp {
                    cookie: B64.encode(cookie_out),
                    ok: true,
                    error: None,
                })
            }
            other => {
                if let Ok(pool) = self.storage.get(&req.pool) {
                    for n in &req.images {
                        pool.delete_volume_best_effort(n);
                    }
                }
                Ok(FinishResp {
                    cookie: String::new(),
                    ok: false,
                    error: other.err().map(|e| e.to_string()),
                })
            }
        }
    }
}

/// Replaces every `'<src>/` and `"<src>/` path prefix in `xml` with the
/// destination pool directory.
fn rewrite_pool_paths<'a>(
    xml: &str,
    src_pools: impl Iterator<Item = &'a String>,
    dst: &Path,
) -> String {
    let dst = dst.to_string_lossy();
    let mut xml = xml.to_string();
    for src in src_pools {
        xml = xml.replace(&format!("'{src}/"), &format!("'{dst}/"));
        xml = xml.replace(&format!("\"{src}/"), &format!("\"{dst}/"));
    }
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_both_quote_styles() {
        let xml = "<source file='/srv/a/x.qcow2'/><source file=\"/srv/a/y.qcow2\"/>";
        let pools = ["/srv/a".to_string()];
        let out = rewrite_pool_paths(xml, pools.iter(), Path::new("/data/b"));
        assert_eq!(
            out,
            "<source file='/data/b/x.qcow2'/><source file=\"/data/b/y.qcow2\"/>"
        );
    }
}
