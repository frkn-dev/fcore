//! Приём событий watchcat и оповещение в Telegram.
//!
//! Контракт: watchcat — источник сигнала, api — единственное место, где
//! решается «кого и когда оповещать». Поэтому Telegram-токен и пороги живут
//! здесь, а не в конфиге демона на ноде.
//!
//! Что уходит в Telegram:
//! - переключение маршрута (`switch`) — всегда, но не чаще `switch_throttle_sec`
//!   на пару (нода, группа): при шторме уведомления схлопываются;
//! - `no_healthy` — всегда и без троттлинга: это «выходов не осталось»;
//! - `intercept_broken` — не чаще `intercept_throttle_sec`, иначе шум
//!   (на живых данных ~13 событий в час при штатной работе).
//!
//! Всё остальное пишется в `watchcat_events` и видно в админке, но молчит.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use warp::{Rejection, Reply};

use crate::postgres::watchcat::WatchcatEvent;
use crate::sync::MemSync;
use fcore::{Connection, ConnectionApiOperations, ConnectionBaseOperations, NodeStorageOperations, Subscription, SubscriptionOperations};

fn json_status(code: u16, message: &str) -> warp::reply::Response {
    let status =
        warp::http::StatusCode::from_u16(code).unwrap_or(warp::http::StatusCode::BAD_REQUEST);
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({ "status": code, "message": message })),
        status,
    )
    .into_response()
}

/// Тело запроса от watchcat.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WatchcatReport {
    /// Имя ноды, на которой работает watchcat.
    pub hostname: String,
    /// Группа выходов (`fr-exit`, `bt-exit`, ...).
    pub group: String,
    /// `switch` | `no_healthy` | `intercept_broken` | `quarantine` | `recovered`.
    pub kind: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    /// Время события на ноде. Пусто — считаем моментом приёма.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

impl WatchcatReport {
    /// Проверка полей. Возвращает текст ошибки для 400.
    pub fn validate(&self) -> Result<(), String> {
        if self.hostname.trim().is_empty() || self.hostname.len() > 255 {
            return Err("hostname: пусто или длиннее 255".into());
        }
        if self.group.trim().is_empty() || self.group.len() > 64 {
            return Err("group: пусто или длиннее 64".into());
        }
        if !matches!(
            self.kind.as_str(),
            "switch" | "no_healthy" | "intercept_broken" | "quarantine" | "recovered"
        ) {
            return Err(format!("kind: неизвестный тип {}", self.kind));
        }
        if self.kind == "switch" && self.to.as_deref().unwrap_or("").is_empty() {
            return Err("to: для switch обязательно".into());
        }
        // Часы на ноде могут врать: слишком старые/будущие отметки заменяем
        // временем приёма, иначе событие ляжет в таблицу с неверной датой.
        if let Some(at) = self.at {
            let now = Utc::now();
            let skew = (now - at).num_seconds().abs();
            if skew > 3600 {
                return Err(format!("at: расхождение с сервером {skew} с"));
            }
        }
        Ok(())
    }

    pub fn into_event(&self) -> WatchcatEvent {
        WatchcatEvent {
            hostname: self.hostname.trim().to_string(),
            group_tag: self.group.trim().to_string(),
            kind: self.kind.clone(),
            from_tag: self.from.clone(),
            to_tag: self.to.clone(),
            reason: self.reason.clone(),
            detail: self.detail.clone(),
            occurred_at: self.at.unwrap_or_else(Utc::now),
        }
    }
}

/// Текст уведомления. Без токенов и без внутренних подробностей.
pub fn format_event(event: &WatchcatEvent) -> String {
    match event.kind.as_str() {
        "switch" => {
            let from = event.from_tag.as_deref().unwrap_or("—");
            let to = event.to_tag.as_deref().unwrap_or("—");
            let reason = event.reason.as_deref().unwrap_or("—");
            format!(
                "🔀 {}\n{}: {} → {}\nпричина: {}",
                event.hostname, event.group_tag, from, to, reason
            )
        }
        "no_healthy" => format!(
            "🛑 {}\n{}: живых выходов не осталось\nпоследний: {}",
            event.hostname,
            event.group_tag,
            event.from_tag.as_deref().unwrap_or("—")
        ),
        "intercept_broken" => format!(
            "⚠️ {}\n{}: перехват не работает, хотя узел отвечает\n{}",
            event.hostname,
            event.group_tag,
            event.detail.as_deref().unwrap_or("")
        ),
        "quarantine" => format!(
            "🚧 {}\n{}: {} в карантине\n{}",
            event.hostname,
            event.group_tag,
            event.to_tag.as_deref().unwrap_or("—"),
            event.reason.as_deref().unwrap_or("")
        ),
        other => format!(
            "ℹ️ {}\n{}: {}\n{}",
            event.hostname,
            event.group_tag,
            other,
            event.detail.as_deref().unwrap_or("")
        ),
    }
}

/// Троттлинг уведомлений: ключ (нода, группа, тип) -> когда в прошлый раз.
#[derive(Clone, Default)]
pub struct NotifyThrottle {
    last: Arc<Mutex<HashMap<(String, String, String), Instant>>>,
}

impl NotifyThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` — можно отправлять, отметка ставится сразу (иначе два события
    /// одной пачки проскочат оба).
    pub fn allow(&self, node: &str, group: &str, kind: &str, window: Duration) -> bool {
        if window.is_zero() {
            return true;
        }
        let key = (node.to_string(), group.to_string(), kind.to_string());
        let mut map = match self.last.lock() {
            Ok(map) => map,
            // Отравленный мьютекс не повод молчать: пусть лучше будет
            // уведомление, чем тишина.
            Err(_) => return true,
        };
        let now = Instant::now();
        match map.get(&key) {
            Some(prev) if now.duration_since(*prev) < window => false,
            _ => {
                map.insert(key, now);
                true
            }
        }
    }
}

/// POST /v1/node/watchcat/event — событие от watchcat.
#[allow(clippy::too_many_arguments)]
pub async fn watchcat_event_handler<N, C, S>(
    report: WatchcatReport,
    memory: MemSync<N, C, S>,
    telegram: String,
    chat_id: String,
    api_base: String,
    switch_throttle_sec: u64,
    intercept_throttle_sec: u64,
    throttle: NotifyThrottle,
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
    if let Err(err) = report.validate() {
        return Ok(json_status(400, &err));
    }

    let event = report.into_event();
    let created = match memory.db.watchcat().record(&event).await {
        Ok(created) => created,
        Err(err) => {
            tracing::error!("watchcat: cannot record event: {err}");
            return Ok(json_status(500, "cannot store event"));
        }
    };
    if !created {
        // Ретрай: событие уже было. Не дублируем и не оповещаем повторно.
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({
                "status": 200,
                "response": { "created": false, "notified": false },
            })),
            warp::http::StatusCode::OK,
        )
        .into_response());
    }

    let window = match event.kind.as_str() {
        "switch" => Duration::from_secs(switch_throttle_sec),
        "intercept_broken" => Duration::from_secs(intercept_throttle_sec),
        _ => Duration::from_secs(0),
    };
    let allowed = throttle.allow(&event.hostname, &event.group_tag, &event.kind, window);

    let mut notified = false;
    if allowed {
        let token = telegram.as_str();
        let chat = chat_id.as_str();
        match (!token.is_empty(), !chat.is_empty()) {
            (true, true) => {
                let client = reqwest::Client::new();
                let text = format_event(&event);
                match crate::alerts::send_telegram(&client, &api_base, token, chat, &text).await {
                    Ok(()) => {
                        notified = true;
                        if let Err(err) = memory.db.watchcat().mark_notified(&event).await {
                            tracing::warn!("watchcat: cannot mark notified: {err}");
                        }
                    }
                    Err(err) => tracing::warn!("watchcat: telegram failed: {err}"),
                }
            }
            _ => tracing::warn!("watchcat: telegram не настроен, событие только в таблице"),
        }
    }

    tracing::info!(
        hostname = event.hostname,
        group = event.group_tag,
        kind = event.kind,
        from = event.from_tag,
        to = event.to_tag,
        notified,
        "watchcat_event"
    );

    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "status": 200,
            "response": { "created": true, "notified": notified },
        })),
        warp::http::StatusCode::OK,
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(kind: &str) -> WatchcatReport {
        WatchcatReport {
            hostname: "msk-tw.frkn.org".into(),
            group: "fr-exit".into(),
            kind: kind.into(),
            from: Some("ufo-fr49".into()),
            to: Some("ufo-fr50".into()),
            reason: Some("active_degraded".into()),
            detail: None,
            at: None,
        }
    }

    #[test]
    fn accepts_switch() {
        assert!(report("switch").validate().is_ok());
    }

    #[test]
    fn rejects_switch_without_target() {
        let mut r = report("switch");
        r.to = None;
        assert!(r.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        assert!(report("what").validate().is_err());
    }

    #[test]
    fn rejects_stale_timestamp() {
        let mut r = report("switch");
        r.at = Some(Utc::now() - chrono::Duration::hours(5));
        assert!(r.validate().is_err(), "часы ноды разошлись больше часа");
    }

    #[test]
    fn allows_fresh_timestamp() {
        let mut r = report("switch");
        r.at = Some(Utc::now() - chrono::Duration::minutes(2));
        assert!(r.validate().is_ok());
    }

    #[test]
    fn renders_switch_with_reason() {
        let r = report("switch");
        let text = format_event(&r.into_event());
        assert!(text.contains("ufo-fr49 → ufo-fr50"));
        assert!(text.contains("active_degraded"));
        assert!(text.contains("msk-tw.frkn.org"));
    }

    #[test]
    fn renders_no_healthy() {
        let r = report("no_healthy");
        let text = format_event(&r.into_event());
        assert!(text.contains("живых выходов не осталось"));
    }

    #[test]
    fn throttle_suppresses_second_event_in_window() {
        let t = NotifyThrottle::new();
        assert!(t.allow("msk-tw", "fr-exit", "switch", Duration::from_secs(60)));
        assert!(!t.allow("msk-tw", "fr-exit", "switch", Duration::from_secs(60)));
        // другая группа — свой счётчик
        assert!(t.allow("msk-tw", "bt-exit", "switch", Duration::from_secs(60)));
    }

    #[test]
    fn throttle_does_not_block_no_healthy() {
        let t = NotifyThrottle::new();
        // окно 0 = без троттлинга
        assert!(t.allow("msk-tw", "fr-exit", "no_healthy", Duration::from_secs(0)));
        assert!(t.allow("msk-tw", "fr-exit", "no_healthy", Duration::from_secs(0)));
    }
}
