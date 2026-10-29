use chrono::{DateTime, Utc};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use std::ops::Deref;
use std::ops::DerefMut;

use crate::memory::env::Env;
use crate::memory::product::Product;
use crate::utils::get_uuid_last_octet_simple;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Billing plan of a subscription. Standard plans expire by time; a lite
/// subscription has no time expiry (expires_at IS NULL) and lives while it
/// has traffic left. Premium is reserved, not used yet. Selfhosted is a
/// user-hosted account: no time expiry (expires_at IS NULL), a personal env
/// scope and no shared connections — it only ever holds the nodes the user
/// registered via an install token.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanKind {
    #[default]
    Standard,
    Lite,
    Premium,
    Selfhosted,
}

impl fmt::Display for PlanKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanKind::Standard => write!(f, "standard"),
            PlanKind::Lite => write!(f, "lite"),
            PlanKind::Premium => write!(f, "premium"),
            PlanKind::Selfhosted => write!(f, "selfhosted"),
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
            "selfhosted" => Ok(PlanKind::Selfhosted),
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
    /// Premium term, independent of `expires_at`: a premium slot may run out
    /// before the subscription does (and vice versa), so the two dates are
    /// stored separately. NULL means "no term set".
    #[serde(default)]
    pub premium_until: Option<DateTime<Utc>>,
    pub plan_kind: PlanKind,

    /// Selfhosted caps (PG columns `server_limit` / `user_limit`): how many
    /// own nodes ("servers", offline included) and how many child
    /// subscriptions ("users") the account may hold. Only meaningful for
    /// `plan_kind = selfhosted`; other plans keep the defaults and never
    /// read them.
    #[serde(default = "default_server_limit")]
    pub server_limit: i64,
    #[serde(default = "default_user_limit")]
    pub user_limit: i64,

    pub limit_bytes: Option<i64>,

    /// Tariff product of the subscription (see [`Product`]). `legacy` for
    /// everything created before products existed: access stays exactly
    /// `enabled_conns`. A renewal with a key of a different product is
    /// rejected, so this value never changes after activation.
    #[serde(default)]
    pub product: Product,
}

/// Default caps of a selfhosted account (new accounts, old rows without the
/// columns, non-selfhosted plans).
pub const DEFAULT_SERVER_LIMIT: i64 = 3;
pub const DEFAULT_USER_LIMIT: i64 = 20;

fn default_server_limit() -> i64 {
    DEFAULT_SERVER_LIMIT
}

fn default_user_limit() -> i64 {
    DEFAULT_USER_LIMIT
}

/// Caps of a selfhosted account as a pair, for the cabinet state endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfhostedLimits {
    pub server_limit: i64,
    pub user_limit: i64,
}

impl Default for SelfhostedLimits {
    fn default() -> Self {
        Self {
            server_limit: DEFAULT_SERVER_LIMIT,
            user_limit: DEFAULT_USER_LIMIT,
        }
    }
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
            premium_until: None,
            plan_kind: PlanKind::Standard,

            server_limit: DEFAULT_SERVER_LIMIT,
            user_limit: DEFAULT_USER_LIMIT,

            limit_bytes,
            product: Product::Standard,
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
            premium_until: None,
            plan_kind: PlanKind::Lite,

            server_limit: DEFAULT_SERVER_LIMIT,
            user_limit: DEFAULT_USER_LIMIT,

            limit_bytes: Some(limit_bytes),
            product: Product::Standard,
        }
    }

    /// Creates a selfhosted (user-hosted) account: no time expiry
    /// (expires_at stays NULL), a personal env scope and no shared
    /// connections. It only ever holds nodes the user registered via an
    /// install token.
    pub fn new_selfhosted(id: uuid::Uuid, ref_code: String, scope_env: Env) -> Subscription {
        let now = Utc::now();
        Self {
            id,
            expires_at: None,
            refer_code: ref_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            parent_id: None,
            scope_env: Some(scope_env),
            premium_token: None,
            premium_until: None,
            plan_kind: PlanKind::Selfhosted,

            server_limit: DEFAULT_SERVER_LIMIT,
            user_limit: DEFAULT_USER_LIMIT,

            limit_bytes: None,
            product: Product::Standard,
        }
    }

    /// A "user" of a selfhosted account: a child subscription (`parent_id` =
    /// the account) that carries only the account's own nodes.
    ///
    /// It has no own servers or users (`0/0` caps), never expires on its own
    /// (it lives exactly as long as the owner keeps it) and is
    /// `PlanKind::Selfhosted`, so the shared-connection reconcile skips it:
    /// a user never receives frkn-shared configs, only the owner's nodes.
    pub fn new_user_child(parent_id: uuid::Uuid) -> Subscription {
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
            parent_id: Some(parent_id),
            // The personal env of a user is the owner's: derived from the
            // owner id, never stored, so a user can never host its own nodes.
            scope_env: None,
            premium_token: None,
            premium_until: None,
            plan_kind: PlanKind::Selfhosted,

            server_limit: 0,
            user_limit: 0,

            limit_bytes: None,
            product: Product::Standard,
        }
    }

    /// Whether this subscription is a user (child) of a selfhosted account.
    pub fn is_selfhosted_user(&self) -> bool {
        self.parent_id.is_some() && self.plan_kind == PlanKind::Selfhosted
    }

    /// Attaches the tariff product. Called by the activation flow before the
    /// subscription is stored; a renewal can never change it (keys of another
    /// product are rejected before expiry/consumption change).
    pub fn with_product(mut self, product: Product) -> Subscription {
        self.product = product;
        self
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
            premium_until: None,
            plan_kind: PlanKind::Standard,
            server_limit: DEFAULT_SERVER_LIMIT,
            user_limit: DEFAULT_USER_LIMIT,
            limit_bytes: None,
            product: Product::Standard,
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
            scope_env: row.try_get::<_, String>("scope_env").ok().and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    Some(Env::from(s.as_str()))
                }
            }),
            premium_token: row.get("premium_token"),
            premium_until: row.get("premium_until"),
            plan_kind: row
                .try_get::<_, String>("plan_kind")
                .ok()
                .and_then(|s| PlanKind::from_str(&s).ok())
                .unwrap_or_default(),
            // Tolerate both INT4 and BIGINT and an unmigrated schema.
            server_limit: row
                .try_get::<_, i32>("server_limit")
                .map(i64::from)
                .or_else(|_| row.try_get::<_, i64>("server_limit"))
                .unwrap_or(DEFAULT_SERVER_LIMIT),
            user_limit: row
                .try_get::<_, i32>("user_limit")
                .map(i64::from)
                .or_else(|_| row.try_get::<_, i64>("user_limit"))
                .unwrap_or(DEFAULT_USER_LIMIT),
            limit_bytes,
            // Tolerate an unmigrated schema and unknown values: an old
            // database degrades to legacy access instead of failing to load.
            product: row
                .try_get::<_, String>("product")
                .ok()
                .and_then(|s| Product::from_stored(&s))
                .unwrap_or_default(),
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

    /// Tariff product of the subscription (`legacy` when it was created
    /// before products existed). Orthogonal to `plan_kind`.
    fn product(&self) -> Product;
    fn set_product(&mut self, product: Product);

    fn parent_id(&self) -> Option<uuid::Uuid>;
    fn set_parent_id(&mut self, parent_id: uuid::Uuid);

    /// Whether this subscription is a "user" (child) of a selfhosted
    /// account: it belongs to a parent and is itself selfhosted-flavoured,
    /// i.e. it lives only on the parent's nodes.
    fn is_selfhosted_user(&self) -> bool {
        self.parent_id().is_some() && self.plan_kind() == PlanKind::Selfhosted
    }

    fn scope_env(&self) -> Option<&Env>;
    fn set_scope_env(&mut self, env: Env);

    fn premium_token(&self) -> Option<&str>;
    fn set_premium_token(&mut self, token: String);

    /// Premium term (`subscriptions.premium_until`): when the slot stops being
    /// paid for. Independent of `expires_at`.
    fn premium_until(&self) -> Option<DateTime<Utc>>;
    fn set_premium_until(&mut self, until: Option<DateTime<Utc>>);

    /// Selfhosted caps: max own nodes (servers, offline included) and max
    /// child subscriptions (users) this account may hold.
    fn server_limit(&self) -> i64;
    fn user_limit(&self) -> i64;
    fn selfhosted_limits(&self) -> SelfhostedLimits;
    fn set_selfhosted_limits(&mut self, server_limit: i64, user_limit: i64);

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
            PlanKind::Selfhosted => false,
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
        !self.is_deleted
            && self
                .expires_at
                .map(|expires_at| expires_at > Utc::now())
                .unwrap_or(true)
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

    fn product(&self) -> Product {
        self.product
    }

    fn set_product(&mut self, product: Product) {
        self.product = product;
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

    fn premium_until(&self) -> Option<DateTime<Utc>> {
        self.premium_until
    }

    fn set_premium_until(&mut self, until: Option<DateTime<Utc>>) {
        self.premium_until = until;
        self.updated_at = Utc::now();
    }

    fn server_limit(&self) -> i64 {
        self.server_limit
    }

    fn user_limit(&self) -> i64 {
        self.user_limit
    }

    fn selfhosted_limits(&self) -> SelfhostedLimits {
        SelfhostedLimits {
            server_limit: self.server_limit,
            user_limit: self.user_limit,
        }
    }

    fn set_selfhosted_limits(&mut self, server_limit: i64, user_limit: i64) {
        self.server_limit = server_limit;
        self.user_limit = user_limit;
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
            (PlanKind::Selfhosted, "selfhosted"),
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

    #[test]
    fn test_new_selfhosted_subscription() {
        let env = Env::personal_for(uuid::Uuid::new_v4());
        let sub =
            Subscription::new_selfhosted(uuid::Uuid::new_v4(), "ref".to_string(), env.clone());

        assert_eq!(sub.plan_kind(), PlanKind::Selfhosted);
        assert_eq!(sub.expires_at(), None);
        assert_eq!(sub.limit_bytes(), None);
        assert_eq!(sub.scope_env(), Some(&env));
        assert!(env.is_personal());
        // Active despite the NULL expiry; no time expiry; no top-up.
        assert!(sub.is_active());
        assert!(!sub.time_expired());
        assert!(!sub.top_up_allowed(true));
        assert!(sub.is_active_with_traffic(true, 0));
    }

    #[test]
    fn test_selfhosted_limits_defaults_and_setter() {
        let env = Env::personal_for(uuid::Uuid::new_v4());
        let mut sub = Subscription::new_selfhosted(uuid::Uuid::new_v4(), "ref".to_string(), env);

        // Fresh accounts start at the defaults.
        assert_eq!(sub.server_limit(), DEFAULT_SERVER_LIMIT);
        assert_eq!(sub.user_limit(), DEFAULT_USER_LIMIT);
        assert_eq!(sub.selfhosted_limits(), SelfhostedLimits::default());

        sub.set_selfhosted_limits(7, 100);
        assert_eq!(sub.server_limit(), 7);
        assert_eq!(sub.user_limit(), 100);
        assert_eq!(
            sub.selfhosted_limits(),
            SelfhostedLimits {
                server_limit: 7,
                user_limit: 100,
            }
        );

        // Non-selfhosted plans carry the same defaults and ignore them.
        let standard = Subscription::new(uuid::Uuid::new_v4(), "ref".to_string(), None, None);
        assert_eq!(standard.server_limit(), DEFAULT_SERVER_LIMIT);
        assert_eq!(standard.user_limit(), DEFAULT_USER_LIMIT);
    }

    #[test]
    fn test_selfhosted_limits_serde_roundtrip_and_legacy_payload() {
        let env = Env::personal_for(uuid::Uuid::new_v4());
        let mut sub =
            Subscription::new_selfhosted(uuid::Uuid::new_v4(), "ref".to_string(), env.clone());
        sub.set_selfhosted_limits(5, 42);

        let json = serde_json::to_string(&sub).unwrap();
        let back: Subscription = serde_json::from_str(&json).unwrap();
        assert_eq!(back.server_limit(), 5);
        assert_eq!(back.user_limit(), 42);

        // A payload written before the columns existed must fall back to the
        // defaults instead of failing to deserialize (or defaulting to 0).
        let mut legacy = serde_json::to_value(&sub).unwrap();
        let obj = legacy.as_object_mut().unwrap();
        obj.remove("server_limit");
        obj.remove("user_limit");
        let back: Subscription = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.server_limit(), DEFAULT_SERVER_LIMIT);
        assert_eq!(back.user_limit(), DEFAULT_USER_LIMIT);
    }

    fn sub_with(expires_at: Option<DateTime<Utc>>, limit_bytes: Option<i64>) -> Subscription {
        Subscription::new(
            uuid::Uuid::new_v4(),
            "ref".to_string(),
            expires_at,
            limit_bytes,
        )
    }

    #[test]
    fn test_user_child_is_selfhosted_child_without_limits() {
        let owner = uuid::Uuid::new_v4();
        let child = Subscription::new_user_child(owner);

        // A user is a child of the account and lives on the owner's nodes.
        assert_eq!(child.parent_id(), Some(owner));
        assert!(child.is_selfhosted_user());
        assert_eq!(child.plan_kind(), PlanKind::Selfhosted);
        // No own personal env, no expiry of its own, no own servers/users.
        assert!(child.scope_env().is_none());
        assert!(child.expires_at().is_none());
        assert_eq!(child.server_limit(), 0);
        assert_eq!(child.user_limit(), 0);
        assert!(child.limit_bytes().is_none());
        // NULL expiry means active: the user lives while the owner keeps it.
        assert!(child.is_active());

        // The owner itself is not a user; a premium child is not one either.
        let owner_sub = Subscription::new(
            owner,
            "ref".to_string(),
            Some(Utc::now() + chrono::Duration::days(30)),
            None,
        );
        assert!(!owner_sub.is_selfhosted_user());
        let mut premium_child = owner_sub.clone();
        premium_child.set_parent_id(owner);
        assert!(!premium_child.is_selfhosted_user());
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
        let active_with_limit = sub_with(Some(Utc::now() + chrono::Duration::days(1)), Some(1000));
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
