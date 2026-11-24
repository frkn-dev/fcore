//! Dual-plan purchases: which plan a buyer is served by, what a purchase buys,
//! and how the cabinet renders «Standard активен / Super Duper — Докупить».
//!
//! Two plans are sold, each in its own shard: `standard` (time, unlimited
//! traffic) and `lite` (a traffic volume, titled «Super Duper» in the UI only
//! — the wire value stays `lite` so grants, shards and `env_capacity.plan`
//! keep one vocabulary). They are paid for separately and one subscription may
//! hold both, because the unit of entitlement is a *grant*
//! (`subscription_access`), not the subscription row: every plan carries its
//! own term and its own traffic limit.
//!
//! Everything here is pure — the handlers read the grants and pass them in — so
//! the two decisions that carry money («what does this purchase do to the
//! grant» and «may this plan be sold right now») are tested without a database.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use fcore::http::response::PlanStatus;

use crate::postgres::peers::AccessRow;

pub const STANDARD: &str = "standard";
/// Sold as «Super Duper»; the value itself is the `PlanKind` string.
pub const LITE: &str = "lite";

/// Plans the shop sells today, in the order the cabinet shows them.
pub const SELLABLE: [&str; 2] = [STANDARD, LITE];

/// A recorded payment reference lives in `subscription_access.sources` with
/// this prefix. Sources already mean «what bought this row» (`payment`,
/// `manual`, `support`), and the reference is exactly that — so a cabinet
/// purchase needs no new table and no schema migration, while a repeated
/// webhook is still recognised by an exact match.
pub const PAYMENT_MARKER_PREFIX: &str = "pay:";

/// Canonical plan string for a request value.
///
/// `super-duper` is accepted next to `lite` because that is the name on the
/// price list and in support threads; the stored value is always `lite`.
pub fn canonical_plan(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "standard" => Some(STANDARD),
        "lite" | "super-duper" | "superduper" => Some(LITE),
        _ => None,
    }
}

/// Human name of a plan — the single place where «lite is Super Duper» lives.
pub fn title(plan: Option<&str>) -> String {
    match plan {
        Some(STANDARD) => "Standard".to_string(),
        Some(LITE) => "Super Duper".to_string(),
        Some("gaming") => "Gaming".to_string(),
        Some("bs-prem") => "BS Premium".to_string(),
        Some(other) => other.to_string(),
        None => "Доступ".to_string(),
    }
}

/// Ledger marker of a payment reference: `pay:<reference>`.
pub fn payment_marker(reference: &str) -> String {
    format!(
        "{}{}",
        PAYMENT_MARKER_PREFIX,
        reference.trim().to_ascii_lowercase()
    )
}

/// Is this source one of our payment-reference markers (and therefore not
/// something the buyer should read)?
pub fn is_payment_marker(source: &str) -> bool {
    source.starts_with(PAYMENT_MARKER_PREFIX)
}

/// Sources to store on the grant of a purchase: the source label plus the
/// payment reference marker.
///
/// The marker **must** be in the row: it is what a repeated webhook looks for.
/// Storing it only in the dedupe query (and writing `sources = [source]`) made
/// every retry a second purchase — a real extension paid once.
pub fn purchase_sources(source: &str, marker: &str) -> Vec<String> {
    let mut sources = vec![source.to_string()];
    if !sources.iter().any(|known| known == marker) {
        sources.push(marker.to_string());
    }
    sources
}

/// Sources as the cabinet shows them: the payment reference is our ledger, not
/// a fact about the purchase.
pub fn public_sources(sources: &[String]) -> Vec<String> {
    sources
        .iter()
        .filter(|source| !is_payment_marker(source))
        .cloned()
        .collect()
}

/// What a purchase does to the plan's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurchaseTerms {
    /// Absolute term to store (`None` = no term, a traffic-only grant).
    pub expires_at: Option<DateTime<Utc>>,
    /// Absolute traffic limit to store (`None` = unlimited).
    pub traffic_limit: Option<i64>,
}

/// Terms of a purchase, given what the plan's grant already holds.
///
/// * `standard` — time plan: the new period is added to the term that is left
///   (an expired grant restarts from now), the same arithmetic `PUT
///   /subscription` and the key activation use, so the subscription row and
///   the grant never disagree. Traffic stays unlimited (`None`).
/// * `lite` — traffic plan: the bought volume is **added** to the grant's
///   limit (a second Super Duper pack is a top-up, never a reset), and the
///   existing limit stays when the purchase carries no volume. A term is
///   written only when the purchase actually carries days: Super Duper is sold
///   by volume, so `expires_at` stays `NULL` and the grant never expires.
pub fn purchase_terms(
    plan: &str,
    days: i64,
    traffic_bytes: Option<i64>,
    existing: Option<(Option<DateTime<Utc>>, Option<i64>)>,
    now: DateTime<Utc>,
) -> PurchaseTerms {
    let (existing_expiry, existing_limit) = match existing {
        Some((expiry, limit)) => (expiry, limit),
        None => (None, None),
    };

    // Extension, not "N days from now": the paid time left is never thrown
    // away, and an expired grant restarts from today.
    // `try_days`, not `days`: an absurd payload (a year count near i64::MAX)
    // must not panic the request task.
    let expires_at = match (days, Duration::try_days(days)) {
        (days, Some(period)) if days > 0 => {
            let base = match existing_expiry {
                Some(at) if at > now => at,
                _ => now,
            };
            base.checked_add_signed(period).or(existing_expiry)
        }
        _ => existing_expiry,
    };

    let traffic_limit = if plan == LITE {
        match traffic_bytes.filter(|bytes| *bytes > 0) {
            Some(add) => Some(existing_limit.unwrap_or(0) + add),
            None => existing_limit,
        }
    } else {
        // A time plan is never metered; a limit left over from a manual grant
        // would silently cut a paying user off.
        None
    };

    PurchaseTerms {
        expires_at,
        traffic_limit,
    }
}

/// Where an activation lands.
///
/// * the subscription the caller named (the cabinet/app knows the user);
/// * otherwise the one the key itself was issued against (a key bought while
///   the buyer was identified carries it);
/// * otherwise `None` — and only then does activation create a new
///   subscription, exactly as it always did.
///
/// This is the whole merge rule: buying a second plan must add a grant to the
/// subscription that already exists, never create a second subscription for
/// the same person.
pub fn activation_target(
    requested: Option<uuid::Uuid>,
    key_bound: Option<uuid::Uuid>,
) -> Option<uuid::Uuid> {
    requested.or(key_bound)
}

/// Facts the subscription row itself carries. Used only as the fallback for a
/// plan that has no grant yet — the legacy world, where the subscription (not
/// a grant) held the term and the traffic balance.
#[derive(Clone, Copy, Debug)]
pub struct SubFacts {
    pub plan_kind: fcore::PlanKind,
    pub expires_at: Option<DateTime<Utc>>,
    pub limit_bytes: Option<i64>,
    pub used_bytes: i64,
}

/// A grant is active while its own clock and quota allow it.
pub fn grant_is_active(grant: &AccessRow, used_bytes: i64, now: DateTime<Utc>) -> bool {
    if grant.expires_at.is_some_and(|at| at <= now) {
        return false;
    }
    match grant.traffic_limit {
        Some(limit) => limit <= 0 || used_bytes < limit,
        None => true,
    }
}

/// Per-plan view for the cabinet and the app.
///
/// Both sold plans are always listed (`active: false` for the one not bought
/// yet), plus any other plan a grant carries — a product granted by support
/// must not disappear from the list. A plan without a grant falls back to the
/// subscription row only when the subscription has no grants at all: that is
/// the legacy shape, and hiding it would tell a paying v1 user they own
/// nothing.
pub fn statuses(
    sub: Option<&SubFacts>,
    grants: &[AccessRow],
    used_by_env: &HashMap<String, i64>,
    purchasable: &HashMap<String, bool>,
    now: DateTime<Utc>,
) -> Vec<PlanStatus> {
    let mut plans: Vec<String> = SELLABLE.iter().map(|plan| plan.to_string()).collect();
    for grant in grants {
        let Some(plan) = grant.plan.as_deref() else {
            continue;
        };
        if !plans.iter().any(|known| known == plan) {
            plans.push(plan.to_string());
        }
    }

    let legacy_only = grants.is_empty();
    plans
        .into_iter()
        .map(|plan| {
            let grant = grants
                .iter()
                .find(|grant| grant.plan.as_deref() == Some(plan.as_str()));
            let used = grant
                .map(|grant| {
                    used_by_env
                        .get(&grant.env.to_string())
                        .copied()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            let legacy = legacy_only
                .then_some(sub)
                .flatten()
                .filter(|sub| sub.plan_kind.to_string() == plan)
                .filter(|sub| match sub.expires_at {
                    Some(at) => at > now,
                    None => true,
                });

            let (active, expires_at, traffic_used, traffic_limit) = match (grant, legacy) {
                (Some(grant), _) => (
                    grant_is_active(grant, used, now),
                    grant.expires_at,
                    used,
                    grant.traffic_limit,
                ),
                (None, Some(legacy)) => (
                    true,
                    legacy.expires_at,
                    legacy.used_bytes,
                    legacy.limit_bytes,
                ),
                (None, None) => (false, None, 0, None),
            };

            PlanStatus {
                title: title(Some(plan.as_str())),
                active,
                expires_at,
                traffic_used,
                traffic_limit,
                purchasable: purchasable.get(plan.as_str()).copied().unwrap_or(false),
                plan,
            }
        })
        .collect()
}

/// Why a plan cannot be sold right now, as the stable error code the cabinet
/// and the app switch on.
pub fn sale_refusal(declared_shards: usize, live_shards: usize) -> Option<&'static str> {
    if declared_shards == 0 {
        // Nobody declared a shard for this plan: selling it would take money
        // for access the api has nowhere to put. Never fall back to another
        // plan's shard — that is how a Super Duper buyer ends up on 0001.
        Some("plan_not_configured")
    } else if live_shards == 0 {
        // Declared, but no machine (or no address space) serves it today.
        Some("no_live_shard")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcore::{Env, Tag};
    use uuid::Uuid;

    const GIB: i64 = 1024 * 1024 * 1024;

    fn grant(
        plan: &str,
        env: Env,
        expires_at: Option<DateTime<Utc>>,
        limit: Option<i64>,
    ) -> AccessRow {
        AccessRow {
            id: Uuid::new_v4(),
            env,
            plan: Some(plan.to_string()),
            protos: vec![Tag::AmneziaPeer],
            sources: vec!["payment".to_string()],
            expires_at,
            traffic_limit: limit,
        }
    }

    fn sub(plan_kind: fcore::PlanKind, expires_at: Option<DateTime<Utc>>) -> SubFacts {
        SubFacts {
            plan_kind,
            expires_at,
            limit_bytes: None,
            used_bytes: 0,
        }
    }

    #[test]
    fn plan_names_accept_the_super_duper_alias() {
        assert_eq!(canonical_plan("standard"), Some(STANDARD));
        assert_eq!(canonical_plan(" Lite "), Some(LITE));
        assert_eq!(canonical_plan("super-duper"), Some(LITE));
        assert_eq!(canonical_plan("super_duper"), Some(LITE));
        assert_eq!(canonical_plan("SuperDuper"), Some(LITE));
        assert_eq!(canonical_plan("premium"), None);
        assert_eq!(title(Some(LITE)), "Super Duper");
        assert_eq!(title(Some(STANDARD)), "Standard");
    }

    #[test]
    fn a_standard_purchase_extends_the_term_and_stays_unmetered() {
        let now = Utc::now();
        // First purchase: a term from today, no traffic cap.
        let first = purchase_terms(STANDARD, 30, None, None, now);
        assert_eq!(first.expires_at, Some(now + Duration::days(30)));
        assert_eq!(first.traffic_limit, None);

        // Renewal keeps the time that is left: 10 + 30, not 30.
        let left = Some(now + Duration::days(10));
        let renewal = purchase_terms(STANDARD, 30, None, Some((left, None)), now);
        assert_eq!(renewal.expires_at, Some(now + Duration::days(40)));

        // An expired grant restarts from today.
        let expired = Some(now - Duration::days(5));
        let restarted = purchase_terms(STANDARD, 30, None, Some((expired, Some(GIB))), now);
        assert_eq!(restarted.expires_at, Some(now + Duration::days(30)));
        // And a time plan never keeps a traffic cap — that would cut a payer off.
        assert_eq!(restarted.traffic_limit, None);
    }

    #[test]
    fn a_super_duper_purchase_adds_traffic_and_does_not_create_a_term() {
        let now = Utc::now();
        let first = purchase_terms(LITE, 0, Some(100 * GIB), None, now);
        assert_eq!(first.traffic_limit, Some(100 * GIB));
        assert_eq!(
            first.expires_at, None,
            "a volume plan does not expire by time"
        );

        // Second pack is a top-up, not a reset.
        let second = purchase_terms(LITE, 0, Some(100 * GIB), Some((None, Some(100 * GIB))), now);
        assert_eq!(second.traffic_limit, Some(200 * GIB));
        assert_eq!(second.expires_at, None);

        // No volume in the payload: the bought limit is left alone.
        let nothing = purchase_terms(LITE, 0, None, Some((None, Some(100 * GIB))), now);
        assert_eq!(nothing.traffic_limit, Some(100 * GIB));

        // A period is honoured when one is sold with the plan.
        let with_term = purchase_terms(LITE, 30, Some(GIB), Some((None, None)), now);
        assert_eq!(with_term.expires_at, Some(now + Duration::days(30)));
    }

    #[test]
    fn both_plans_are_listed_with_their_own_flags() {
        let now = Utc::now();
        let sub_facts = sub(fcore::PlanKind::Standard, Some(now + Duration::days(20)));
        let grants = vec![grant(
            STANDARD,
            Env::Ru,
            Some(now + Duration::days(20)),
            None,
        )];
        let used = HashMap::from([("ru".to_string(), 5 * GIB)]);
        let purchasable = HashMap::from([(STANDARD.to_string(), true), (LITE.to_string(), true)]);

        let plans = statuses(Some(&sub_facts), &grants, &used, &purchasable, now);
        assert_eq!(plans.len(), 2, "both sold plans are always listed");
        assert_eq!(plans[0].plan, STANDARD);
        assert_eq!(plans[0].title, "Standard");
        assert!(plans[0].active);
        assert_eq!(plans[0].traffic_used, 5 * GIB);
        assert_eq!(plans[0].traffic_limit, None, "standard is unlimited");
        assert!(plans[0].purchasable);

        // Not bought yet: no grant, no legacy fallback (the sub has a grant).
        assert_eq!(plans[1].plan, LITE);
        assert_eq!(plans[1].title, "Super Duper");
        assert!(!plans[1].active);
        assert_eq!(plans[1].traffic_limit, None);
        // Still buyable — that is exactly the «Докупить Super Duper» button.
        assert!(plans[1].purchasable);
    }

    #[test]
    fn a_super_duper_grant_reports_its_own_term_and_quota() {
        let now = Utc::now();
        let grants = vec![
            grant(STANDARD, Env::Ru, Some(now + Duration::days(10)), None),
            grant(LITE, Env::Wl, None, Some(100 * GIB)),
        ];
        let used = HashMap::from([("ru".to_string(), 0), ("wl".to_string(), 30 * GIB)]);
        let purchasable = HashMap::new();

        let plans = statuses(None, &grants, &used, &purchasable, now);
        assert!(
            plans[0].active && plans[1].active,
            "one subscription holds both plans"
        );
        assert_eq!(plans[1].traffic_used, 30 * GIB);
        assert_eq!(plans[1].traffic_limit, Some(100 * GIB));
        assert_eq!(plans[1].expires_at, None);
        assert!(
            !plans[1].purchasable,
            "unknown availability is not sellable"
        );
    }

    #[test]
    fn an_expired_or_spent_grant_is_not_active() {
        let now = Utc::now();
        let grants = vec![
            grant(STANDARD, Env::Ru, Some(now - Duration::days(1)), None),
            grant(LITE, Env::Wl, None, Some(10 * GIB)),
        ];
        let used = HashMap::from([("ru".to_string(), 0), ("wl".to_string(), 10 * GIB)]);
        let plans = statuses(None, &grants, &used, &HashMap::new(), now);
        assert!(!plans[0].active, "the term is over");
        assert!(!plans[1].active, "the quota is spent");
        // The dates and the numbers stay visible: the user must see what to buy.
        assert_eq!(plans[0].expires_at, Some(now - Duration::days(1)));
        assert_eq!(plans[1].traffic_used, 10 * GIB);
    }

    #[test]
    fn a_legacy_subscription_without_grants_keeps_its_plan_active() {
        let now = Utc::now();
        let mut sub_facts = sub(fcore::PlanKind::Lite, None);
        sub_facts.limit_bytes = Some(50 * GIB);
        sub_facts.used_bytes = 5 * GIB;

        let plans = statuses(Some(&sub_facts), &[], &HashMap::new(), &HashMap::new(), now);
        assert_eq!(plans.len(), 2);
        assert!(
            !plans[0].active,
            "a lite subscription does not own standard"
        );
        assert!(plans[1].active, "the v1 lite subscription is live");
        assert_eq!(plans[1].traffic_limit, Some(50 * GIB));
        assert_eq!(plans[1].traffic_used, 5 * GIB);
    }

    #[test]
    fn an_unknown_plan_of_a_grant_is_appended_not_hidden() {
        let now = Utc::now();
        let grants = vec![grant(
            "gaming",
            Env::BsPrem,
            Some(now + Duration::days(3)),
            None,
        )];
        let plans = statuses(None, &grants, &HashMap::new(), &HashMap::new(), now);
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[2].plan, "gaming");
        assert_eq!(plans[2].title, "Gaming");
        assert!(plans[2].active);
    }

    #[test]
    fn a_plan_without_a_declared_shard_is_refused_not_redirected() {
        assert_eq!(sale_refusal(0, 0), Some("plan_not_configured"));
        assert_eq!(sale_refusal(1, 0), Some("no_live_shard"));
        assert_eq!(sale_refusal(1, 2), None);
    }

    #[test]
    fn a_second_plan_lands_on_the_existing_subscription() {
        use crate::peers::store::AccessStore;

        let sub = Uuid::new_v4();

        // The caller names the subscription: the key's plan is added to it.
        assert_eq!(
            activation_target(Some(sub), None),
            Some(sub),
            "an identified account keeps its subscription"
        );
        // A key issued against a subscription lands there too.
        assert_eq!(activation_target(None, Some(sub)), Some(sub));
        // The named one wins when a key carries a stale binding.
        let other = Uuid::new_v4();
        assert_eq!(activation_target(Some(sub), Some(other)), Some(sub));
        // Nobody is identified: a new subscription, as before.
        assert_eq!(activation_target(None, None), None);

        // What the merged write looks like: one subscription, one grant per
        // plan, both live.
        let mut store = AccessStore::load(Vec::new());
        store.insert(sub, grant(STANDARD, Env::Ru, Some(Utc::now()), None));
        store.insert(sub, grant(LITE, Env::Wl, None, Some(10 * GIB)));
        assert_eq!(store.of_sub(&sub).len(), 2, "two plans, one subscription");
        assert!(store.get(&sub, &Env::Ru).is_some());
        assert!(store.get(&sub, &Env::Wl).is_some());
    }

    #[test]
    fn a_purchase_records_its_payment_reference() {
        let marker = payment_marker("TX-42");
        let sources = purchase_sources("payment", &marker);
        assert_eq!(
            sources,
            vec!["payment".to_string(), "pay:tx-42".to_string()]
        );
        // The row must carry the marker, or a repeated webhook buys again.
        assert!(sources.iter().any(|source| source == &marker));
        // Already recorded: no duplicate entry.
        assert_eq!(purchase_sources(&marker, &marker).len(), 1);
        // And the buyer never sees it.
        assert_eq!(public_sources(&sources), vec!["payment".to_string()]);
    }

    #[test]
    fn the_payment_reference_is_a_hidden_source() {
        let marker = payment_marker(" 3F1C-ABC ");
        assert_eq!(marker, "pay:3f1c-abc");
        assert!(is_payment_marker(&marker));
        let sources = vec!["payment".to_string(), marker.clone()];
        assert_eq!(public_sources(&sources), vec!["payment".to_string()]);
    }
}
