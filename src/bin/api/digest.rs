//! Сводка по системе: данные для писем daily / weekly / monthly.
//!
//! Считается целиком из Postgres api — отчёту не нужны внешние источники,
//! поэтому он переживает перезапуски и не зависит от того, что сейчас в
//! памяти. Отправку и расписание держит mrkting: он забирает готовый JSON
//! через `GET /v1/admin/digest` и сам собирает письмо.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use tokio_postgres::Client as PgClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestKind {
    Daily,
    Weekly,
    Monthly,
}

impl DigestKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "daily" | "day" => Some(Self::Daily),
            "weekly" | "week" => Some(Self::Weekly),
            "monthly" | "month" => Some(Self::Monthly),
            _ => None,
        }
    }

    /// Окно отчёта: daily — прошлые сутки, weekly — 7 дней, monthly — прошлый
    /// календарный месяц.
    pub fn window(&self, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        match self {
            Self::Daily => {
                let to = start_of_day(now);
                (to - Duration::days(1), to)
            }
            Self::Weekly => {
                let to = start_of_day(now);
                (to - Duration::days(7), to)
            }
            Self::Monthly => {
                let to = start_of_month(now);
                (start_of_month(to - Duration::days(1)), to)
            }
        }
    }

    /// Заголовок отчёта.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Daily => "Сводка за сутки",
            Self::Weekly => "Сводка за неделю",
            Self::Monthly => "Сводка за месяц",
        }
    }
}

fn start_of_day(at: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(&at.date_naive().and_hms_opt(0, 0, 0).unwrap())
}

fn start_of_month(at: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(at.year(), at.month(), 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    )
}

/// Всё, что попало в отчёт.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Digest {
    pub kind: Option<DigestKind>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub nodes_total: i64,
    pub nodes_online: i64,
    pub by_env: Vec<(String, i64, i64)>,
    pub alerts_total: i64,
    pub alerts_by_rule: Vec<(String, i64)>,
    pub alert_hosts: Vec<(String, i64)>,
    pub downtime_minutes: i64,
    pub connects: i64,
    pub connecting_subs: i64,
    pub by_protocol: Vec<(String, i64)>,
    pub by_country: Vec<(String, i64)>,
    pub active_now: i64,
    pub active_at_start: i64,
    pub created: i64,
    pub expired: i64,
    pub expiring_7d: i64,
    pub by_product: Vec<(String, i64)>,
    /// (подпись, активных на конец периода)
    pub trend: Vec<(String, i64)>,
}

impl Digest {
    /// Число людей = активные подписки: одна подписка ≈ один клиент,
    /// устройства считаются отдельно (connections).
    async fn active_at(client: &PgClient, at: DateTime<Utc>) -> Result<i64, tokio_postgres::Error> {
        let row = client
            .query_one(
                r#"SELECT count(*) FROM subscriptions
                   WHERE is_deleted IS NOT TRUE
                     AND created_at < $1
                     AND (expires_at IS NULL OR expires_at > $1)"#,
                &[&at],
            )
            .await?;
        row.try_get::<_, i64>(0)
    }

    pub async fn collect(
        client: &PgClient,
        kind: DigestKind,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Self, tokio_postgres::Error> {
        let mut d = Digest {
            kind: Some(kind),
            from: Some(from),
            to: Some(to),
            ..Default::default()
        };

        // --- флот -------------------------------------------------------
        // bigint column: bind an i64, or tokio_postgres refuses to serialise f64
        let heartbeat_alive_ms: i64 = Utc::now().timestamp_millis() - 240_000;
        let row = client
            .query_one(
                r#"select count(*),
                          count(*) filter (where hb.last_ms > $1)
                   from nodes n left join node_heartbeats hb on hb.node_id = n.id"#,
                &[&heartbeat_alive_ms],
            )
            .await?;
        d.nodes_total = row.try_get(0)?;
        d.nodes_online = row.try_get(1)?;

        d.by_env = client
            .query(
                r#"select case
                            when n.env ilike 'custompersonal%' or n.env ilike 'personal%' then 'личные'
                            when n.env ilike 'custompremium%' or n.env ilike 'premium%' then 'премиум'
                            else n.env
                          end as grp,
                          count(*),
                          count(*) filter (where hb.last_ms > $1)
                   from nodes n left join node_heartbeats hb on hb.node_id = n.id
                   group by 1 order by 2 desc, 1"#,
                &[&heartbeat_alive_ms],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;

        // --- алерты -----------------------------------------------------
        d.alerts_by_rule = client
            .query(
                r#"select rule, count(*) from alert_events
                   where created_at >= $1 and created_at < $2
                   group by 1 order by 2 desc"#,
                &[&from, &to],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        d.alerts_total = d.alerts_by_rule.iter().map(|(_, c)| *c).sum();

        d.alert_hosts = client
            .query(
                r#"select hostname, count(*) from alert_events
                   where created_at >= $1 and created_at < $2
                   group by 1 order by 2 desc limit 5"#,
                &[&from, &to],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;

        // Простой считаем по худшему эпизоду на ноду и не длиннее окна:
        // иначе одна и та же авария складывается из node_offline и
        // node_offline_long и «простой» получается больше календаря.
        let window_secs = (to - from).num_seconds().max(1);
        let downtime: i64 = client
            .query_one(
                r#"select coalesce(sum(least(biggest, $3)), 0)::bigint from (
                       select max(duration_sec) as biggest
                       from alert_events
                       where created_at >= $1 and created_at < $2
                         and rule = 'node_offline'
                       group by hostname
                   ) t"#,
                &[&from, &to, &window_secs],
            )
            .await?
            .try_get::<_, i64>(0)?;
        d.downtime_minutes = downtime / 60;

        // --- клиенты ----------------------------------------------------
        let row = client
            .query_one(
                r#"select count(*), count(distinct subscription_id) from connect_events
                   where at >= $1 and at < $2"#,
                &[&from, &to],
            )
            .await?;
        d.connects = row.try_get(0)?;
        d.connecting_subs = row.try_get(1)?;

        d.by_protocol = client
            .query(
                r#"select coalesce(nullif(protocol, ''), '—'), count(*) from connect_events
                   where at >= $1 and at < $2 group by 1 order by 2 desc limit 6"#,
                &[&from, &to],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;

        d.by_country = client
            .query(
                r#"select country, count(*) from connect_events
                   where at >= $1 and at < $2 and country is not null and country <> ''
                   group by 1 order by 2 desc limit 5"#,
                &[&from, &to],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;

        // --- бизнес: люди ----------------------------------------------
        d.active_now = Self::active_at(client, to).await?;
        d.active_at_start = Self::active_at(client, from).await?;

        d.created = client
            .query_one(
                r#"select count(*) from subscriptions
                   where is_deleted is not true and created_at >= $1 and created_at < $2"#,
                &[&from, &to],
            )
            .await?
            .try_get::<_, i64>(0)?;

        d.expired = client
            .query_one(
                r#"select count(*) from subscriptions
                   where is_deleted is not true and expires_at >= $1 and expires_at < $2"#,
                &[&from, &to],
            )
            .await?
            .get(0);

        d.expiring_7d = client
            .query_one(
                r#"select count(*) from subscriptions
                   where is_deleted is not true and expires_at > now()
                     and expires_at < now() + interval '7 days'"#,
                &[],
            )
            .await?
            .get(0);

        d.by_product = client
            .query(
                r#"select coalesce(nullif(product, ''), 'standard'), count(*) from subscriptions
                   where is_deleted is not true and expires_at > $1 group by 1 order by 2 desc"#,
                &[&to],
            )
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;

        // --- тренд ------------------------------------------------------
        // Точки — концы периодов: 7 дней / 8 недель / 12 месяцев.
        let (points, step_days, label): (usize, i64, &str) = match kind {
            DigestKind::Daily => (7, 1, "день"),
            DigestKind::Weekly => (8, 7, "неделя"),
            DigestKind::Monthly => (12, 30, "месяц"),
        };
        for i in (0..points).rev() {
            let end = to - Duration::days(step_days * i as i64);
            let label = match kind {
                DigestKind::Daily => format!("{}", end.format("%d.%m")),
                DigestKind::Weekly => format!("нед. {}", end.format("%d.%m")),
                DigestKind::Monthly => {
                    // конец месяца = начало следующего; подпись — сам месяц
                    format!("{}", (end - Duration::days(1)).format("%m.%Y"))
                }
            };
            let value = Self::active_at(client, end).await?;
            d.trend.push((label, value));
        }

        Ok(d)
    }

    fn window_label(&self) -> String {
        match (self.from, self.to) {
            (Some(f), Some(t)) => match self.kind {
                // за сутки — одна дата, иначе «10.11 — 10.11» в теме и в шапке
                Some(DigestKind::Daily) => f.format("%d.%m").to_string(),
                Some(DigestKind::Monthly) => (t - Duration::days(1)).format("%m.%Y").to_string(),
                _ => format!("{} — {}", f.format("%d.%m"), (t - Duration::days(1)).format("%d.%m")),
            },
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn daily_window_is_yesterday() {
        let now = utc("2026-11-12T06:00:00Z");
        let (from, to) = DigestKind::Daily.window(now);
        assert_eq!(from, utc("2026-11-11T00:00:00Z"));
        assert_eq!(to, utc("2026-11-12T00:00:00Z"));
    }

    #[test]
    fn weekly_window_is_seven_days() {
        let now = utc("2026-11-12T06:00:00Z");
        let (from, to) = DigestKind::Weekly.window(now);
        assert_eq!((to - from).num_days(), 7);
        assert_eq!(to, utc("2026-11-12T00:00:00Z"));
    }

    #[test]
    fn monthly_window_is_the_previous_calendar_month() {
        let now = utc("2026-11-01T06:00:00Z");
        let (from, to) = DigestKind::Monthly.window(now);
        assert_eq!(from, utc("2026-10-01T00:00:00Z"));
        assert_eq!(to, utc("2026-11-01T00:00:00Z"));
    }

    #[test]
    fn window_label_is_short() {
        let d = Digest {
            kind: Some(DigestKind::Daily),
            from: Some(utc("2026-11-10T00:00:00Z")),
            to: Some(utc("2026-11-11T00:00:00Z")),
            ..Default::default()
        };
        assert_eq!(d.window_label(), "10.11");
    }

    #[test]
    fn digest_serializes_tuples_as_pairs() {
        let d = Digest {
            kind: Some(DigestKind::Daily),
            by_env: vec![("dev".to_string(), 3, 2)],
            ..Default::default()
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["kind"], "daily");
        assert_eq!(json["by_env"][0][0], "dev");
        assert_eq!(json["by_env"][0][1], 3);
    }
}
