//! Сводка по системе: daily / weekly / monthly отчёт в почту.
//!
//! Считается целиком из Postgres api — никаких внешних источников, поэтому
//! отчёт переживает перезапуски и не зависит от того, что сейчас в памяти.
//! Доставка — тем же SMTP, что у алертов (`[alerts.email]`).

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use tokio_postgres::Client as PgClient;

use crate::alerts::{send_email_html, AlertEmailConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// `[digest]` в конфиге api.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct DigestConfig {
    /// Выключено по умолчанию: сводка — это рассылка, её включают осознанно.
    #[serde(default)]
    pub enabled: bool,
    /// Локальный час отправки (0..23).
    #[serde(default = "default_digest_hour")]
    pub hour: u32,
    /// Смещение локального времени от UTC (МСК = 3).
    #[serde(default = "default_digest_offset")]
    pub utc_offset_hours: i32,
    /// Присылать недельную сводку по понедельникам.
    #[serde(default = "default_true")]
    pub weekly: bool,
    /// Присылать месячную сводку 1-го числа.
    #[serde(default = "default_true")]
    pub monthly: bool,
    /// Откуда брать покупки: payment-gw `/analytics`. Без этой секции блок
    /// покупок в письме просто не выводится (деньги живут в другой базе).
    #[serde(default)]
    pub payment: Option<PaymentSource>,
}

/// Источник данных по покупкам: payment-gw и его analytics-токен.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct PaymentSource {
    pub endpoint: String,
    pub token: String,
}

fn default_digest_hour() -> u32 {
    9
}
fn default_digest_offset() -> i32 {
    3
}
fn default_true() -> bool {
    true
}

impl Default for DigestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hour: default_digest_hour(),
            utc_offset_hours: default_digest_offset(),
            weekly: true,
            monthly: true,
            payment: None,
        }
    }
}

impl DigestConfig {
    /// Окно отчёта в UTC с учётом локального смещения: «вчера» считается по
    /// местным суткам, а не по UTC.
    pub fn window_utc(
        &self,
        kind: DigestKind,
        now_utc: DateTime<Utc>,
    ) -> (DateTime<Utc>, DateTime<Utc>) {
        let offset = Duration::hours(self.utc_offset_hours as i64);
        let (from, to) = kind.window(now_utc + offset);
        (from - offset, to - offset)
    }

    pub fn local_now(&self, now_utc: DateTime<Utc>) -> DateTime<Utc> {
        now_utc + Duration::hours(self.utc_offset_hours as i64)
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
#[derive(Debug, Default, Clone)]
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
    /// Покупки и деньги из payment-gw. `None` — источник не настроен;
    /// `Some` с `error` — источник настроен, но недоступен.
    pub purchases: Option<Purchases>,
}

/// Покупки за окно отчёта: транзакции payment-gw и выручка по ним.
#[derive(Debug, Default, Clone)]
pub struct Purchases {
    pub transactions: i64,
    pub confirmed: i64,
    pub canceled: i64,
    pub pending: i64,
    pub revenue: f64,
    /// (тип покупки, число, выручка)
    pub by_type: Vec<(String, i64, f64)>,
    /// (длительность в днях, число, выручка)
    pub by_duration: Vec<(i32, i64, f64)>,
    /// (промокод, число, выручка)
    pub by_promocode: Vec<(String, i64, f64)>,
    /// Почему данных нет: в письме это отдельная строка, иначе пустой блок
    /// читается как «покупок не было».
    pub error: Option<String>,
}

/// Собрать покупки из payment-gw `/analytics` за окно отчёта.
///
/// Деньги живут в базе payment-gw, а не в базе api, поэтому блок добирается
/// HTTP-запросом: сломанный payment-gw не должен ронять письмо целиком.
pub async fn fetch_purchases(
    src: &PaymentSource,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    offset_hours: i32,
) -> Purchases {
    let days = ((to - from).num_days() + 2).max(2);
    let url = format!(
        "{}/analytics/sales?granularity=daily&days={days}",
        src.endpoint.trim_end_matches('/')
    );

    let client = reqwest::Client::new();
    let resp = match client
        .get(&url)
        .bearer_auth(&src.token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Purchases {
                error: Some(format!("payment-gw недоступен: {e}")),
                ..Default::default()
            }
        }
    };
    if !resp.status().is_success() {
        return Purchases {
            error: Some(format!("payment-gw ответил {}", resp.status())),
            ..Default::default()
        };
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Purchases {
                error: Some(format!("ответ payment-gw не разобран: {e}")),
                ..Default::default()
            }
        }
    };

    // Окно — локальные сутки, а бакеты payment-gw — календарные дни его базы.
    // Берём бакеты, чья дата попадает в окно, и складываем вручную.
    let shift = Duration::hours(offset_hours as i64);
    let from_day = (from + shift).format("%Y-%m-%d").to_string();
    let to_day = (to + shift).format("%Y-%m-%d").to_string();
    Purchases::from_analytics(&body, &from_day, &to_day)
}

impl Purchases {
    fn from_analytics(body: &serde_json::Value, from_day: &str, to_day: &str) -> Self {
        let mut p = Purchases::default();
        let mut types: std::collections::BTreeMap<String, (i64, f64)> = Default::default();
        let mut durations: std::collections::BTreeMap<i32, (i64, f64)> = Default::default();
        let mut promos: std::collections::BTreeMap<String, (i64, f64)> = Default::default();

        let empty = Vec::new();
        let buckets = body["data"].as_array().unwrap_or(&empty);
        for b in buckets {
            let day = b["bucket"].as_str().unwrap_or("");
            if day < from_day || day >= to_day {
                continue;
            }
            p.transactions += b["transactions"].as_i64().unwrap_or(0);
            p.confirmed += b["confirmed"].as_i64().unwrap_or(0);
            p.canceled += b["canceled"].as_i64().unwrap_or(0);
            p.pending += b["pending"].as_i64().unwrap_or(0);
            p.revenue += b["revenue"].as_f64().unwrap_or(0.0);

            for t in b["byType"].as_array().unwrap_or(&empty) {
                let key = type_title(t["type"].as_str().unwrap_or("")).to_string();
                let e = types.entry(key).or_default();
                e.0 += t["count"].as_i64().unwrap_or(0);
                e.1 += t["revenue"].as_f64().unwrap_or(0.0);
            }
            for d in b["byDuration"].as_array().unwrap_or(&empty) {
                let key = d["durationDays"].as_i64().unwrap_or(0) as i32;
                let e = durations.entry(key).or_default();
                e.0 += d["count"].as_i64().unwrap_or(0);
                e.1 += d["revenue"].as_f64().unwrap_or(0.0);
            }
            for c in b["byPromocode"].as_array().unwrap_or(&empty) {
                let key = c["promocode"].as_str().unwrap_or("без промокода").to_string();
                let e = promos.entry(key).or_default();
                e.0 += c["count"].as_i64().unwrap_or(0);
                e.1 += c["revenue"].as_f64().unwrap_or(0.0);
            }
        }

        let mut by_type: Vec<(String, i64, f64)> =
            types.into_iter().map(|(k, (c, r))| (k, c, r)).collect();
        by_type.sort_by(|a, b| b.1.cmp(&a.1));
        p.by_type = by_type;

        let mut by_duration: Vec<(i32, i64, f64)> =
            durations.into_iter().map(|(k, (c, r))| (k, c, r)).collect();
        by_duration.sort_by_key(|(d, _, _)| *d);
        p.by_duration = by_duration;

        let mut by_promocode: Vec<(String, i64, f64)> =
            promos.into_iter().map(|(k, (c, r))| (k, c, r)).collect();
        by_promocode.sort_by(|a, b| b.1.cmp(&a.1));
        by_promocode.truncate(5);
        p.by_promocode = by_promocode;

        p
    }

    /// Доля отмен: выше 20% — повод посмотреть, что сломано в оплате.
    fn cancel_ratio(&self) -> f64 {
        if self.transactions == 0 {
            0.0
        } else {
            self.canceled as f64 / self.transactions as f64
        }
    }
}

/// Название продукта из payment-gw в человеческое.
fn type_title(t: &str) -> &str {
    match t {
        "KeyPurchase" => "ключ",
        "SubscriptionRenewal" => "продление",
        "Donation" => "донат",
        "TrafficTopup" => "трафик",
        other => other,
    }
}

fn money(v: f64) -> String {
    // Копейки в сводке не нужны, но и «12» вместо «12.00» — тоже.
    let rounded = (v * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.2}")
    }
}

fn rule_title(rule: &str) -> &str {
    match rule {
        "node_offline" => "🔴 офлайн",
        "node_offline_long" => "🚨 офлайн > 1 ч",
        "node_recovered" => "🟢 вернулась",
        "node_flapping" => "🟠 мигает",
        "node_cpu_high" => "🔥 CPU",
        "node_disk_high" => "💾 диск",
        other => other,
    }
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

    /// Добрать блок покупок из payment-gw: деньги живут в другой базе, поэтому
    /// это отдельный HTTP-запрос. Недоступный payment-gw не отменяет письмо —
    /// в блоке появится причина.
    pub async fn attach_purchases(&mut self, cfg: &DigestConfig) {
        let (from, to) = match (self.from, self.to) {
            (Some(f), Some(t)) => (f, t),
            _ => return,
        };
        if let Some(src) = cfg.payment.as_ref() {
            self.purchases = Some(
                fetch_purchases(src, from, to, cfg.utc_offset_hours).await,
            );
        }
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

    fn delta(&self) -> i64 {
        self.active_now - self.active_at_start
    }

    fn delta_text(&self, value: i64) -> String {
        if value > 0 {
            format!("+{value}")
        } else {
            format!("{value}")
        }
    }

    pub fn subject(&self) -> String {
        let revenue = match &self.purchases {
            Some(p) if p.error.is_none() && p.confirmed > 0 => {
                format!(", выручка {}", money(p.revenue))
            }
            _ => String::new(),
        };
        format!(
            "[FRKN] 📊 сводка за {} — {} / {} нод онлайн, {} алертов, {} подключений{}",
            self.window_label(),
            self.nodes_online,
            self.nodes_total,
            self.alerts_total,
            self.connects,
            revenue
        )
    }

    pub fn render_text(&self) -> String {
        let kind = self.kind.unwrap_or(DigestKind::Daily);
        let mut out = String::new();
        out.push_str(&format!("{}\n{}\n\n", kind.title(), self.window_label()));

        out.push_str("── ФЛОТ ─────────────────────\n");
        out.push_str(&format!(
            "нод: {}   онлайн: {}   офлайн: {}\n",
            self.nodes_total,
            self.nodes_online,
            self.nodes_total - self.nodes_online
        ));
        for (env, total, online) in &self.by_env {
            out.push_str(&format!("  {env:<22} {online}/{total}\n"));
        }

        out.push_str("\n── АЛЕРТЫ ───────────────────\n");
        out.push_str(&format!("всего: {}   простой: {} мин\n", self.alerts_total, self.downtime_minutes));
        for (rule, count) in &self.alerts_by_rule {
            out.push_str(&format!("  {:<16} {count}\n", rule_title(rule)));
        }
        if !self.alert_hosts.is_empty() {
            out.push_str("  топ нод: ");
            let items: Vec<String> = self
                .alert_hosts
                .iter()
                .map(|(h, c)| format!("{h}×{c}"))
                .collect();
            out.push_str(&items.join(", "));
            out.push('\n');
        }

        out.push_str("\n── КЛИЕНТЫ ──────────────────\n");
        out.push_str(&format!(
            "подключений: {}   от {} подписок\n",
            self.connects, self.connecting_subs
        ));
        if !self.by_protocol.is_empty() {
            let items: Vec<String> = self
                .by_protocol
                .iter()
                .map(|(p, c)| format!("{p} {c}"))
                .collect();
            out.push_str(&format!("  протоколы: {}\n", items.join(", ")));
        }
        if !self.by_country.is_empty() {
            let items: Vec<String> = self
                .by_country
                .iter()
                .map(|(p, c)| format!("{p} {c}"))
                .collect();
            out.push_str(&format!("  страны: {}\n", items.join(", ")));
        }

        out.push_str("\n── ЛЮДИ ─────────────────────\n");
        out.push_str(&format!(
            "активных на конец периода: {} ({} к началу)\n",
            self.active_now,
            self.delta_text(self.delta())
        ));
        out.push_str(&format!(
            "новых: {}   истекло: {}   истекают за 7 дней: {}\n",
            self.created, self.expired, self.expiring_7d
        ));
        if !self.by_product.is_empty() {
            let items: Vec<String> = self
                .by_product
                .iter()
                .map(|(p, c)| format!("{p} {c}"))
                .collect();
            out.push_str(&format!("  продукты: {}\n", items.join(", ")));
        }

        out.push_str("\n── ПОКУПКИ ──────────────────\n");
        match &self.purchases {
            None => out.push_str("источник не настроен ([digest.payment])\n"),
            Some(p) => match &p.error {
                Some(e) => out.push_str(&format!("нет данных: {e}\n")),
                None if p.transactions == 0 => out.push_str("за период покупок нет\n".to_string().as_str()),
                None => {
                    out.push_str(&format!(
                        "транзакций: {}   подтверждено: {}   отменено: {}   ждут: {}\n",
                        p.transactions, p.confirmed, p.canceled, p.pending
                    ));
                    out.push_str(&format!("выручка: {}   чек: {}\n", money(p.revenue), money(
                        if p.confirmed > 0 { p.revenue / p.confirmed as f64 } else { 0.0 }
                    )));
                    if !p.by_type.is_empty() {
                        let items: Vec<String> = p
                            .by_type
                            .iter()
                            .map(|(t, c, r)| format!("{t} {c} / {}", money(*r)))
                            .collect();
                        out.push_str(&format!("  типы: {}\n", items.join(", ")));
                    }
                    if !p.by_duration.is_empty() {
                        let items: Vec<String> = p
                            .by_duration
                            .iter()
                            .map(|(d, c, r)| format!("{d} дн {c} / {}", money(*r)))
                            .collect();
                        out.push_str(&format!("  сроки: {}\n", items.join(", ")));
                    }
                    if !p.by_promocode.is_empty() {
                        let items: Vec<String> = p
                            .by_promocode
                            .iter()
                            .map(|(code, c, r)| format!("{code} {c} / {}", money(*r)))
                            .collect();
                        out.push_str(&format!("  промокоды: {}\n", items.join(", ")));
                    }
                    if p.cancel_ratio() > 0.2 && p.transactions >= 3 {
                        out.push_str(&format!(
                            "  ⚠️ отмен {:.0}% — проверить платёжный канал\n",
                            p.cancel_ratio() * 100.0
                        ));
                    }
                }
            },
        }

        out.push_str("\n── ТРЕНД ────────────────────\n");
        for (label, value) in &self.trend {
            out.push_str(&format!("  {label:<14} {value}\n"));
        }

        out
    }

    pub fn render_html(&self) -> String {
        let kind = self.kind.unwrap_or(DigestKind::Daily);
        let mut h = String::new();
        h.push_str(
            "<div style=\"font-family:-apple-system,Segoe UI,Roboto,sans-serif;font-size:15px;color:#111\">",
        );
        h.push_str(&format!(
            "<h2 style=\"margin:0 0 4px\">{}</h2><div style=\"color:#666;margin-bottom:16px\">{}</div>",
            kind.title(),
            self.window_label()
        ));

        h.push_str("<h3 style=\"margin:18px 0 6px\">Флот</h3><table cellpadding=4 style=\"border-collapse:collapse\">");
        h.push_str(&format!(
            "<tr><td>нод</td><td><b>{}</b></td><td>онлайн</td><td><b style=\"color:#1a7f37\">{}</b></td><td>офлайн</td><td><b style=\"color:#c62828\">{}</b></td></tr>",
            self.nodes_total,
            self.nodes_online,
            self.nodes_total - self.nodes_online
        ));
        for (env, total, online) in &self.by_env {
            h.push_str(&format!(
                "<tr><td>{env}</td><td colspan=2>{online}/{total}</td></tr>"
            ));
        }
        h.push_str("</table>");

        h.push_str("<h3 style=\"margin:18px 0 6px\">Люди</h3><table cellpadding=4 style=\"border-collapse:collapse\">");
        h.push_str(&format!(
            "<tr><td>активных сейчас</td><td><b>{}</b></td><td>{}</td><td>к началу периода</td></tr>",
            self.active_now,
            self.delta_text(self.delta())
        ));
        h.push_str(&format!(
            "<tr><td>новых</td><td><b>{}</b></td><td>истекло</td><td><b>{}</b></td><td>истекают за 7 дней</td><td><b>{}</b></td></tr>",
            self.created, self.expired, self.expiring_7d
        ));
        h.push_str("</table>");

        h.push_str("<h3 style=\"margin:18px 0 6px\">Алерты</h3>");
        h.push_str(&format!(
            "<div>всего <b>{}</b>, простой <b>{} мин</b></div>",
            self.alerts_total, self.downtime_minutes
        ));
        if !self.alerts_by_rule.is_empty() {
            h.push_str("<table cellpadding=3 style=\"border-collapse:collapse\">");
            for (rule, count) in &self.alerts_by_rule {
                h.push_str(&format!("<tr><td>{}</td><td><b>{count}</b></td></tr>", rule_title(rule)));
            }
            h.push_str("</table>");
        }

        h.push_str("<h3 style=\"margin:18px 0 6px\">Клиенты</h3>");
        h.push_str(&format!(
            "<div>подключений <b>{}</b> от <b>{}</b> подписок</div>",
            self.connects, self.connecting_subs
        ));
        if !self.by_protocol.is_empty() {
            let items: Vec<String> = self
                .by_protocol
                .iter()
                .map(|(p, c)| format!("{p} — {c}"))
                .collect();
            h.push_str(&format!("<div style=\"color:#555\">протоколы: {}</div>", items.join(", ")));
        }

        h.push_str("<h3 style=\"margin:18px 0 6px\">Покупки</h3>");
        match &self.purchases {
            None => h.push_str("<div style=\"color:#999\">источник не настроен</div>"),
            Some(p) => match &p.error {
                Some(e) => h.push_str(&format!("<div style=\"color:#c62828\">нет данных: {e}</div>")),
                None if p.transactions == 0 => {
                    h.push_str("<div style=\"color:#666\">за период покупок нет</div>")
                }
                None => {
                    h.push_str(&format!(
                        "<table cellpadding=4 style=\"border-collapse:collapse\">\
                         <tr><td>транзакций</td><td><b>{}</b></td><td>подтверждено</td><td><b style=\"color:#1a7f37\">{}</b></td><td>отменено</td><td><b style=\"color:#c62828\">{}</b></td><td>ждут</td><td><b>{}</b></td></tr>\
                         <tr><td>выручка</td><td><b>{}</b></td><td>средний чек</td><td><b>{}</b></td></tr></table>",
                        p.transactions,
                        p.confirmed,
                        p.canceled,
                        p.pending,
                        money(p.revenue),
                        money(if p.confirmed > 0 { p.revenue / p.confirmed as f64 } else { 0.0 })
                    ));
                    if !p.by_type.is_empty() {
                        let items: Vec<String> = p
                            .by_type
                            .iter()
                            .map(|(t, c, r)| format!("{t} — {c} ({})", money(*r)))
                            .collect();
                        h.push_str(&format!(
                            "<div style=\"color:#555\">типы: {}</div>",
                            items.join(", ")
                        ));
                    }
                    if !p.by_duration.is_empty() {
                        let items: Vec<String> = p
                            .by_duration
                            .iter()
                            .map(|(d, c, r)| format!("{d} дн — {c} ({})", money(*r)))
                            .collect();
                        h.push_str(&format!(
                            "<div style=\"color:#555\">сроки: {}</div>",
                            items.join(", ")
                        ));
                    }
                    if !p.by_promocode.is_empty() {
                        let items: Vec<String> = p
                            .by_promocode
                            .iter()
                            .map(|(code, c, r)| format!("{code} — {c} ({})", money(*r)))
                            .collect();
                        h.push_str(&format!(
                            "<div style=\"color:#555\">промокоды: {}</div>",
                            items.join(", ")
                        ));
                    }
                    if p.cancel_ratio() > 0.2 && p.transactions >= 3 {
                        h.push_str(&format!(
                            "<div style=\"color:#c62828;margin-top:4px\">⚠️ отмен {:.0}% — проверить платёжный канал</div>",
                            p.cancel_ratio() * 100.0
                        ));
                    }
                }
            },
        }

        h.push_str("<h3 style=\"margin:18px 0 6px\">Тренд</h3><table cellpadding=3 style=\"border-collapse:collapse\">");
        for (label, value) in &self.trend {
            h.push_str(&format!("<tr><td style=\"color:#666\">{label}</td><td><b>{value}</b></td></tr>"));
        }
        h.push_str("</table>");
        h.push_str("<p style=\"color:#999;font-size:12px;margin-top:18px\">fcore api · отчёт собран из базы, без внешних источников</p></div>");
        h
    }
}

/// Отправить письмо с HTML-частью и текстовым фолбэком.
pub async fn send_digest(
    cfg: &AlertEmailConfig,
    subject: &str,
    text: &str,
    html: &str,
) -> Result<(), String> {
    send_email_html(cfg, subject, text, html).await
}

/// Отправить уже собранный отчёт (или только вернуть его, если dry_run).
pub async fn run_with(
    digest: &Digest,
    cfg: &AlertEmailConfig,
    dry_run: bool,
) -> Result<String, String> {
    let subject = digest.subject();
    if dry_run {
        return Ok(subject);
    }
    send_digest(cfg, &subject, &digest.render_text(), &digest.render_html()).await?;
    Ok(subject)
}

/// Собрать и отправить отчёт. Возвращает (subject, text) для проверки.
pub async fn run(
    client: &PgClient,
    cfg: &AlertEmailConfig,
    kind: DigestKind,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    dry_run: bool,
) -> Result<(String, String), String> {
    let digest = Digest::collect(client, kind, from, to)
        .await
        .map_err(|e| format!("digest query failed: {e}"))?;
    let subject = digest.subject();
    let text = digest.render_text();
    if dry_run {
        return Ok((subject, text));
    }
    send_digest(cfg, &subject, &text, &digest.render_html()).await?;
    Ok((subject, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn daily_window_is_yesterday_in_local_time() {
        let cfg = DigestConfig::default(); // +3
        let now = utc("2026-11-12T06:00:00Z"); // 09:00 MSK
        let (from, to) = cfg.window_utc(DigestKind::Daily, now);
        // вчерашние сутки по МСК = 11.11 00:00..12.11 00:00 MSK
        assert_eq!(from, utc("2026-11-10T21:00:00Z"));
        assert_eq!(to, utc("2026-11-11T21:00:00Z"));
    }

    #[test]
    fn monthly_window_is_the_previous_calendar_month() {
        let cfg = DigestConfig::default();
        let now = utc("2026-11-01T06:00:00Z"); // 1 ноября, 09:00 MSK
        let (from, to) = cfg.window_utc(DigestKind::Monthly, now);
        assert_eq!(from, utc("2026-09-30T21:00:00Z")); // 1 окт 00:00 MSK
        assert_eq!(to, utc("2026-10-31T21:00:00Z")); // 1 ноя 00:00 MSK
    }

    #[test]
    fn weekly_window_is_seven_local_days() {
        let cfg = DigestConfig::default();
        let now = utc("2026-11-12T06:00:00Z");
        let (from, to) = cfg.window_utc(DigestKind::Weekly, now);
        assert_eq!((to - from).num_days(), 7);
        assert_eq!(to, utc("2026-11-11T21:00:00Z"));
    }

    fn bucket(day: &str, tx: i64, conf: i64, canc: i64, rev: f64) -> serde_json::Value {
        serde_json::json!({
            "bucket": day,
            "transactions": tx,
            "confirmed": conf,
            "canceled": canc,
            "pending": tx - conf - canc,
            "revenue": rev,
            "byType": [{"type": "Subscription30Days", "count": conf, "revenue": rev}],
            "byDuration": [{"durationDays": 30, "count": conf, "revenue": rev}],
            "byPromocode": [{"promocode": "PROMO10", "count": conf, "revenue": rev}],
        })
    }

    #[test]
    fn purchases_sum_only_buckets_inside_the_window() {
        let body = serde_json::json!({
            "totals": {"transactions": 99, "confirmed": 99, "canceled": 0, "pending": 0, "revenue": 9999.0},
            "data": [
                bucket("2026-11-13", 5, 4, 1, 400.0),
                bucket("2026-11-12", 3, 3, 0, 300.0),
                bucket("2026-11-11", 7, 7, 0, 700.0), // до окна
            ],
        });
        let p = Purchases::from_analytics(&body, "2026-11-12", "2026-11-14");
        assert_eq!(p.transactions, 8);
        assert_eq!(p.confirmed, 7);
        assert_eq!(p.canceled, 1);
        assert_eq!(p.revenue, 700.0);
        assert_eq!(p.by_type.len(), 1);
        assert_eq!(p.by_type[0].1, 7);
        assert_eq!(p.by_duration[0].0, 30);
        assert_eq!(p.by_promocode[0].0, "PROMO10");
    }

    #[test]
    fn empty_window_is_zero_not_missing() {
        let body = serde_json::json!({"data": []});
        let p = Purchases::from_analytics(&body, "2026-11-12", "2026-11-14");
        assert_eq!(p.transactions, 0);
        assert_eq!(p.revenue, 0.0);
        assert!(p.error.is_none());
        assert_eq!(p.cancel_ratio(), 0.0);
    }

    #[test]
    fn subject_shows_counts() {
        let d = Digest {
            kind: Some(DigestKind::Daily),
            from: Some(utc("2026-11-10T21:00:00Z")),
            to: Some(utc("2026-11-11T21:00:00Z")),
            nodes_total: 64,
            nodes_online: 55,
            alerts_total: 56,
            connects: 3422,
            ..Default::default()
        };
        let subject = d.subject();
        assert!(subject.contains("55 / 64"), "{subject}");
        assert!(subject.contains("56 алертов"), "{subject}");
    }
}
