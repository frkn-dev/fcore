use chrono::{DateTime, Utc};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use std::ops::Deref;
use std::ops::DerefMut;

use crate::utils::get_uuid_last_octet_simple;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub id: uuid::Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub referred_by: Option<String>,
    pub refer_code: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_deleted: bool,
    pub referral_bonus_awarded: bool,

    pub limit_bytes: Option<i64>,
}

impl Subscription {
    pub fn new(
        id: uuid::Uuid,
        ref_by: Option<String>,
        ref_code: String,
        exp_at: Option<DateTime<Utc>>,
        limit_bytes: Option<i64>,
    ) -> Subscription {
        let now = Utc::now();
        Self {
            id,
            expires_at: exp_at,
            referred_by: ref_by,
            refer_code: ref_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            referral_bonus_awarded: false,

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
            referred_by: None,
            refer_code,
            created_at: now,
            updated_at: now,
            is_deleted: false,
            referral_bonus_awarded: false,
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
            referred_by: row.get("referred_by"),
            refer_code: row.get("refer_code"),
            created_at,
            updated_at,
            is_deleted: row.get::<_, bool>("is_deleted"),
            referral_bonus_awarded: row
                .try_get::<_, bool>("referral_bonus_awarded")
                .unwrap_or(false),
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
    fn referred_by(&self) -> Option<&str>;
    fn set_referred_by(&mut self, code: String);
    fn is_active(&self) -> bool;
    fn days_remaining(&self) -> Option<i64>;
    fn set_expires_at(&mut self, expires_at: DateTime<Utc>) -> Result<(), String>;
    fn mark_deleted(&mut self);
    fn stats(&self) -> SubscriptionStats;

    fn limit_bytes(&self) -> Option<i64>;
    fn set_limit_bytes(&mut self, bytes: i64);

    fn referral_bonus_awarded(&self) -> bool;
    fn set_referral_bonus_awarded(&mut self, awarded: bool);
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
        if let Some(expires_at) = self.expires_at {
            let new_exp = expires_at + chrono::Duration::days(days);
            self.expires_at = Some(new_exp);
        };
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

    fn referred_by(&self) -> Option<&str> {
        self.referred_by.as_deref().map(str::trim)
    }

    fn set_referred_by(&mut self, code: String) {
        self.referred_by = Some(code);
    }

    fn is_active(&self) -> bool {
        !self.is_deleted && self.expires_at > Some(Utc::now())
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

    fn referral_bonus_awarded(&self) -> bool {
        self.referral_bonus_awarded
    }

    fn set_referral_bonus_awarded(&mut self, awarded: bool) {
        self.referral_bonus_awarded = awarded;
        self.updated_at = Utc::now();
    }
}
