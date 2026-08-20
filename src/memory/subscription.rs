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
}
