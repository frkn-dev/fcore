//! Админ-ручки оповещений: история срабатываний и проверка канала.

use chrono::Utc;
use warp::{Rejection, Reply};

use fcore::{
    Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations,
    Subscription, SubscriptionOperations,
};

use crate::sync::MemSync;

use super::admin::check_token;

fn json_status(code: u16, message: &str) -> warp::reply::Response {
    let status =
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_REQUEST);
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({ "status": code, "message": message })),
        status,
    )
    .into_response()
}

/// GET /v1/admin/alerts?limit=50 — последние срабатывания правил.
pub async fn admin_alerts_handler<N, C, S>(
    limit: Option<i64>,
    memory: MemSync<N, C, S>,
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
) -> Result<warp::reply::Response, Rejection>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }

    let limit = limit.unwrap_or(50).clamp(1, 500);
    match memory.db.alerts().recent(limit).await {
        Ok(events) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": { "events": events },
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(e) => {
            tracing::error!("alerts: cannot read events: {e}");
            Ok(json_status(500, "internal"))
        }
    }
}

/// `GET /v1/admin/digest?kind=daily&from=<rfc3339>&to=<rfc3339>` — данные
/// сводки для письма. Собирает их api (данные лежат в его базе), а письмо
/// рендерит и отправляет mrkting: у него расписание, шаблоны и SMTP.
///
/// Окно приходит готовым (`from`/`to`), потому что локальные сутки считает
/// тот, кто отправляет. Без них берётся прошлый период от `kind`.
#[derive(Debug, serde::Deserialize)]
pub struct DigestQuery {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub from: Option<chrono::DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<chrono::DateTime<Utc>>,
}

pub async fn admin_digest_handler<N, C, S>(
    query: DigestQuery,
    memory: MemSync<N, C, S>,
) -> Result<warp::reply::Response, Rejection>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let kind = crate::digest::DigestKind::parse(query.kind.as_deref().unwrap_or("daily"))
        .unwrap_or(crate::digest::DigestKind::Daily);
    let (from, to) = match (query.from, query.to) {
        (Some(f), Some(t)) if t > f => (f, t),
        _ => kind.window(Utc::now()),
    };

    match memory.db.digest().collect(kind, from, to).await {
        Ok(digest) => Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "window": { "from": from, "to": to },
                "digest": digest,
            })),
            warp::http::StatusCode::OK,
        )
        .into_response()),
        Err(e) => {
            tracing::error!("digest: collect failed: {e}");
            Ok(json_status(500, "digest query failed"))
        }
    }
}

/// Тело `POST /v1/admin/alerts/send`: свободный текст от другого сервиса
/// (payment-gw шлёт так пустой сток ключей — молча терять оплаченный заказ
/// нельзя, а своего SMTP у гейтвея нет).
#[derive(Debug, serde::Deserialize)]
pub struct SendAlertRequest {
    #[serde(default)]
    pub subject: String,
    pub text: String,
}

const MAX_ALERT_TEXT: usize = 8000;

/// POST /v1/admin/alerts/send — отправить произвольное оповещение в
/// настроенные каналы. Ручка под `with_service_or_admin_auth`: её дёргают
/// наши же бекенды сервисным токеном (payment-gw сообщает о пустом стоке
/// ключей — своего SMTP у гейтвея нет).
pub async fn admin_alerts_send_handler(
    alerts: crate::alerts::AlertsConfig,
    body: SendAlertRequest,
) -> Result<warp::reply::Response, Rejection> {
    if !alerts.can_deliver() {
        return Ok(json_status(400, "alerts are not configured"));
    }
    let text = body.text.trim();
    if text.is_empty() {
        return Ok(json_status(400, "empty text"));
    }
    let text: String = text.chars().take(MAX_ALERT_TEXT).collect();
    let subject = body.subject.trim();
    let subject = if subject.is_empty() {
        crate::alerts::email_subject("ops")
    } else {
        subject.chars().take(200).collect()
    };

    let client = reqwest::Client::new();
    let report = crate::alerts::notify_report(&alerts, &client, &subject, "ops", &text).await;
    if !report.delivered() {
        tracing::error!("alerts: ops message delivery failed: {:?}", report);
    }

    let code = if report.delivered() { 200 } else { 502 };
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": code,
            "response": report.as_json(),
        })),
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_GATEWAY),
    )
    .into_response())
}

/// Тело `POST /v1/admin/alerts/exit`: состояние группы выходов от watchdog'а
/// на ноде. Нода присылает текущее состояние каждый тик, api решает, писать ли
/// письмо (переход — сразу, повтор — не чаще `alerts.exit_reminder_sec`).
#[derive(Debug, serde::Deserialize)]
pub struct ExitAlertRequest {
    /// `ru` | `ai` | `gov` | `youtube` | …
    pub group: String,
    /// Хост ноды, например `sh-0001-nl01`.
    pub node: String,
    #[serde(default)]
    pub table: Option<u16>,
    /// `exit_dead` | `exit_degraded` | `exit_recovered`.
    pub event: String,
    #[serde(default)]
    pub duration_sec: i64,
    #[serde(default)]
    pub detail: String,
    /// Куда сейчас реально едет трафик группы (`wg-ru-uo`…).
    #[serde(default)]
    pub active_iface: Option<String>,
    #[serde(default)]
    pub probes: Vec<crate::alerts::ExitProbe>,
}

/// Значение из сети (группа, нода) — только `[A-Za-z0-9._-]`, с потолком
/// длины: оно попадает в тему письма и в имя файла журнала.
fn safe_token(raw: &str, max: usize) -> String {
    raw.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(max)
        .collect()
}

/// POST /v1/admin/alerts/exit — событие health группы выходов от watchdog'а.
///
/// Авторизация — `with_mgmt_auth` (сервисный или админский токен): почтовые
/// креденшелы остаются на api, на нодах лежит только токен вызова.
///
/// Ответ: `{"delivered":bool,"action":"sent"|"suppressed","reason":…,
/// "reminder":bool}`. 502 — доставка не удалась, событие надо повторить;
/// 2xx с `action=suppressed` — дедупликация сработала, повторять не нужно.
pub async fn admin_exit_alert_handler<N, C, S>(
    dedupe: std::sync::Arc<std::sync::Mutex<crate::alerts::ExitDedupe>>,
    memory: MemSync<N, C, S>,
    alerts: crate::alerts::AlertsConfig,
    req: ExitAlertRequest,
) -> Result<warp::reply::Response, Rejection>
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
    Connection: From<C>,
    S: SubscriptionOperations + Send + Sync + Clone + 'static + PartialEq + From<Subscription>,
{
    let group = safe_token(&req.group, 32);
    let node = safe_token(&req.node, 64);
    if group.is_empty() || node.is_empty() {
        return Ok(json_status(400, "group and node are required"));
    }
    let Some(event) = crate::alerts::ExitEvent::parse(&req.event) else {
        return Ok(json_status(
            400,
            "unknown event: exit_dead | exit_degraded | exit_recovered",
        ));
    };
    if !alerts.can_deliver() {
        return Ok(json_status(400, "alerts are not configured"));
    }

    // Первое событие по паре в этом процессе: подтянуть историю, иначе рестарт
    // api посреди аварии превратится в повторное письмо.
    let known = dedupe
        .lock()
        .map(|d| d.contains(&group, &node))
        .unwrap_or(false);
    if !known {
        match memory.db.alerts().latest_exit(&group, &node).await {
            Ok(Some((rule, at))) => {
                if let Some(prev) = crate::alerts::ExitEvent::parse(&rule) {
                    if let Ok(mut d) = dedupe.lock() {
                        d.seed(&group, &node, prev, at);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("alerts: cannot seed exit dedupe for {group}/{node}: {e}"),
        }
    }

    let now = Utc::now();
    let decision = match dedupe.lock() {
        Ok(mut d) => d.decide(&group, &node, event, alerts.exit_reminder_sec, now),
        Err(_) => crate::alerts::ExitDecision::Send { reminder: false },
    };

    let reminder = match decision {
        crate::alerts::ExitDecision::Suppress(reason) => {
            // Тик watchdog'а повторяет то же состояние каждые 10 с: это норма,
            // а не событие. Пишем на debug, чтобы журнал не рос.
            tracing::debug!(
                "exit alert suppressed: {} {group}/{node} reason={reason}",
                event.as_str()
            );
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "status": 200,
                    "response": {
                        "group": group,
                        "node": node,
                        "event": event.as_str(),
                        "action": "suppressed",
                        "reason": reason,
                        "delivered": false,
                        "reminder": false,
                    }
                })),
                warp::http::StatusCode::OK,
            )
            .into_response());
        }
        crate::alerts::ExitDecision::Send { reminder } => reminder,
    };

    let text = crate::alerts::format_exit_message(
        event,
        &group,
        &node,
        req.table,
        &req.probes,
        req.duration_sec,
        Some(req.detail.as_str()),
        req.active_iface.as_deref(),
    );
    let state = format!(
        "группа {group}: {}{}",
        event.label_ru(),
        if reminder { " (напоминание)" } else { "" }
    );
    let subject = crate::alerts::email_subject_state(&node, event.emoji(), &state);

    let client = reqwest::Client::new();
    let report = crate::alerts::notify_report(&alerts, &client, &subject, &node, &text).await;
    if !report.delivered() {
        tracing::error!(
            "exit alert delivery failed: {} {group}/{node} {:?}",
            event.as_str(),
            report
        );
        if let Ok(mut d) = dedupe.lock() {
            d.delivery_failed(&group, &node, alerts.exit_reminder_sec);
        }
    } else {
        tracing::info!(
            "alert {}: {node} (exit/{group}) duration={}s delivered={} route=exit reminder={}",
            event.as_str(),
            req.duration_sec,
            report.delivered(),
            reminder
        );
    }

    let details = serde_json::json!({
        "group": group,
        "node": node,
        "table": req.table,
        "detail": req.detail,
        "active_iface": req.active_iface,
        "probes": req.probes.iter().map(|p| serde_json::json!({
            "iface": p.iface,
            "role": p.role,
            "expected_ip": p.expected_ip,
            "observed_ip": p.observed_ip,
            "alive": p.alive,
            "handshake_age_sec": p.handshake_age_sec,
        })).collect::<Vec<_>>(),
        "reminder": reminder,
        "route": "exit",
    });
    if let Err(e) = memory
        .db
        .alerts()
        .record(
            event.as_str(),
            None,
            Some(&node),
            Some(&group),
            req.duration_sec,
            report.delivered(),
            &details,
            Utc::now(),
        )
        .await
    {
        tracing::error!("alerts: cannot record exit event: {e}");
    }

    let code = if report.delivered() { 200 } else { 502 };
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": code,
            "response": {
                "group": group,
                "node": node,
                "event": event.as_str(),
                "action": "sent",
                "reason": serde_json::Value::Null,
                "email": report.email,
                "telegram": report.telegram,
                "delivered": report.delivered(),
                "reminder": reminder,
            }
        })),
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_GATEWAY),
    )
    .into_response())
}

/// POST /v1/admin/alerts/test — отправить тестовое сообщение во все
/// настроенные каналы (телеграм и почта) и вернуть результат по каждому.
pub async fn admin_alerts_test_handler(
    admin_enabled: bool,
    admin_token: String,
    auth_header: Option<String>,
    alerts: crate::alerts::AlertsConfig,
) -> Result<warp::reply::Response, Rejection> {
    if !admin_enabled {
        return Ok(json_status(404, "not found"));
    }
    if !check_token(auth_header, &admin_token) {
        return Ok(json_status(401, "unauthorized"));
    }
    if !alerts.can_deliver() {
        return Ok(json_status(
            400,
            "alerts are not configured (alerts.enabled / telegram token+chat / email recipients)",
        ));
    }

    let text = format!(
        "🧪 FRKN alerts: тестовое сообщение\nвремя: {}",
        Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );

    let client = reqwest::Client::new();
    let subject = crate::alerts::email_subject_state("test", "🧪", "проверка канала");
    let report = crate::alerts::notify_report(&alerts, &client, &subject, "test", &text).await;
    if !report.delivered() {
        tracing::error!("alerts: test delivery failed: {:?}", report);
    }

    let status = if report.delivered() {
        warp::http::StatusCode::OK
    } else {
        warp::http::StatusCode::BAD_GATEWAY
    };
    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": status.as_u16(),
            "response": report.as_json(),
        })),
        status,
    )
    .into_response())
}
