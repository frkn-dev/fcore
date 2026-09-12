use chrono::{DateTime, Utc};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use std::ops::Deref;
use std::ops::DerefMut;

use crate::memory::env::Env;
use crate::utils::get_uuid_last_octet_simple;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Billing plan of a subscription. Standard plans expire by time; a lite
/// subscription has no time expiry (expires_at IS NULL) and lives while it
/// has traffic left. Premium is reserved, not used yet.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanKind {
    #[default]
    Standard,
    Lite,
    Premium,
}

impl fmt::Display for PlanKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanKind::Standard => write!(f, "standard"),
            PlanKind::Lite => write!(f, "lite"),
            PlanKind::Premium => write!(f, "premium"),
        }
    }
}

impl FromStr for PlanKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "standard" => Ok(PlanKind::Standard),
            "lite" => Ok(PlanKind::Lite),
            "premium" => Ok(PlanKind::Premium),
            _ => Err(format!("Wrong PlanKind string: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub id: uuid::Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub refer_code: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_deleted: bool,
    pub parent_id: Option<uuid::Uuid>,
    pub scope_env: Option<Env>,
    pub premium_token: Option<String>,
    pub plan_kind: PlanKind,

    pub limit_bytes: Option<i64>,
}

impl Subscription {
    pub fn new(
        id: uuid::Uuid,
        ref_code: String,
        exp_at: Option<DateTime<Utc>>,
        limit_bytes: Option<i64>,
    ) -> Subscription {
        let now = Utc::now();
        Self {
            id,
            expires_at: exp_at,
            refer_code: ref_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            parent_id: None,
            scope_env: None,
            premium_token: None,
            plan_kind: PlanKind::Standard,

            limit_bytes,
        }
    }

    /// Creates a lite (traffic-only) subscription: no time expiry
    /// (expires_at stays NULL — the sub is active while it has traffic),
    /// the traffic limit comes from the lite key.
    pub fn new_lite(id: uuid::Uuid, ref_code: String, limit_bytes: i64) -> Subscription {
        let now = Utc::now();
        Self {
            id,
            expires_at: None,
            refer_code: ref_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            parent_id: None,
            scope_env: None,
            premium_token: None,
            plan_kind: PlanKind::Lite,

            limit_bytes: Some(limit_bytes),
        }
    }
}

impl Default for Subscription {
    fn default() -> Self {
        let now = Utc::now();
        let id = uuid::Uuid::new_v4();

        let refer_code = get_uuid_last_octet_simple(&id);

        Self {
            id,
            expires_at: None,
            refer_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            parent_id: None,
            scope_env: None,
            premium_token: None,
            plan_kind: PlanKind::Standard,
            limit_bytes: None,
        }
    }
}

impl From<tokio_postgres::Row> for Subscription {
    fn from(row: tokio_postgres::Row) -> Self {
        let expires_at: Option<DateTime<Utc>> = row.get("expires_at");
        let created_at: DateTime<Utc> = row.get::<_, DateTime<Utc>>("created_at");
        let updated_at: DateTime<Utc> = row.get::<_, DateTime<Utc>>("updated_at");

        let limit_bytes: Option<i64> = row.get("limit_bytes");

        Self {
            id: row.get("id"),
            expires_at,
            refer_code: row.get("refer_code"),
            created_at,
            updated_at,
            is_deleted: row.get::<_, bool>("is_deleted"),
            parent_id: row.get("parent_id"),
            scope_env: row
                .try_get::<_, String>("scope_env")
                .ok()
                .and_then(|s| if s.is_empty() { None } else { Some(Env::from(s.as_str())) }),
            premium_token: row.get("premium_token"),
            plan_kind: row
                .try_get::<_, String>("plan_kind")
                .ok()
                .and_then(|s| PlanKind::from_str(&s).ok())
                .unwrap_or_default(),
            limit_bytes,
        }
    }
}

#[derive(
    Archive, PartialEq, Deserialize, Serialize, RkyvDeserialize, RkyvSerialize, Debug, Clone,
)]
#[archive(check_bytes)]
pub struct Subscriptions<S>(pub HashMap<uuid::Uuid, S>);

impl<S> Default for Subscriptions<S> {
    fn default() -> Self {
        Subscriptions(HashMap::new())
    }
}

impl<S: fmt::Display> fmt::Display for Subscriptions<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (id, sub) in &self.0 {
            writeln!(f, "{} => {}", id, sub)?;
        }
        Ok(())
    }
}

impl<S> Deref for Subscriptions<S> {
    type Target = HashMap<uuid::Uuid, S>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S> DerefMut for Subscriptions<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionStats {
    pub id: uuid::Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: i64,
    pub is_active: bool,
}

pub trait Operations {
    fn extend(&mut self, days: i64);
    fn id(&self) -> uuid::Uuid;
    fn expires_at(&self) -> Option<DateTime<Utc>>;
    fn created_at(&self) -> DateTime<Utc>;
    fn refer_code(&self) -> String;
    fn set_refer_code(&mut self, code: String);
    fn is_active(&self) -> bool;
    fn is_deleted(&self) -> bool;
    fn days_remaining(&self) -> Option<i64>;
    fn set_expires_at(&mut self, expires_at: DateTime<Utc>) -> Result<(), String>;
    fn mark_deleted(&mut self);
    fn stats(&self) -> SubscriptionStats;

    fn limit_bytes(&self) -> Option<i64>;
    fn set_limit_bytes(&mut self, bytes: i64);

    fn plan_kind(&self) -> PlanKind;

    fn parent_id(&self) -> Option<uuid::Uuid>;
    fn set_parent_id(&mut self, parent_id: uuid::Uuid);

    fn scope_env(&self) -> Option<&Env>;
    fn set_scope_env(&mut self, env: Env);

    fn premium_token(&self) -> Option<&str>;
    fn set_premium_token(&mut self, token: String);

    /// Bytes left on the traffic balance. None when the subscription has no
    /// traffic limit at all.
    fn remaining_bytes(&self, used_bytes: i64) -> Option<i64> {
        self.limit_bytes().map(|limit| limit - used_bytes)
    }

    /// The paid time has run out. A lite subscription (expires_at NULL) is
    /// never time-expired — it lives on its traffic balance.
    fn time_expired(&self) -> bool {
        self.expires_at()
            .map(|expires_at| expires_at <= Utc::now())
            .unwrap_or(false)
    }

    /// Traffic mode: the paid time is over and the subscription lives on its
    /// remaining traffic balance. Always false when the feature flag is off.
    fn traffic_mode(&self, traffic_mode_enabled: bool, used_bytes: i64) -> bool {
        traffic_mode_enabled
            && self.time_expired()
            && self
                .remaining_bytes(used_bytes)
                .map(|remaining| remaining > 0)
                .unwrap_or(false)
    }

    /// is_active extended with the traffic balance: with the feature flag on
    /// a subscription is active while it has paid time left OR bytes left.
    /// With the flag off this is exactly `is_active`.
    fn is_active_with_traffic(&self, traffic_mode_enabled: bool, used_bytes: i64) -> bool {
        if !traffic_mode_enabled {
            return self.is_active();
        }
        if self.is_deleted() {
            return false;
        }
        if self
            .expires_at()
            .map(|expires_at| expires_at > Utc::now())
            .unwrap_or(false)
        {
            return true;
        }
        match self.remaining_bytes(used_bytes) {
            Some(remaining) => remaining > 0,
            // Lite (expires_at NULL) without a limit keeps the legacy
            // NULL-means-active semantics.
            None => self.expires_at().is_none(),
        }
    }

    /// Whether a traffic top-up is allowed: always for lite subscriptions;
    /// for standard ones only in traffic mode once the paid time has expired
    /// (buying traffic on an active standard subscription is rejected).
    fn top_up_allowed(&self, traffic_mode_enabled: bool) -> bool {
        match self.plan_kind() {
            PlanKind::Lite => true,
            PlanKind::Standard => traffic_mode_enabled && self.time_expired(),
            PlanKind::Premium => false,
        }
    }
}

impl Operations for Subscription {
    fn stats(&self) -> SubscriptionStats {
        let now = Utc::now();
        let days_remaining = if let Some(expires_at) = self.expires_at {
            (expires_at - now).num_days()
        } else {
            99999
        };

        SubscriptionStats {
            id: self.id,
            expires_at: self.expires_at,
            days_remaining,
            is_active: days_remaining > 0 && !self.is_deleted,
        }
    }
    fn extend(&mut self, days: i64) {
        let now = Utc::now();
        let base = match self.expires_at {
            Some(exp) if exp > now => exp,
            _ => now,
        };
        self.expires_at = Some(base + chrono::Duration::days(days));
        self.updated_at = Utc::now();
    }

    fn id(&self) -> uuid::Uuid {
        self.id
    }

    fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    fn refer_code(&self) -> String {
        self.refer_code.trim().to_string()
    }
    fn set_refer_code(&mut self, code: String) {
        self.refer_code = code;
    }

    fn is_active(&self) -> bool {
        !self.is_deleted && self.expires_at.map(|expires_at| expires_at > Utc::now()).unwrap_or(true)
    }

    fn is_deleted(&self) -> bool {
        self.is_deleted
    }

    fn days_remaining(&self) -> Option<i64> {
        let now = Utc::now();
        self.expires_at
            .map(|expires_at| (expires_at - now).num_days())
    }

    fn set_expires_at(&mut self, expires_at: DateTime<Utc>) -> Result<(), String> {
        if expires_at <= Utc::now() {
            return Err("Expiration date must be in the future".to_string());
        }
        self.expires_at = Some(expires_at);
        self.updated_at = Utc::now();
        Ok(())
    }

    fn mark_deleted(&mut self) {
        self.is_deleted = true;
        self.updated_at = Utc::now();
    }

    fn limit_bytes(&self) -> Option<i64> {
        self.limit_bytes
    }

    fn set_limit_bytes(&mut self, bytes: i64) {
        self.limit_bytes = Some(bytes)
    }

    fn plan_kind(&self) -> PlanKind {
        self.plan_kind
    }

    fn parent_id(&self) -> Option<uuid::Uuid> {
        self.parent_id
    }

    fn set_parent_id(&mut self, parent_id: uuid::Uuid) {
        self.parent_id = Some(parent_id);
        self.updated_at = Utc::now();
    }

    fn scope_env(&self) -> Option<&Env> {
        self.scope_env.as_ref()
    }

    fn set_scope_env(&mut self, env: Env) {
        self.scope_env = Some(env);
        self.updated_at = Utc::now();
    }

    fn premium_token(&self) -> Option<&str> {
        self.premium_token.as_deref()
    }

    fn set_premium_token(&mut self, token: String) {
        self.premium_token = Some(token);
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_kind_string_roundtrip() {
        for (kind, s) in [
            (PlanKind::Standard, "standard"),
            (PlanKind::Lite, "lite"),
            (PlanKind::Premium, "premium"),
        ] {
            assert_eq!(kind.to_string(), s);
            assert_eq!(PlanKind::from_str(s).unwrap(), kind);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", s));
            assert_eq!(
                serde_json::from_str::<PlanKind>(&format!("\"{}\"", s)).unwrap(),
                kind
            );
        }

        assert!(PlanKind::from_str("trial").is_err());
        assert_eq!(PlanKind::default(), PlanKind::Standard);
    }

    #[test]
    fn test_new_subscription_is_standard() {
        let sub = Subscription::new(uuid::Uuid::new_v4(), "ref".to_string(), None, None);

        assert_eq!(sub.plan_kind(), PlanKind::Standard);
    }

    #[test]
    fn test_new_lite_subscription() {
        let sub = Subscription::new_lite(uuid::Uuid::new_v4(), "ref".to_string(), 1024);

        assert_eq!(sub.plan_kind(), PlanKind::Lite);
        assert_eq!(sub.expires_at(), None);
        assert_eq!(sub.limit_bytes(), Some(1024));
        // A lite sub with expires_at NULL is active: it lives while it has
        // traffic, so the cleanup task must never treat it as expired.
        assert!(sub.is_active());
    }

    fn sub_with(expires_at: Option<DateTime<Utc>>, limit_bytes: Option<i64>) -> Subscription {
        Subscription::new(uuid::Uuid::new_v4(), "ref".to_string(), expires_at, limit_bytes)
    }

    #[test]
    fn test_remaining_bytes() {
        let sub = sub_with(None, Some(1000));
        assert_eq!(sub.remaining_bytes(400), Some(600));
        assert_eq!(sub.remaining_bytes(1000), Some(0));
        // Overdraft is reported as negative, not clamped.
        assert_eq!(sub.remaining_bytes(1200), Some(-200));

        let no_limit = sub_with(None, None);
        assert_eq!(no_limit.remaining_bytes(0), None);
    }

    #[test]
    fn test_time_expired() {
        let future = sub_with(Some(Utc::now() + chrono::Duration::days(1)), None);
        assert!(!future.time_expired());

        let past = sub_with(Some(Utc::now() - chrono::Duration::days(1)), None);
        assert!(past.time_expired());

        // Lite (expires_at NULL) is never time-expired.
        let lite = sub_with(None, Some(100));
        assert!(!lite.time_expired());
    }

    #[test]
    fn test_traffic_mode() {
        let expired_with_balance =
            sub_with(Some(Utc::now() - chrono::Duration::days(1)), Some(1000));

        // Flag off: never in traffic mode.
        assert!(!expired_with_balance.traffic_mode(false, 100));
        // Flag on, expired, balance left: traffic mode.
        assert!(expired_with_balance.traffic_mode(true, 100));
        // No balance left: not in traffic mode.
        assert!(!expired_with_balance.traffic_mode(true, 1000));
        // Active paid time: not traffic mode even with a limit.
        let active_with_limit =
            sub_with(Some(Utc::now() + chrono::Duration::days(1)), Some(1000));
        assert!(!active_with_limit.traffic_mode(true, 0));
        // Lite (expires_at NULL) is not "traffic mode" — it has no paid time.
        let lite = sub_with(None, Some(1000));
        assert!(!lite.traffic_mode(true, 0));
    }

    #[test]
    fn test_is_active_with_traffic_flag_off_matches_legacy() {
        let expired = sub_with(Some(Utc::now() - chrono::Duration::days(1)), Some(1000));
        assert!(!expired.is_active());
        assert!(!expired.is_active_with_traffic(false, 0));

        let lite = sub_with(None, Some(100));
        assert!(lite.is_active());
        assert!(lite.is_active_with_traffic(false, 100));

        let active = sub_with(Some(Utc::now() + chrono::Duration::days(1)), None);
        assert!(active.is_active());
        assert!(active.is_active_with_traffic(false, 0));
    }

    #[test]
    fn test_is_active_with_traffic_flag_on() {
        // Expired but has balance: active.
        let expired = sub_with(Some(Utc::now() - chrono::Duration::days(1)), Some(1000));
        assert!(expired.is_active_with_traffic(true, 100));
        // Expired and exhausted: inactive.
        assert!(!expired.is_active_with_traffic(true, 1000));
        // Expired without a limit: inactive (nothing to live on).
        let expired_no_limit = sub_with(Some(Utc::now() - chrono::Duration::days(1)), None);
        assert!(!expired_no_limit.is_active_with_traffic(true, 0));
        // Active paid time: active even over the limit (balance is for later).
        let active = sub_with(Some(Utc::now() + chrono::Duration::days(1)), Some(100));
        assert!(active.is_active_with_traffic(true, 1000));
        // Lite lives while it has traffic.
        let lite = sub_with(None, Some(100));
        assert!(lite.is_active_with_traffic(true, 99));
        assert!(!lite.is_active_with_traffic(true, 100));
        // Lite without a limit keeps legacy NULL-means-active semantics.
        let lite_no_limit = sub_with(None, None);
        assert!(lite_no_limit.is_active_with_traffic(true, 0));
        // Deleted stays deleted.
        let mut deleted = sub_with(None, Some(100));
        deleted.mark_deleted();
        assert!(!deleted.is_active_with_traffic(true, 0));
    }

    #[test]
    fn test_top_up_allowed() {
        // Lite: always allowed, flag on or off.
        let lite = Subscription::new_lite(uuid::Uuid::new_v4(), "ref".to_string(), 100);
        assert!(lite.top_up_allowed(false));
        assert!(lite.top_up_allowed(true));

        let active_standard = sub_with(Some(Utc::now() + chrono::Duration::days(1)), None);
        let expired_standard = sub_with(Some(Utc::now() - chrono::Duration::days(1)), None);

        // Flag off: standard never eligible (legacy behavior).
        assert!(!active_standard.top_up_allowed(false));
        assert!(!expired_standard.top_up_allowed(false));

        // Flag on: only an expired standard sub may buy traffic.
        assert!(!active_standard.top_up_allowed(true));
        assert!(expired_standard.top_up_allowed(true));

        let mut premium = sub_with(Some(Utc::now() - chrono::Duration::days(1)), None);
        premium.plan_kind = PlanKind::Premium;
        assert!(!premium.top_up_allowed(true));
    }
}
