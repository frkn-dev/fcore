//! Which access model applies to a subscription.
//!
//! Two models coexist while v1 is being drained:
//!
//! * **grants** — rows in `subscription_access`: what the payer bought, with
//!   its own term and traffic limit per env;
//! * **legacy** — the product catalog in the api config (`enabled_conns` /
//!   `lite_enabled_conns` / `env_proto_templates`) plus the subscription's own
//!   `expires_at`.
//!
//! The switch is decided **per subscription, not per env**: mixing the two
//! inside one subscription would hand out the same env twice (a v1 connection
//! and a peer) with two addresses and double traffic. A subscription with at
//! least one live grant is served from grants only; one with none keeps the
//! old path until the catalogue is switched off.
//!
//! Nothing here writes: this module answers "who is the source of truth" and
//! translates grants into the shape the legacy provisioning code already
//! understands, so the two paths share one implementation of "create the
//! missing connections".

use std::collections::HashMap;
use uuid::Uuid;

use fcore::{Env, Result, Tag};

use crate::postgres::peers::AccessRow;
use crate::postgres::pg::PgContext;

/// Grants of a subscription that actually gate v1 connections today: peer-only
/// protocols are dropped, an expired grant is dropped too (its connections
/// must be pruned like any other expired one).
pub fn grants_to_conns_map(
    grants: &[AccessRow],
    now: chrono::DateTime<chrono::Utc>,
) -> HashMap<Env, Vec<Tag>> {
    let mut out: HashMap<Env, Vec<Tag>> = HashMap::new();

    for grant in grants {
        if grant.expires_at.is_some_and(|at| at <= now) {
            continue;
        }
        for proto in &grant.protos {
            // Peers are not connections: their state lives in `peers`, and a
            // v1 connection for these tags is refused by the handlers anyway.
            if proto.is_peer() {
                continue;
            }
            out.entry(grant.env.clone()).or_default().push(*proto);
        }
    }

    out
}

/// Where the conns map came from. Pruning is only allowed on grants: the
/// catalogue is a plan-wide default and says nothing about a single payer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Grants,
    Catalogue,
}

/// A live v1 connection the subscription is no longer entitled to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneCandidate {
    pub conn: Uuid,
    pub env: Env,
    pub proto: Tag,
}

/// Live connections that carry no right any more: `(env, proto)` is absent from
/// the granted map.
///
/// `scope_env` is the subscription's own (self-hosted / personal) env: it is
/// never granted through `subscription_access`, so pruning it would take a
/// customer's own server away. `max` caps what is returned; `found` is the real
/// number of candidates, so the caller can refuse the whole batch when a wrong
/// grant would delete too much at once.
///
/// `on_peer_plane` is the subscription's proof that its client reads cards: at
/// least one live peer. A grant alone does not prove it — activations hand
/// grants to subscriptions whose app may still be a build that knows nothing
/// but `/v1/config` (anything below 4.8.14.186), and v1 connections live in
/// `dev`/`ru`/`wl`, which no shard grants. Pruning those would leave the payer
/// with no config at all, so nothing is a candidate until a peer exists.
pub fn connections_to_prune(
    live: &[(Uuid, Env, Tag)],
    granted: &HashMap<Env, Vec<Tag>>,
    scope_env: Option<&Env>,
    on_peer_plane: bool,
    max: usize,
) -> (Vec<PruneCandidate>, usize) {
    if !on_peer_plane {
        return (Vec::new(), 0);
    }

    let mut out = Vec::new();
    let mut found = 0usize;

    for (conn, env, proto) in live {
        if Some(env) == scope_env {
            continue;
        }
        if granted
            .get(env)
            .is_some_and(|tags| tags.contains(proto))
        {
            continue;
        }
        found += 1;
        if out.len() < max {
            out.push(PruneCandidate {
                conn: *conn,
                env: env.clone(),
                proto: *proto,
            });
        }
    }

    (out, found)
}

/// The conns map the provisioning code should use: grants when the
/// subscription has any, the catalogue otherwise.
///
/// `catalog` is what the product resolver produced — returned untouched when
/// there are no grants, so a legacy subscription behaves exactly as before.
pub async fn conns_map_for(
    db: &PgContext,
    subscription_id: Uuid,
    catalog: Option<HashMap<Env, Vec<Tag>>>,
) -> Result<(Option<HashMap<Env, Vec<Tag>>>, Source)> {
    let grants = db.peers().grants(subscription_id).await?;
    if grants.is_empty() {
        return Ok((catalog, Source::Catalogue));
    }

    Ok((
        Some(grants_to_conns_map(&grants, chrono::Utc::now())),
        Source::Grants,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
    }

    fn grant(env: Env, protos: Vec<Tag>, expires_at: Option<chrono::DateTime<Utc>>) -> AccessRow {
        AccessRow {
            id: uuid::Uuid::nil(),
            plan: Some("standard".to_string()),
            env,
            protos,
            sources: vec!["standard".to_string()],
            expires_at,
            traffic_limit: None,
        }
    }

    #[test]
    fn peers_are_not_connections() {
        let map = grants_to_conns_map(
            &[grant(Env::Ru, vec![Tag::AmneziaPeer, Tag::Hysteria2], None)],
            now(),
        );
        assert_eq!(map.get(&Env::Ru), Some(&vec![Tag::Hysteria2]));
    }

    #[test]
    fn expired_grants_provision_nothing() {
        let map = grants_to_conns_map(
            &[grant(
                Env::Ru,
                vec![Tag::VlessXhttpCdn],
                Some(now() - chrono::Duration::days(1)),
            )],
            now(),
        );
        assert!(map.is_empty());
    }

    #[test]
    fn several_protocols_of_one_env_stay_together() {
        let map = grants_to_conns_map(
            &[grant(
                Env::Wl,
                vec![Tag::VlessXhttpCdn, Tag::Hysteria2],
                Some(now() + chrono::Duration::days(30)),
            )],
            now(),
        );
        assert_eq!(map.get(&Env::Wl).map(|v| v.len()), Some(2));
    }

    #[test]
    fn envs_are_kept_apart() {
        let map = grants_to_conns_map(
            &[
                grant(Env::Ru, vec![Tag::Hysteria2], None),
                grant(Env::Wl, vec![Tag::VlessXhttpCdn], None),
            ],
            now(),
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn the_own_env_of_a_subscription_is_never_pruned() {
        use std::str::FromStr;
        // A self-hosted subscription has its own env with no grant at all;
        // a migration must not take the customer's server away.
        let own = Env::from_str("custompersonalc6831dad").unwrap();
        let live = vec![(uuid::Uuid::nil(), own.clone(), Tag::Hysteria2)];
        let (candidates, found) = connections_to_prune(&live, &HashMap::new(), Some(&own), true, 50);
        assert!(candidates.is_empty());
        assert_eq!(found, 0);
    }

    #[test]
    fn granted_pairs_survive_and_the_rest_goes() {
        let granted = grants_to_conns_map(&[grant(Env::Dev, vec![Tag::Hysteria2], None)], now());
        let live = vec![
            (uuid::Uuid::nil(), Env::Dev, Tag::Hysteria2),
            (uuid::Uuid::nil(), Env::Ru, Tag::Wireguard),
        ];
        let (candidates, found) = connections_to_prune(&live, &granted, None, true, 50);
        assert_eq!(found, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].env, Env::Ru);
        assert_eq!(candidates[0].proto, Tag::Wireguard);
    }

    /// A grant is not proof that the client reads cards: activations hand it to
    /// subscriptions whose app may still only know `/v1/config`. Until the
    /// subscription has a live peer, its v1 connections are all it can load.
    #[test]
    fn a_subscription_without_a_peer_keeps_its_v1_connections() {
        let live = vec![
            (uuid::Uuid::nil(), Env::Dev, Tag::Hysteria2),
            (uuid::Uuid::nil(), Env::Ru, Tag::Wireguard),
        ];
        let (candidates, found) = connections_to_prune(&live, &HashMap::new(), None, false, 50);
        assert!(candidates.is_empty(), "nothing may be pruned before a peer exists");
        assert_eq!(found, 0);
    }

    /// And the same subscription, once on the peer plane, loses exactly what the
    /// grant does not cover.
    #[test]
    fn a_peer_moves_the_same_subscription_onto_pruning() {
        let granted = grants_to_conns_map(&[grant(Env::Dev, vec![Tag::Hysteria2], None)], now());
        let live = vec![
            (uuid::Uuid::nil(), Env::Dev, Tag::Hysteria2),
            (uuid::Uuid::nil(), Env::Ru, Tag::Wireguard),
        ];
        let (candidates, found) = connections_to_prune(&live, &granted, None, true, 50);
        assert_eq!(found, 1);
        assert_eq!(candidates[0].env, Env::Ru);
    }

    #[test]
    fn a_batch_over_the_cap_is_reported_in_full() {
        let live: Vec<(uuid::Uuid, Env, Tag)> = (0..5)
            .map(|_| (uuid::Uuid::nil(), Env::Ru, Tag::Hysteria2))
            .collect();
        let (candidates, found) = connections_to_prune(&live, &HashMap::new(), None, true, 2);
        assert_eq!(candidates.len(), 2);
        assert_eq!(found, 5);
    }
}
