use super::*;
use crate::cluster::model::{ClusterMode, MemberState};
use crate::spec::tests::sample_spec;
use prism_cluster::Backend;

fn state() -> State {
    State::new(StateDb::memory().unwrap(), Uuid::from_u128(1))
}

#[tokio::test]
async fn put_and_conflicts() {
    let s = state();
    let vm = sample_spec("a");
    s.apply(Command::PutVm {
        vm: Box::new(vm.clone()),
        expect_generation: Some(None),
    })
    .await
    .unwrap();
    // Creating it again with "must not exist" conflicts.
    let again = s
        .apply(Command::PutVm {
            vm: Box::new(vm.clone()),
            expect_generation: Some(None),
        })
        .await;
    assert!(matches!(again, Err(StateError::Conflict(_))));
    // A different VM with the same name conflicts.
    let mut other = sample_spec("a");
    other.id = Uuid::new_v4();
    let dup = s
        .apply(Command::PutVm {
            vm: Box::new(other),
            expect_generation: None,
        })
        .await;
    assert!(matches!(dup, Err(StateError::Conflict(_))));
    assert_eq!(s.vm(vm.id).unwrap().unwrap().node, Uuid::from_u128(1));
    s.apply(Command::DeleteVm { id: vm.id }).await.unwrap();
    assert!(matches!(
        s.apply(Command::DeleteVm { id: vm.id }).await,
        Err(StateError::NotFound(_))
    ));
}

#[tokio::test]
async fn subscribers_see_commands() {
    let s = state();
    let mut rx = s.subscribe();
    s.apply(Command::DeleteVm { id: Uuid::nil() }).await.ok();
    assert!(rx.try_recv().is_err(), "failed commands are not broadcast");
    let pool = PoolDoc {
        name: "p".into(),
        path: "/srv/p".into(),
        format: "qcow2".into(),
        node: Uuid::from_u128(1),
    };
    s.apply(Command::PutPool { pool }).await.unwrap();
    assert!(
        matches!(rx.try_recv().unwrap(), Change::Applied(c) if matches!(*c, Command::PutPool { .. }))
    );
    assert_eq!(s.pools(Uuid::from_u128(1)).unwrap().len(), 1);
    assert_eq!(s.pools(Uuid::from_u128(2)).unwrap().len(), 0);
}

fn cluster() -> ClusterDoc {
    ClusterDoc {
        cluster_id: Uuid::from_u128(9),
        name: "c".into(),
        mode: ClusterMode::HubSpoke,
        mesh_prefix: "fd12:3456:789a::".parse().unwrap(),
        wg_mtu: 1420,
        feature_level: 1,
        created: 0,
    }
}

fn member(c: &ClusterDoc, n: u128, raft_id: u64) -> MemberDoc {
    MemberDoc {
        node_id: Uuid::from_u128(n),
        raft_id,
        name: format!("n{n}"),
        wg_pubkey: format!("key{n}"),
        mesh_addr: c.mesh_addr(raft_id),
        endpoints: vec![],
        state: MemberState::Joining,
        voter: false,
        version: "0.1.0".into(),
        feature_level: 1,
        joined: 0,
    }
}

#[tokio::test]
async fn join_protocol_checks() {
    let s = state();
    let c = cluster();
    let mut vm = sample_spec("legacy");
    vm.node = Uuid::nil();
    s.apply(Command::PutVm {
        vm: Box::new(vm.clone()),
        expect_generation: None,
    })
    .await
    .unwrap();
    s.apply(Command::InitCluster {
        cluster: c.clone(),
        hub: member(&c, 1, 1),
    })
    .await
    .unwrap();
    // Pre-cluster VMs now explicitly belong to the hub.
    assert_eq!(
        s.db()
            .get::<VmSpec>(VMS, &vm.id.to_string())
            .unwrap()
            .unwrap()
            .node,
        Uuid::from_u128(1)
    );

    let invite = InviteDoc {
        invite_id: "aa".into(),
        raft_id: 2,
        assigned_addr: c.mesh_addr(2),
        exp: 100,
        created_by: Uuid::from_u128(1),
        name_hint: None,
        consumed_by: None,
    };
    s.apply(Command::CreateInvite {
        invite: invite.clone(),
    })
    .await
    .unwrap();
    let dup = InviteDoc {
        invite_id: "bb".into(),
        ..invite.clone()
    };
    assert!(matches!(
        s.apply(Command::CreateInvite { invite: dup }).await,
        Err(StateError::Conflict(_))
    ));

    let new = member(&c, 2, 2);
    let late = s
        .apply(Command::AddMember {
            member: new.clone(),
            invite_id: "aa".into(),
            now: 101,
        })
        .await;
    assert!(matches!(late, Err(StateError::Conflict(m)) if m.starts_with("invite_expired")));
    let wrong = MemberDoc {
        mesh_addr: c.mesh_addr(7),
        ..new.clone()
    };
    let r = s
        .apply(Command::AddMember {
            member: wrong,
            invite_id: "aa".into(),
            now: 50,
        })
        .await;
    assert!(matches!(r, Err(StateError::Conflict(m)) if m.starts_with("address_mismatch")));
    s.apply(Command::AddMember {
        member: new.clone(),
        invite_id: "aa".into(),
        now: 50,
    })
    .await
    .unwrap();
    // Replaying the same reply is a no-op; another node is refused.
    s.apply(Command::AddMember {
        member: new.clone(),
        invite_id: "aa".into(),
        now: 50,
    })
    .await
    .unwrap();
    let other = member(&c, 3, 2);
    let r = s
        .apply(Command::AddMember {
            member: other,
            invite_id: "aa".into(),
            now: 50,
        })
        .await;
    assert!(matches!(r, Err(StateError::Conflict(m)) if m.starts_with("invite_consumed")));

    s.apply(Command::RemoveMember {
        node_id: new.node_id,
        now: 60,
    })
    .await
    .unwrap();
    assert_eq!(s.tombstones().unwrap().len(), 1);
    assert_eq!(s.members().unwrap().len(), 1);
}

#[test]
fn backend_records_progress_for_rejected_commands_and_restores() {
    let s = state();
    let b = s.backend();
    let mut meta = prism_cluster::AppliedMeta {
        last_applied: None,
        ..prism_cluster::AppliedMeta::default()
    };
    meta.last_applied = Some(openraft::LogId::new(
        openraft::CommittedLeaderId::new(1, 1),
        5,
    ));
    let bad = serde_json::to_string(&Command::DeleteVm { id: Uuid::nil() }).unwrap();
    let out: Outcome = serde_json::from_str(&b.apply(Some(&bad), &meta).unwrap()).unwrap();
    assert!(matches!(out, Outcome::NotFound(_)));
    assert_eq!(
        b.applied_meta()
            .unwrap()
            .unwrap()
            .last_applied
            .unwrap()
            .index,
        5
    );

    let vm = sample_spec("x");
    let put = serde_json::to_string(&Command::PutVm {
        vm: Box::new(vm.clone()),
        expect_generation: None,
    })
    .unwrap();
    meta.last_applied = Some(openraft::LogId::new(
        openraft::CommittedLeaderId::new(1, 1),
        6,
    ));
    b.apply(Some(&put), &meta).unwrap();
    let snap = b.snapshot().unwrap();

    let s2 = state();
    let b2 = s2.backend();
    b2.restore(&snap, &meta).unwrap();
    assert_eq!(s2.vm(vm.id).unwrap().unwrap().name, "x");
    assert_eq!(
        b2.applied_meta()
            .unwrap()
            .unwrap()
            .last_applied
            .unwrap()
            .index,
        6
    );
}

#[test]
fn backend_halts_on_unreadable_commands() {
    let s = state();
    let b = s.backend();
    let meta = prism_cluster::AppliedMeta::default();
    let err = b
        .apply(Some(r#"{"op":"from_the_future"}"#), &meta)
        .unwrap_err();
    assert!(err.to_string().contains("unreadable"));
}

#[test]
fn level_two_commands_are_refused_at_level_one() {
    assert_eq!(Command::DeleteVm { id: Uuid::nil() }.required_level(), 1);
    assert_eq!(
        Command::DeleteNetwork {
            id: Uuid::nil(),
            now: 0
        }
        .required_level(),
        2
    );
}
