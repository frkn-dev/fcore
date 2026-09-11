pub mod admin;
pub mod amnezia;
pub mod cluster;
pub mod connection;
pub mod iap;
pub mod key;
pub mod metrics;
pub mod node;
pub mod premium;
pub mod subscription;

use warp::http::StatusCode;

use fcore::{
    http::ResponseMessage, Connection, ConnectionApiOperations, ConnectionBaseOperations,
    NodeStorageOperations, SubscriptionOperations,
};

use crate::sync::MemSync;

// GET /healthcheck
pub async fn healthcheck_handler<N, C, S>(
    _state: MemSync<N, C, S>,
) -> Result<impl warp::Reply, warp::Rejection>
where
    N: NodeStorageOperations + Sync + Send + Clone + 'static,
    C: ConnectionApiOperations
        + ConnectionBaseOperations
        + Sync
        + Send
        + Clone
        + 'static
        + From<Connection>
        + PartialEq,
    S: SubscriptionOperations + Send + Sync + Clone + 'static,
{
    let response = ResponseMessage::<Option<uuid::Uuid>> {
        status: 200,
        message: "Ok".to_string(),
        response: None,
    };

    Ok(warp::reply::with_status(
        warp::reply::json(&response),
        StatusCode::OK,
    ))
}

/// Serving-side activity classification for client-facing config endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServingAccess {
    /// Active the legacy way (paid time left, or lite with expires_at NULL):
    /// serve all protocols.
    Full,
    /// Traffic mode: the paid time is over and the subscription lives on its
    /// traffic balance — serve metered protocols only.
    TrafficMode,
    /// Expired/revoked: legacy "subscription expired" response.
    Expired,
    /// Time-expired subscription with a traffic limit: resolve against the DB
    /// via [`resolve_serving_access`] (carries limit_bytes).
    CheckBalance(i64),
}

/// Classify a subscription for client-facing config serving. Pure and cheap:
/// with the traffic-mode flag off the result is the legacy is_active split
/// and no DB access happens downstream, so the hot path is unchanged.
pub(crate) fn serving_access<S>(sub: &S, traffic_mode_enabled: bool) -> ServingAccess
where
    S: SubscriptionOperations,
{
    if sub.is_active() {
        return ServingAccess::Full;
    }
    if !traffic_mode_enabled || sub.is_deleted() || !sub.time_expired() {
        return ServingAccess::Expired;
    }
    match sub.limit_bytes() {
        Some(limit) => ServingAccess::CheckBalance(limit),
        None => ServingAccess::Expired,
    }
}

/// Resolves a possibly-pending `CheckBalance` to a terminal state by loading
/// the lifetime traffic from the DB: balance left -> TrafficMode, exhausted
/// -> Expired. Fail-open on a DB error (warn + TrafficMode): a traffic-DB
/// hiccup must not take down config serving for everyone whose paid time has
/// expired — the enforce worker still stops actually-exhausted subscriptions.
pub(crate) async fn resolve_serving_access(
    db: &crate::postgres::pg::PgContext,
    sub_id: uuid::Uuid,
    access: ServingAccess,
) -> ServingAccess {
    let limit_bytes = match access {
        ServingAccess::CheckBalance(limit) => limit,
        terminal => return terminal,
    };

    match db.traffic().total_for_subscription(sub_id).await {
        Ok((uplink, downlink)) => {
            if limit_bytes - (uplink + downlink) > 0 {
                ServingAccess::TrafficMode
            } else {
                ServingAccess::Expired
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to load traffic total for subscription {}: {}",
                sub_id,
                e
            );
            ServingAccess::TrafficMode
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(expires_in_days: i64, limit_bytes: Option<i64>) -> fcore::Subscription {
        fcore::Subscription::new(
            uuid::Uuid::new_v4(),
            "ref".to_string(),
            Some(chrono::Utc::now() + chrono::Duration::days(expires_in_days)),
            limit_bytes,
        )
    }

    #[test]
    fn test_serving_access_flag_off_is_legacy() {
        // Active: full access; expired: expired — never a DB check.
        assert_eq!(serving_access(&sub(1, None), false), ServingAccess::Full);
        assert_eq!(serving_access(&sub(1, Some(100)), false), ServingAccess::Full);
        assert_eq!(serving_access(&sub(-1, None), false), ServingAccess::Expired);
        assert_eq!(serving_access(&sub(-1, Some(100)), false), ServingAccess::Expired);
    }

    #[test]
    fn test_serving_access_flag_on() {
        // Active paid time: full access regardless of the limit.
        assert_eq!(serving_access(&sub(1, Some(100)), true), ServingAccess::Full);
        // Expired with a limit: needs the DB balance check.
        assert_eq!(
            serving_access(&sub(-1, Some(100)), true),
            ServingAccess::CheckBalance(100)
        );
        // Expired without a limit: nothing to live on.
        assert_eq!(serving_access(&sub(-1, None), true), ServingAccess::Expired);
        // Deleted stays expired even with a limit.
        let mut deleted = sub(-1, Some(100));
        deleted.mark_deleted();
        assert_eq!(serving_access(&deleted, true), ServingAccess::Expired);
    }
}
