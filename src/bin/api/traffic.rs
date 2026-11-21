use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use fcore::{Env, MetricStorage};
use serde::Serialize;
use std::collections::HashMap;

/// Start of the UTC calendar day containing `t`.
pub fn day_start(t: DateTime<Utc>) -> DateTime<Utc> {
    let naive = t.date_naive().and_hms_opt(0, 0, 0).unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(t.year(), t.month(), 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    });
    Utc.from_utc_datetime(&naive)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .unwrap();
    next_month_first.pred_opt().unwrap().day()
}

fn build_anchor(year: i32, month: u32, day: u32, time: NaiveTime) -> DateTime<Utc> {
    let last_day = days_in_month(year, month);
    let d = day.min(last_day);
    let naive = NaiveDate::from_ymd_opt(year, month, d)
        .unwrap()
        .and_time(time);
    Utc.from_utc_datetime(&naive)
}

/// Most recent monthly boundary for a subscription created at `created_at`.
/// The boundary repeats the day-of-month (and time) of `created_at`, clamped
/// to the last day of shorter months.
pub fn monthly_anchor(created_at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let time = created_at.time();
    let day = created_at.day();

    let mut anchor = build_anchor(now.year(), now.month(), day, time);
    if anchor > now {
        let (prev_year, prev_month) = if now.month() == 1 {
            (now.year() - 1, 12)
        } else {
            (now.year(), now.month() - 1)
        };
        anchor = build_anchor(prev_year, prev_month, day, time);
    }
    anchor
}

/// Previous monthly boundary before `anchor`.
pub fn prev_monthly_anchor(created_at: DateTime<Utc>, anchor: DateTime<Utc>) -> DateTime<Utc> {
    let time = created_at.time();
    let day = created_at.day();

    let (prev_year, prev_month) = if anchor.month() == 1 {
        (anchor.year() - 1, 12)
    } else {
        (anchor.year(), anchor.month() - 1)
    };
    build_anchor(prev_year, prev_month, day, time)
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct TrafficValue {
    pub uplink: u64,
    pub downlink: u64,
}

impl std::ops::AddAssign<TrafficValue> for TrafficValue {
    fn add_assign(&mut self, rhs: TrafficValue) {
        self.uplink += rhs.uplink;
        self.downlink += rhs.downlink;
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct EnvTraffic {
    pub total: TrafficValue,
    pub daily: TrafficValue,
    pub monthly: TrafficValue,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SubscriptionTraffic {
    pub total: TrafficValue,
    pub daily: TrafficValue,
    pub monthly: TrafficValue,
    pub by_env: HashMap<String, EnvTraffic>,
}

impl SubscriptionTraffic {
    pub fn add_persisted(
        &mut self,
        env: &str,
        total: TrafficValue,
        daily: TrafficValue,
        monthly: TrafficValue,
    ) {
        self.total += total;
        self.daily += daily;
        self.monthly += monthly;
        let entry = self.by_env.entry(env.to_string()).or_default();
        entry.total += total;
        entry.daily += daily;
        entry.monthly += monthly;
    }

    pub fn add_live_segment(&mut self, env: &str, segment: &SegmentTraffic) {
        let value = TrafficValue {
            uplink: segment.uplink,
            downlink: segment.downlink,
        };
        self.daily += value;
        self.monthly += value;
        let entry = self.by_env.entry(env.to_string()).or_default();
        entry.daily += value;
        entry.monthly += value;
    }
}

/// A slice of traffic that belongs to exactly one daily and one monthly bucket.
#[derive(Debug, Clone, Copy)]
pub struct SegmentTraffic {
    pub day_bucket: DateTime<Utc>,
    pub month_bucket: DateTime<Utc>,
    pub uplink: u64,
    pub downlink: u64,
}

/// Split traffic for a single connection between `from` and `to` into segments
/// at day/month boundaries and return each segment with its correct buckets.
pub fn connection_deltas_between(
    metrics: &MetricStorage,
    conn_id: &uuid::Uuid,
    created_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<SegmentTraffic> {
    segments_between(created_at, from, to, |start, end| {
        metrics.get_connection_delta_traffic(conn_id, start, end)
    })
}

/// Same split as [`connection_deltas_between`], for a peer or any other series
/// keyed by its own id: `delta` returns the (uplink, downlink) of one window.
pub fn segments_between<F>(
    created_at: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    mut delta: F,
) -> Vec<SegmentTraffic>
where
    F: FnMut(i64, i64) -> (u64, u64),
{
    if from >= to {
        return Vec::new();
    }

    let mut boundaries: Vec<DateTime<Utc>> = Vec::new();

    // Daily (midnight) boundaries in (from, to].
    let mut d = day_start(to);
    while d > from && d <= to {
        boundaries.push(d);
        d -= Duration::days(1);
    }

    // Monthly (subscription-anchor) boundaries in (from, to].
    let mut m = monthly_anchor(created_at, to);
    while m > from && m <= to {
        boundaries.push(m);
        m = prev_monthly_anchor(created_at, m);
    }

    boundaries.sort();
    boundaries.dedup();

    let mut points = Vec::with_capacity(boundaries.len() + 2);
    points.push(from);
    points.extend(boundaries);
    points.push(to);
    points.sort();
    points.dedup();

    let mut result = Vec::with_capacity(points.len().saturating_sub(1));
    for window in points.windows(2) {
        let seg_start = window[0].timestamp_millis();
        let seg_end = window[1].timestamp_millis();
        let (uplink, downlink) = delta(seg_start, seg_end);
        if uplink == 0 && downlink == 0 {
            continue;
        }
        result.push(SegmentTraffic {
            day_bucket: day_start(window[1]),
            month_bucket: monthly_anchor(created_at, window[1]),
            uplink,
            downlink,
        });
    }

    result
}

/// Booking window of one peer for one persister tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Window the peer persister has to book now.
///
/// `last_persist_at` is the value stored in `peer_watermarks`; `None` means the
/// peer was never booked and the window starts at its creation. It is never
/// derived from `now` or `now - interval`: that in-memory origin reset on every
/// restart and re-counted up to one interval of already persisted traffic.
/// `None` is returned when the stored point is already at or past `now`, so a
/// window that was booked can never be booked again.
pub fn peer_window(
    created_at: DateTime<Utc>,
    last_persist_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<PeerWindow> {
    let from = last_persist_at.unwrap_or(created_at).max(created_at);
    if from >= now {
        return None;
    }
    Some(PeerWindow { from, to: now })
}

/// One row written to `peer_traffic` / `connection_traffic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookingRow {
    pub period: &'static str,
    pub bucket: DateTime<Utc>,
    pub uplink: u64,
    pub downlink: u64,
}

/// Rows a booking writes for a window: one `day` and one `month` row per
/// segment, exactly like the v1 connection path. Both periods are needed —
/// `/sub/` and the traffic screen read `month`, the card's lifetime total
/// reads `day`.
pub fn booking_rows(segments: &[SegmentTraffic]) -> Vec<BookingRow> {
    let mut rows = Vec::with_capacity(segments.len() * 2);
    for seg in segments {
        rows.push(BookingRow {
            period: "day",
            bucket: seg.day_bucket,
            uplink: seg.uplink,
            downlink: seg.downlink,
        });
        rows.push(BookingRow {
            period: "month",
            bucket: seg.month_bucket,
            uplink: seg.uplink,
            downlink: seg.downlink,
        });
    }
    rows
}

/// Parse an env string. There is no fallback on purpose: booking traffic to
/// `experimental` because of a typo silently corrupts another region's quota,
/// so an unparsable env is dropped at the call site.
pub fn parse_env(env: &str) -> Option<Env> {
    match env.parse::<Env>() {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!("traffic: unparsable env {env:?} dropped: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_day_start() {
        let t = Utc.with_ymd_and_hms(2026, 6, 7, 14, 30, 0).unwrap();
        let start = day_start(t);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 6, 7, 0, 0, 0).unwrap());
    }

    #[test]
    fn test_monthly_anchor_same_month() {
        let created = Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 10, 0, 0).unwrap();
        assert_eq!(
            monthly_anchor(created, now),
            Utc.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn test_monthly_anchor_previous_month() {
        let created = Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 10, 10, 0, 0).unwrap();
        assert_eq!(
            monthly_anchor(created, now),
            Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn test_monthly_anchor_clamps_to_shorter_month() {
        let created = Utc.with_ymd_and_hms(2026, 1, 31, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 10, 0, 0).unwrap();
        assert_eq!(
            monthly_anchor(created, now),
            Utc.with_ymd_and_hms(2026, 1, 31, 12, 0, 0).unwrap()
        );

        let now = Utc.with_ymd_and_hms(2026, 3, 15, 10, 0, 0).unwrap();
        assert_eq!(
            monthly_anchor(created, now),
            Utc.with_ymd_and_hms(2026, 2, 28, 12, 0, 0).unwrap()
        );
    }
}
