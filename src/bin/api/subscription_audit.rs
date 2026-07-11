use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Parses an optional trace id from a raw header value. Falls back to a fresh
/// UUID if the value is missing or invalid.
pub fn trace_id_from_header(raw: Option<String>) -> Uuid {
    raw.and_then(|s| Uuid::parse_str(&s).ok())
        .unwrap_or_else(Uuid::new_v4)
}

/// Creates a tracing span that carries a trace_id for a subscription-days
/// transaction. If `trace_id` is `None` a fresh UUID is generated.
/// All `subscription.audit` events emitted inside this span will include the
/// trace_id, the API handler name and the subscription id.
pub fn transaction_span(handler: &str, sub_id: Uuid, trace_id: Option<Uuid>) -> tracing::Span {
    let trace_id = trace_id.unwrap_or_else(Uuid::new_v4);
    tracing::info_span!(
        "subscription_transaction",
        trace_id = %trace_id,
        handler = handler,
        sub_id = %sub_id,
    )
}

/// Computes how many full days remain until the given expiration date.
fn days_remaining(expires_at: Option<DateTime<Utc>>) -> i64 {
    let now = Utc::now();
    expires_at
        .map(|exp| (exp - now).num_days())
        .unwrap_or(0)
}

/// Logs the start of a subscription-days transaction.
/// Should be called right before the instrumented async operation.
pub fn log_transaction_start(sub_id: Uuid, requested_days: Option<i64>) {
    let direction = match requested_days {
        Some(d) if d > 0 => "add",
        Some(d) if d < 0 => "subtract",
        Some(0) => "noop",
        _ => "unknown",
    };

    tracing::info!(
        target: "subscription.audit",
        event = "transaction_start",
        sub_id = %sub_id,
        requested_days = requested_days,
        direction = direction,
        "subscription days transaction started",
    );
}

/// Logs a change of the subscription expiration balance.
pub fn log_days_change(
    event: &str,
    sub_id: Uuid,
    old_expires_at: Option<DateTime<Utc>>,
    new_expires_at: Option<DateTime<Utc>>,
    delta_days: Option<i64>,
    initiator: &str,
) {
    let old = old_expires_at.map(|d| d.to_rfc3339());
    let new = new_expires_at.map(|d| d.to_rfc3339());
    let old_days = days_remaining(old_expires_at);
    let new_days = days_remaining(new_expires_at);

    let direction = match delta_days {
        Some(d) if d > 0 => "add",
        Some(d) if d < 0 => "subtract",
        Some(0) => "noop",
        _ => "unknown",
    };

    tracing::info!(
        target: "subscription.audit",
        event = event,
        sub_id = %sub_id,
        old_expires_at = old,
        new_expires_at = new,
        old_days_remaining = old_days,
        new_days_remaining = new_days,
        delta_days = delta_days,
        direction = direction,
        initiator = initiator,
        "subscription days balance changed",
    );
}

/// Logs the moment a subscription expires and its connections are deactivated.
pub fn log_subscription_expired(
    sub_id: Uuid,
    expires_at: Option<DateTime<Utc>>,
    connections_deleted: usize,
) {
    tracing::info!(
        target: "subscription.audit",
        event = "subscription_expired",
        sub_id = %sub_id,
        expires_at = expires_at.map(|d| d.to_rfc3339()),
        days_remaining = days_remaining(expires_at),
        connections_deleted = connections_deleted,
        "subscription expired, connections deactivated",
    );
}

/// Logs the moment a standalone connection expires and is deleted.
pub fn log_connection_expired(
    conn_id: Uuid,
    expires_at: Option<DateTime<Utc>>,
    subscription_id: Option<Uuid>,
) {
    tracing::info!(
        target: "subscription.audit",
        event = "connection_expired",
        conn_id = %conn_id,
        subscription_id = subscription_id.map(|id| id.to_string()),
        expires_at = expires_at.map(|d| d.to_rfc3339()),
        days_remaining = days_remaining(expires_at),
        "standalone connection expired",
    );
}
