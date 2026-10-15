//! Raft membership planning for member removal: who to demote, who to drop,
//! and when a removal must be refused to keep the cluster safe.

use std::collections::BTreeSet;

use prism_cluster::RaftId;
use uuid::Uuid;

use crate::cluster::model::{ClusterMode, MemberDoc, MemberState};

/// What the leader should change in raft to match committed members.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Replace the voter set (demoted voters stay learners for now).
    pub set_voters: Option<BTreeSet<RaftId>>,
    /// Drop these from raft entirely.
    pub remove: BTreeSet<RaftId>,
}

/// Compares raft's membership with committed members. Voters are demoted
/// before they are dropped, and a voter is never dropped without a voter-set
/// change that keeps at least one voter.
pub fn plan(in_raft: &BTreeSet<RaftId>, voters: &BTreeSet<RaftId>, members: &[MemberDoc]) -> Plan {
    let want: BTreeSet<RaftId> = members
        .iter()
        .filter(|m| m.voter && m.state == MemberState::Active)
        .map(|m| m.raft_id)
        .collect();
    let gone: BTreeSet<RaftId> = in_raft
        .iter()
        .copied()
        .filter(|id| !members.iter().any(|m| m.raft_id == *id))
        .collect();
    let set_voters = (!want.is_empty() && want != *voters).then_some(want);
    let remove = if set_voters.is_some() {
        gone
    } else {
        gone.difference(voters).copied().collect()
    };
    Plan { set_voters, remove }
}

/// Whether removing `target` is safe. Returns the reason when it is not.
pub fn check_removal(mode: ClusterMode, members: &[MemberDoc], target: Uuid) -> Result<(), String> {
    let Some(m) = members.iter().find(|m| m.node_id == target) else {
        return Ok(());
    };
    if !m.voter {
        return Ok(());
    }
    match mode {
        ClusterMode::HubSpoke => Err(format!(
            "{} is the hub, the cluster's only voter; run `prism leave --force` on it after removing the other members",
            m.name
        )),
        ClusterMode::Consensus => {
            let left = members
                .iter()
                .filter(|o| o.voter && o.node_id != target)
                .count();
            if matches!(left, 1 | 3 | 5) {
                Ok(())
            } else {
                Err(format!(
                    "{} is a voter; removing it would leave {left} voters. Make another member a voter first so 1, 3 or 5 remain",
                    m.name
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(raft_id: u64, voter: bool) -> MemberDoc {
        MemberDoc {
            node_id: Uuid::from_u128(u128::from(raft_id)),
            raft_id,
            name: format!("n{raft_id}"),
            wg_pubkey: String::new(),
            mesh_addr: "fd00::1".parse().unwrap(),
            endpoints: vec![],
            state: MemberState::Active,
            voter,
            version: String::new(),
            joined: 0,
        }
    }

    fn set(ids: &[u64]) -> BTreeSet<RaftId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn removed_learner_is_dropped() {
        let members = [member(1, true), member(2, false)];
        let p = plan(&set(&[1, 2, 3]), &set(&[1]), &members);
        assert_eq!(p.set_voters, None);
        assert_eq!(p.remove, set(&[3]));
    }

    #[test]
    fn removed_voter_is_demoted_then_dropped() {
        let members = [member(1, true), member(2, true), member(3, true)];
        let p = plan(&set(&[1, 2, 3, 4]), &set(&[1, 2, 3, 4]), &members);
        assert_eq!(p.set_voters, Some(set(&[1, 2, 3])));
        assert_eq!(p.remove, set(&[4]));
    }

    #[test]
    fn last_voter_is_never_dropped() {
        let p = plan(&set(&[1, 2]), &set(&[1]), &[member(2, false)]);
        assert_eq!(p.set_voters, None);
        assert!(p.remove.is_empty());
    }

    #[test]
    fn hub_cannot_be_removed() {
        let members = [member(1, true), member(2, false)];
        assert!(check_removal(ClusterMode::HubSpoke, &members, members[0].node_id).is_err());
        assert!(check_removal(ClusterMode::HubSpoke, &members, members[1].node_id).is_ok());
    }

    #[test]
    fn consensus_keeps_odd_voters() {
        let members = [
            member(1, true),
            member(2, true),
            member(3, true),
            member(4, true),
            member(5, true),
            member(6, false),
        ];
        assert!(check_removal(ClusterMode::Consensus, &members, members[0].node_id).is_err());
        let three = &members[..3];
        assert!(check_removal(ClusterMode::Consensus, three, three[0].node_id).is_err());
        let four = &members[..4];
        assert!(check_removal(ClusterMode::Consensus, four, four[0].node_id).is_ok());
    }
}
