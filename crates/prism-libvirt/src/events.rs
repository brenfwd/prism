//! Domain events delivered by the daemon after `register_event`.

use crate::Error;
use crate::proto::{Domain, LifecycleEvent, procs};
use crate::typed::{TYPED_PARAMS_MAX, TypedParam, decode_params};
use crate::xdr::Decoder;

/// `virConnectDomainEventBlockJobStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockJobStatus {
    Completed,
    Failed,
    Canceled,
    Ready,
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

#[derive(Debug, Clone, PartialEq)]
pub enum DomainEvent {
    Lifecycle {
        domain: Domain,
        event: LifecycleEvent,
        detail: i32,
    },
    BlockJob {
        domain: Domain,
        /// Disk target (for example `vda`).
        disk: String,
        job_type: i32,
        status: BlockJobStatus,
    },
    JobCompleted {
        domain: Domain,
        stats: Vec<TypedParam>,
    },
    BalloonChange {
        domain: Domain,
        actual_kib: u64,
    },
    DeviceRemoved {
        domain: Domain,
        alias: String,
    },
    AgentLifecycle {
        domain: Domain,
        connected: bool,
    },
    MigrationIteration {
        domain: Domain,
        iteration: i32,
    },
}

impl DomainEvent {
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
