//! Batch peer provisioning (`POST /v1/peers/prepare`).
//!
//! The decision half of the endpoint, kept free of IO: given the subscription's
//! grants, the peer cards of every env and the in-memory peer state, it says
//! for every pool what has to happen — create a tunnel at this address, return
//! the one that is already there, or skip the pool with a reason. The handler
//! only executes the plan (one independent insert per pool) and renders it.
//!
//! Why a plan instead of a loop over `post_peers_handler`: the single-create
//! path answers 409 for the pool that is at its cap, and a batch must not turn
//! one full pool into a failed call for the other fourteen. Making the decision
//! pure also means the rules are testable without a database.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use fcore::{Env, IpAddrMask};

use crate::peers::store::PeersState;
use crate::peers::{alloc, check_grant, Grant, Limits, PeerProto};

/// One peer card the batch works on: `<shard>:<pool>:<proto>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolTarget {
    pub card_id: String,
    pub env: Env,
    pub pool: String,
    pub proto: PeerProto,
}

/// What has to happen in one pool. `Create` already carries the address the
/// handler must insert: addresses of one env are unique across pools, so the
/// plan reserves each one before the next pool is looked at.
#[derive(Clone, Debug)]
pub enum Decision {
    /// The same key is already live in this pool (and this subscription):
    /// return it instead of re-creating it.
    Existing { target: PoolTarget, peer: crate::postgres::peers::PeerRow },
    Create { target: PoolTarget, address: Ipv4Addr },
    /// The pool is not provisioned; `reason` is a stable refusal code the
    /// client already knows from `POST /v1/peers`.
    Skip { card_id: String, reason: &'static str },
}

/// Everything the plan validates against, gathered once by the handler.
pub struct Batch<'a> {
    pub state: &'a PeersState,
    pub subscription_id: Uuid,
    /// X25519 key of the physical device (`devices.public_key`): the per-pool
    /// cap is counted by it, exactly like the single-create path.
    pub device_key: &'a str,
    /// Key the peer itself is created with (`public_key` of the request). A
    /// router channel has its own key on the same device, so the idempotency
    /// lookup uses this one and not `device_key`.
    pub peer_key: &'a str,
    pub limits: Limits,
    pub now: DateTime<Utc>,
    /// Envs whose grant quota is already spent (`used_bytes` >= limit).
    pub quota_spent: &'a HashSet<Env>,
    /// Address space per env after the auto-pool step, or why the env cannot
    /// hand one out at all (`db` when the allocation query failed).
    pub blocks: &'a HashMap<Env, Vec<IpAddrMask>>,
    pub env_errors: &'a HashMap<Env, &'static str>,
}

/// Decide every pool, in the order the cards were handed out.
///
/// `non_peer` names the cards that are built client-side (hysteria2 today):
/// they are reported as `card_kind_not_peer` and never reach the peer path.
pub fn plan(
    batch: &Batch<'_>,
    grants: &[Grant],
    targets: &[PoolTarget],
    non_peer: &[(String, String)],
) -> Vec<Decision> {
    let mut out: Vec<Decision> = non_peer
        .iter()
        .map(|(card_id, _kind)| Decision::Skip {
            card_id: card_id.clone(),
            reason: "card_kind_not_peer",
        })
        .collect();

    // Counters the plan moves itself: a batch of fifteen pools adds fifteen
    // live peers, and a later pool must see the ones already planned.
    let base_sub = batch.state.peers.live_of_sub(&batch.subscription_id).len() as i64;
    let mut added_sub: i64 = 0;
    let mut added_in_pool: HashMap<(String, String), i64> = HashMap::new();
    let mut used: HashMap<Env, HashSet<Ipv4Addr>> = HashMap::new();

    for target in targets {
        let env = &target.env;

        if let Some(reason) = batch.env_errors.get(env) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason,
            });
            continue;
        }

        // Grant term and protocol: the same check the single create runs, so a
        // refused protocol reports the code the client already switches on.
        let grant = grants.iter().find(|grant| &grant.env == env);
        if let Err(refusal) = check_grant(grant, target.proto, batch.now) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: refusal.code(),
            });
            continue;
        }
        if batch.quota_spent.contains(env) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: crate::peers::Refusal::QuotaExhausted.code(),
            });
            continue;
        }

        // Idempotency is per (env, pool, key) and comes BEFORE the caps: a
        // retry of a batch that already filled a pool must hand the same peer
        // back, not a `peers_limit` the single-create path also avoids.
        if let Some(existing) =
            batch
                .state
                .peers
                .live_by_key_in(env, Some(&target.pool), batch.peer_key)
        {
            if existing.subscription_id == batch.subscription_id {
                out.push(Decision::Existing {
                    target: target.clone(),
                    peer: existing.clone(),
                });
            } else {
                out.push(Decision::Skip {
                    card_id: target.card_id.clone(),
                    reason: crate::peers::Refusal::KeyTaken.code(),
                });
            }
            continue;
        }

        let pool_key = (env.to_string(), target.pool.clone());
        let live = batch
            .state
            .peers_of_device_key_in(batch.device_key, env, &target.pool)
            + added_in_pool.get(&pool_key).copied().unwrap_or(0);
        if let Err(refusal) = batch.limits.check_device_peers(live) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: refusal.code(),
            });
            continue;
        }
        if let Err(refusal) = batch.limits.check_subscription(base_sub + added_sub) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: refusal.code(),
            });
            continue;
        }

        // The pool's own /20, not the whole shard: a shared list let the first
        // free address come from a NEIGHBOUR pool's range (an eu peer landed on
        // 10.88.16.2, which is ru's block), and HashMap order made it random.
        let pool_blocks = batch.state.block_masks_for(env, Some(target.pool.as_str()));
        let env_blocks: &[IpAddrMask] = batch.blocks.get(env).map(Vec::as_slice).unwrap_or(&[]);
        let blocks: &[IpAddrMask] = if pool_blocks.is_empty() { env_blocks } else { &pool_blocks };
        if blocks.is_empty() {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: crate::peers::Refusal::EnvNotServed.code(),
            });
            continue;
        }
        let env_used = used
            .entry(env.clone())
            .or_insert_with(|| batch.state.peers.used_in(env));
        if env_used.len() as u64 >= alloc::allocatable(blocks) {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: crate::peers::Refusal::NoFreeAddress.code(),
            });
            continue;
        }
        let Some(address) = alloc::first_free(blocks, env_used) else {
            out.push(Decision::Skip {
                card_id: target.card_id.clone(),
                reason: crate::peers::Refusal::NoFreeAddress.code(),
            });
            continue;
        };
        env_used.insert(address);
        *added_in_pool.entry(pool_key).or_default() += 1;
        added_sub += 1;

        out.push(Decision::Create {
            target: target.clone(),
            address,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::str::FromStr;

    use chrono::Utc;

    use fcore::Tag;

    use crate::postgres::peers::{DeviceRow, PeerRow};
    use crate::peers::store::PeersState;

    const KEY: &str = "DEVKEY";

    fn env() -> Env {
        Env::from_str("0001").unwrap()
    }

    fn device_row(sub: Uuid, id: Uuid) -> DeviceRow {
        DeviceRow {
            id,
            subscription_id: sub,
            device_uid: Uuid::new_v4(),
            device_name: Some("iPhone".into()),
            public_key: KEY.to_string(),
            platform: None,
            support_tag: "7K2M-9XQ4P".into(),
            created_at: Utc::now(),
            rotated_at: None,
        }
    }

    fn state_with(dev: DeviceRow, peers: Vec<PeerRow>) -> PeersState {
        let mut state = PeersState::default();
        state.devices.insert(dev);
        for row in peers {
            state.peers.insert(row);
        }
        state
    }

    fn peer_row(sub: Uuid, dev: Uuid, pool: &str, addr: &str) -> PeerRow {
        PeerRow {
            id: Uuid::new_v4(),
            device_id: dev,
            subscription_id: sub,
            env: env(),
            proto: Tag::AmneziaPeer,
            public_key: KEY.to_string(),
            address: addr.to_string(),
            pool: Some(pool.to_string()),
            label: None,
            created_at: Utc::now(),
            revoked_at: None,
            revoke_reason: None,
        }
    }

    fn grant(protos: Vec<Tag>) -> Grant {
        Grant {
            env: env(),
            protos,
            sources: vec!["standard".to_string()],
            expires_at: None,
            traffic_limit: None,
        }
    }

    fn target(pool: &str) -> PoolTarget {
        PoolTarget {
            card_id: format!("0001:{}:amnezia_peer", pool),
            env: env(),
            pool: pool.to_string(),
            proto: PeerProto::Amnezia,
        }
    }

    fn limits(per_device: i64, per_subscription: i64) -> Limits {
        Limits {
            per_device,
            per_subscription,
            devices_per_subscription: 15,
        }
    }

    fn blocks() -> HashMap<Env, Vec<IpAddrMask>> {
        HashMap::from([(env(), vec!["10.88.0.0/24".parse().unwrap()])])
    }

    /// Executes a `Create` the way the handler does, so the next plan sees the
    /// same state the database would have.
    fn insert_creates(state: &mut PeersState, sub: Uuid, dev: Uuid, decisions: &[Decision]) -> Vec<PeerRow> {
        let mut rows = Vec::new();
        for decision in decisions {
            if let Decision::Create { target, address } = decision {
                let row = PeerRow {
                    id: Uuid::new_v4(),
                    device_id: dev,
                    subscription_id: sub,
                    env: target.env.clone(),
                    proto: target.proto.tag(),
                    public_key: KEY.to_string(),
                    address: format!("{}/32", address),
                    pool: Some(target.pool.clone()),
                    label: None,
                    created_at: Utc::now(),
                    revoked_at: None,
                    revoke_reason: None,
                };
                state.peers.insert(row.clone());
                rows.push(row);
            }
        }
        rows
    }

    /// (a) Two pools of one grant: two tunnels, two addresses, one key.
    #[test]
    fn two_pools_get_two_peers_with_two_addresses() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let decisions = plan(
            &batch,
            &[grant(vec![Tag::AmneziaPeer])],
            &[target("eu"), target("ru")],
            &[],
        );

        let creates: Vec<_> = decisions
            .iter()
            .filter_map(|d| match d {
                Decision::Create { target, address } => Some((target.pool.clone(), *address)),
                _ => None,
            })
            .collect();
        assert_eq!(creates.len(), 2, "every pool must get its own tunnel");
        assert_eq!(creates[0].0, "eu");
        assert_eq!(creates[1].0, "ru");
        // One key, one address per peer, inside the same env block.
        assert_eq!(creates[0].1.to_string(), "10.88.0.2");
        assert_eq!(creates[1].1.to_string(), "10.88.0.3");

        // The handler writes these rows; the store keeps both under one key
        // because the pool is part of the identity.
        let mut state = state;
        let rows = insert_creates(&mut state, sub, dev, &decisions);
        assert_eq!(rows.len(), 2);
        assert_eq!(state.peers.live_of_sub(&sub).len(), 2);
        assert_eq!(
            state
                .peers
                .live_by_key_in(&env(), Some("eu"), KEY)
                .map(|p| p.address.clone()),
            Some("10.88.0.2/32".to_string())
        );
        assert_eq!(
            state
                .peers
                .live_by_key_in(&env(), Some("ru"), KEY)
                .map(|p| p.address.clone()),
            Some("10.88.0.3/32".to_string())
        );
    }

    /// (b) The second call is a no-op: same peer ids, no new addresses.
    #[test]
    fn a_second_call_returns_the_same_peers() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let mut state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let limits_here = limits(3, 0);

        let first = plan(
            &Batch {
                state: &state,
                subscription_id: sub,
                device_key: KEY,
                peer_key: KEY,
                limits: limits_here,
                now: Utc::now(),
                quota_spent: &quota,
                blocks: &blocks,
                env_errors: &env_errors,
            },
            &[grant(vec![Tag::AmneziaPeer])],
            &[target("eu"), target("ru")],
            &[],
        );
        let rows = insert_creates(&mut state, sub, dev, &first);

        let second = plan(
            &Batch {
                state: &state,
                subscription_id: sub,
                device_key: KEY,
                peer_key: KEY,
                limits: limits_here,
                now: Utc::now(),
                quota_spent: &quota,
                blocks: &blocks,
                env_errors: &env_errors,
            },
            &[grant(vec![Tag::AmneziaPeer])],
            &[target("eu"), target("ru")],
            &[],
        );
        assert!(second
            .iter()
            .all(|d| !matches!(d, Decision::Create { .. })));

        let mut existing: Vec<(String, Uuid)> = second
            .iter()
            .filter_map(|d| match d {
                Decision::Existing { target, peer } => Some((target.pool.clone(), peer.id)),
                _ => None,
            })
            .collect();
        existing.sort();
        let mut first_ids: Vec<(String, Uuid)> = first
            .iter()
            .filter_map(|d| match d {
                Decision::Create { target, .. } => {
                    let row = rows.iter().find(|r| r.pool.as_deref() == Some(target.pool.as_str()))?;
                    Some((target.pool.clone(), row.id))
                }
                _ => None,
            })
            .collect();
        first_ids.sort();
        assert_eq!(existing, first_ids, "ids must not change between calls");
    }

    /// (c) A pool at its cap is skipped; the other pool still gets a peer.
    #[test]
    fn a_full_pool_is_skipped_without_aborting_the_others() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let full: Vec<PeerRow> = ["10.88.0.2", "10.88.0.3", "10.88.0.4"]
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                // Three tunnels of the same device in `eu` under their own
                // channel keys: the batch's key is not among them, so the cap
                // is what refuses, not idempotency.
                let mut row = peer_row(sub, dev, "eu", addr);
                row.public_key = format!("CHANNEL{}", i + 1);
                row
            })
            .collect();
        let state = state_with(device_row(sub, dev), full);
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let decisions = plan(
            &batch,
            &[grant(vec![Tag::AmneziaPeer])],
            &[target("eu"), target("ru")],
            &[],
        );

        match &decisions[0] {
            Decision::Skip { card_id, reason } => {
                assert_eq!(card_id, "0001:eu:amnezia_peer");
                assert_eq!(*reason, "peers_limit");
            }
            other => panic!("expected the full pool to be skipped, got {:?}", other),
        }
        match &decisions[1] {
            Decision::Create { target, address } => {
                assert_eq!(target.pool, "ru");
                assert_eq!(address.to_string(), "10.88.0.5");
            }
            other => panic!("the other pool must still be provisioned, got {:?}", other),
        }
    }

    /// (d) A hysteria2 card is reported, never sent down the peer path.
    #[test]
    fn a_client_config_card_is_skipped_as_card_kind_not_peer() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let decisions = plan(
            &batch,
            &[grant(vec![Tag::AmneziaPeer])],
            &[target("eu")],
            &[(
                "0001:prem:hysteria2".to_string(),
                "client_config".to_string(),
            )],
        );

        assert!(matches!(
            &decisions[0],
            Decision::Skip { card_id, reason }
                if card_id == "0001:prem:hysteria2" && *reason == "card_kind_not_peer"
        ));
        assert!(matches!(&decisions[1], Decision::Create { .. }));
    }

    /// (d') A whole-batch call made only of client_config cards prepares
    /// nothing: every one of them comes back with the stable refusal, and no
    /// address is burned on a pool the client builds itself.
    #[test]
    fn only_client_config_cards_prepare_nothing() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let decisions = plan(
            &batch,
            &[grant(vec![Tag::AmneziaPeer])],
            &[],
            &[
                (
                    "0001:prem:vless_xhttp_cdn".to_string(),
                    "client_config".to_string(),
                ),
                (
                    "0001:prem:hysteria2".to_string(),
                    "client_config".to_string(),
                ),
            ],
        );

        assert_eq!(decisions.len(), 2);
        for decision in &decisions {
            assert!(matches!(
                decision,
                Decision::Skip { reason, .. } if *reason == "card_kind_not_peer"
            ));
        }
    }

    /// (e) Fifteen pools and no subscription-wide cap: one peer per pool.
    #[test]
    fn no_subscription_cap_blocks_fifteen_pools_on_one_device() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks: HashMap<Env, Vec<IpAddrMask>> =
            HashMap::from([(env(), vec!["10.88.0.0/24".parse().unwrap()])]);
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let targets: Vec<PoolTarget> = (1..=15).map(|i| target(&format!("p{}", i))).collect();
        let decisions = plan(&batch, &[grant(vec![Tag::AmneziaPeer])], &targets, &[]);

        let creates: Vec<_> = decisions
            .iter()
            .filter_map(|d| match d {
                Decision::Create { address, .. } => Some(*address),
                _ => None,
            })
            .collect();
        assert_eq!(creates.len(), 15, "0 = unlimited must not cap the batch");
        let unique: HashSet<_> = creates.iter().collect();
        assert_eq!(unique.len(), 15, "every peer needs its own address");
    }

    /// A protocol the grant does not allow is refused per pool, with the code
    /// `POST /v1/peers` uses.
    #[test]
    fn an_ungranted_proto_is_skipped_with_the_refusal_code() {
        let sub = Uuid::new_v4();
        let dev = Uuid::new_v4();
        let state = state_with(device_row(sub, dev), Vec::new());
        let quota = HashSet::new();
        let env_errors = HashMap::new();
        let blocks = blocks();
        let batch = Batch {
            state: &state,
            subscription_id: sub,
            device_key: KEY,
            peer_key: KEY,
            limits: limits(3, 0),
            now: Utc::now(),
            quota_spent: &quota,
            blocks: &blocks,
            env_errors: &env_errors,
        };

        let decisions = plan(
            &batch,
            &[grant(vec![Tag::WireguardPeer])],
            &[target("eu")],
            &[],
        );
        assert!(matches!(
            &decisions[0],
            Decision::Skip { reason, .. } if *reason == "proto_not_granted"
        ));
    }
}
