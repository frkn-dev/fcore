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

/// The conns map the provisioning code should use: grants when the
/// subscription has any, the catalogue otherwise.
///
/// `catalog` is what the product resolver produced — returned untouched when
/// there are no grants, so a legacy subscription behaves exactly as before.
pub async fn conns_map_for(
    db: &PgContext,
    subscription_id: Uuid,
    catalog: Option<HashMap<Env, Vec<Tag>>>,
) -> Result<Option<HashMap<Env, Vec<Tag>>>> {
    let grants = db.peers().grants(subscription_id).await?;
    if grants.is_empty() {
        return Ok(catalog);
    }

    Ok(Some(grants_to_conns_map(&grants, chrono::Utc::now())))
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
}
