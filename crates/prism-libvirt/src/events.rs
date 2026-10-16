//! Domain events delivered by the daemon after `register_event`.

use crate::Error;
use crate::proto::{Domain, LifecycleEvent, procs};
use crate::typed::{TYPED_PARAMS_MAX, TypedParam, decode_params};
use crate::xdr::Decoder;

/// `virConnectDomainEventBlockJobStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockJobStatus {
    /// The job finished.
    Completed,
    /// The job failed.
    Failed,
    /// The job was cancelled.
    Canceled,
    /// A mirror job is in sync and can be pivoted.
    Ready,
    /// A status code this client does not know.
    Unknown(i32),
}

impl From<i32> for BlockJobStatus {
    fn from(v: i32) -> Self {
        match v {
            0 => Self::Completed,
            1 => Self::Failed,
            2 => Self::Canceled,
            3 => Self::Ready,
            v => Self::Unknown(v),
        }
    }
}

/// An event the daemon sends for a domain, after the matching
/// `register_event` call.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainEvent {
    /// A lifecycle change (`VIR_DOMAIN_EVENT_ID_LIFECYCLE`).
    Lifecycle {
        /// The domain.
        domain: Domain,
        /// What happened.
        event: LifecycleEvent,
        /// The event-specific detail code.
        detail: i32,
    },
    /// A block job changed status.
    BlockJob {
        /// The domain.
        domain: Domain,
        /// Disk target (for example `vda`).
        disk: String,
        /// `virDomainBlockJobType`.
        job_type: i32,
        /// The new status.
        status: BlockJobStatus,
    },
    /// A domain job (migration, backup) finished.
    JobCompleted {
        /// The domain.
        domain: Domain,
        /// Final job statistics.
        stats: Vec<TypedParam>,
    },
    /// The balloon driver reached a new size.
    BalloonChange {
        /// The domain.
        domain: Domain,
        /// Current balloon size in KiB.
        actual_kib: u64,
    },
    /// A hot-unplugged device is gone from the guest.
    DeviceRemoved {
        /// The domain.
        domain: Domain,
        /// Device alias, such as `virtio-disk1`.
        alias: String,
    },
    /// The QEMU guest agent connected or disconnected.
    AgentLifecycle {
        /// The domain.
        domain: Domain,
        /// Whether the agent is connected.
        connected: bool,
    },
    /// A live migration finished another memory copy pass.
    MigrationIteration {
        /// The domain.
        domain: Domain,
        /// Pass number, starting at 1.
        iteration: i32,
    },
}

impl DomainEvent {
    /// The domain the event is about.
    pub fn domain(&self) -> &Domain {
        match self {
            Self::Lifecycle { domain, .. }
            | Self::BlockJob { domain, .. }
            | Self::JobCompleted { domain, .. }
            | Self::BalloonChange { domain, .. }
            | Self::DeviceRemoved { domain, .. }
            | Self::AgentLifecycle { domain, .. }
            | Self::MigrationIteration { domain, .. } => domain,
        }
    }

    /// Decodes an event message body. Returns `None` for unhandled procedures.
    pub(crate) fn decode(proc: u32, body: &[u8]) -> Result<Option<Self>, Error> {
        let mut d = Decoder::new(body);
        let ev = match proc {
            procs::DOMAIN_EVENT_CALLBACK_LIFECYCLE => {
                d.i32()?;
                Self::Lifecycle {
                    domain: Domain::decode(&mut d)?,
                    event: LifecycleEvent::from(d.i32()?),
                    detail: d.i32()?,
                }
            }
            procs::DOMAIN_EVENT_BLOCK_JOB_2 => {
                d.i32()?;
                Self::BlockJob {
                    domain: Domain::decode(&mut d)?,
                    disk: d.string()?,
                    job_type: d.i32()?,
                    status: BlockJobStatus::from(d.i32()?),
                }
            }
            procs::DOMAIN_EVENT_CALLBACK_JOB_COMPLETED => {
                d.i32()?;
                Self::JobCompleted {
                    domain: Domain::decode(&mut d)?,
                    stats: decode_params(&mut d, TYPED_PARAMS_MAX)?,
                }
            }
            procs::DOMAIN_EVENT_CALLBACK_BALLOON_CHANGE => {
                d.i32()?;
                Self::BalloonChange {
                    domain: Domain::decode(&mut d)?,
                    actual_kib: d.u64()?,
                }
            }
            procs::DOMAIN_EVENT_CALLBACK_DEVICE_REMOVED => {
                d.i32()?;
                Self::DeviceRemoved {
                    domain: Domain::decode(&mut d)?,
                    alias: d.string()?,
                }
            }
            procs::DOMAIN_EVENT_CALLBACK_AGENT_LIFECYCLE => {
                d.i32()?;
                let domain = Domain::decode(&mut d)?;
                Self::AgentLifecycle {
                    domain,
                    connected: d.i32()? == 1,
                }
            }
            procs::DOMAIN_EVENT_CALLBACK_MIGRATION_ITERATION => {
                d.i32()?;
                Self::MigrationIteration {
                    domain: Domain::decode(&mut d)?,
                    iteration: d.i32()?,
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(ev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xdr::Encoder;

    fn dom() -> Domain {
        Domain {
            name: "vm".into(),
            uuid: [1; 16],
            id: 2,
        }
    }

    fn body(f: impl FnOnce(&mut Encoder)) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(1);
        dom().encode(&mut e);
        f(&mut e);
        e.into_inner()
    }

    #[test]
    fn decodes_block_job() {
        let b = body(|e| {
            e.string("vda").i32(2).i32(3);
        });
        let ev = DomainEvent::decode(procs::DOMAIN_EVENT_BLOCK_JOB_2, &b)
            .unwrap()
            .unwrap();
        assert_eq!(
            ev,
            DomainEvent::BlockJob {
                domain: dom(),
                disk: "vda".into(),
                job_type: 2,
                status: BlockJobStatus::Ready,
            }
        );
        assert_eq!(ev.domain(), &dom());
    }

    #[test]
    fn decodes_balloon_and_agent_events() {
        let b = body(|e| {
            e.u64(1024);
        });
        let ev = DomainEvent::decode(procs::DOMAIN_EVENT_CALLBACK_BALLOON_CHANGE, &b).unwrap();
        assert_eq!(
            ev,
            Some(DomainEvent::BalloonChange {
                domain: dom(),
                actual_kib: 1024
            })
        );
        let b = body(|e| {
            e.i32(1).i32(1);
        });
        let ev = DomainEvent::decode(procs::DOMAIN_EVENT_CALLBACK_AGENT_LIFECYCLE, &b).unwrap();
        assert!(matches!(
            ev,
            Some(DomainEvent::AgentLifecycle {
                connected: true,
                ..
            })
        ));
    }

    #[test]
    fn unknown_procedure_is_ignored_and_short_body_fails() {
        assert_eq!(DomainEvent::decode(1, &[]).unwrap(), None);
        let err = DomainEvent::decode(procs::DOMAIN_EVENT_CALLBACK_LIFECYCLE, &[0, 0, 0, 1]);
        assert!(err.is_err());
    }
}
