//! Правила оповещений: «нода пропала дольше N», «нода вернулась».
//!
//! Движок намеренно простой и живёт в api, потому что все нужные сигналы уже
//! здесь: `sys.heartbeat` по каждой ноде (MetricsStorage) и реестр нод.
//! Правило = данные (порог/окно/задержка), состояние держим в памяти, факты
//! срабатываний пишем в `alert_events`.
//!
//! Защита от шума:
//! - гистерезис: офлайн-алерт только когда нода молчит дольше `offline_threshold_sec`,
//!   отбой — после `recovery_grace_sec` стабильных heartbeat-ов;
//! - новые ноды не алертим: пока ноду видим меньше `new_node_grace_sec`, молчим;
//! - подавление повторов: не чаще `throttle_sec` на пару (нода, тип).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;

/// Настройки оповещений (`[alerts]` в config.toml).
#[derive(Clone, Debug, serde::Deserialize)]
pub struct AlertsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Куда слать: токен бота и chat_id.
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    #[serde(default)]
    pub telegram_chat_id: Option<String>,
    /// Сколько нода должна молчать, чтобы считаться упавшей.
    #[serde(default = "default_offline_threshold")]
    pub offline_threshold_sec: u64,
    /// Сколько стабильных heartbeat-ов ждём для отбоя.
    #[serde(default = "default_recovery_grace")]
    pub recovery_grace_sec: u64,
    /// Ноду, которую видим меньше этого времени, не алертим.
    #[serde(default = "default_new_node_grace")]
    pub new_node_grace_sec: u64,
    /// Минимальный интервал между одинаковыми алертами по одной ноде.
    #[serde(default = "default_throttle")]
    pub throttle_sec: u64,
    /// База Telegram Bot API (можно подменить на прокси).
    #[serde(default = "default_api_base")]
    pub telegram_api_base: String,
}

fn default_offline_threshold() -> u64 {
    600
}
fn default_recovery_grace() -> u64 {
    120
}
fn default_new_node_grace() -> u64 {
    600
}
fn default_throttle() -> u64 {
    1800
}
fn default_api_base() -> String {
    "https://api.telegram.org".to_string()
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            telegram_bot_token: None,
            telegram_chat_id: None,
            offline_threshold_sec: default_offline_threshold(),
            recovery_grace_sec: default_recovery_grace(),
            new_node_grace_sec: default_new_node_grace(),
            throttle_sec: default_throttle(),
            telegram_api_base: default_api_base(),
        }
    }
}

impl AlertsConfig {
    /// Оповещения реально могут доставляться.
    pub fn can_deliver(&self) -> bool {
        self.enabled && self.telegram_bot_token.is_some() && self.telegram_chat_id.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertKind {
    NodeOffline,
    NodeRecovered,
}

impl AlertKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NodeOffline => "node_offline",
            Self::NodeRecovered => "node_recovered",
        }
    }
}

/// Что движок решил сделать на этом тике.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Ничего не делаем.
    Idle,
    /// Сработало правило: надо отправить и записать.
    Fire {
        kind: AlertKind,
        node_id: Uuid,
        hostname: String,
        env: String,
        /// Сколько секунд нода молчит (для офлайна) или была в алерте (для отбоя).
        duration_sec: i64,
    },
}

fn throttled(track: &NodeTrack, kind: &AlertKind, now: Instant, throttle_sec: u64) -> bool {
    track
        .last_sent
        .get(kind.as_str())
        .map(|at| now.duration_since(*at).as_secs() < throttle_sec)
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct NodeTrack {
    /// Когда мы впервые увидели ноду в этом процессе.
    first_seen: Instant,
    /// Когда нода начала молчать (None — heartbeat свежий).
    offline_since: Option<Instant>,
    /// Когда heartbeat снова стал свежим (для отбоя).
    ok_since: Option<Instant>,
    /// Уже отправили «офлайн» и ждём отбоя.
    alerted: bool,
    /// Последний отправленный алерт по типу — для подавления повторов.
    last_sent: HashMap<&'static str, Instant>,
}

impl NodeTrack {
    fn new(now: Instant) -> Self {
        Self {
            first_seen: now,
            offline_since: None,
            ok_since: None,
            alerted: false,
            last_sent: HashMap::new(),
        }
    }
}

/// Чистая логика правил — без побочных эффектов, чтобы её можно было тестировать.
pub struct AlertsEngine {
    cfg: AlertsConfig,
    nodes: HashMap<Uuid, NodeTrack>,
}

impl AlertsEngine {
    pub fn new(cfg: AlertsConfig) -> Self {
        Self {
            cfg,
            nodes: HashMap::new(),
        }
    }

    /// Один тик по одной ноде.
    ///
    /// `last_heartbeat_ms` — метка последнего `sys.heartbeat` (None, если нода
    /// ещё не присылала метрик вообще).
    pub fn observe(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        last_heartbeat_ms: Option<i64>,
        now_ms: i64,
    ) -> Decision {
        let now = Instant::now();
        // читаем настройки заранее: дальше занимаем self.nodes мутабельно
        let throttle_sec = self.cfg.throttle_sec;
        let new_node_grace = self.cfg.new_node_grace_sec;
        let track = self
            .nodes
            .entry(node_id)
            .or_insert_with(|| NodeTrack::new(now));

        // Нода, которую мы видим впервые и которая ещё не отчитывалась, —
        // это «новая нода» или нода в процессе регистрации: молчим.
        let seen_for = now.duration_since(track.first_seen);
        let new_node = seen_for < Duration::from_secs(new_node_grace);

        let Some(last_ms) = last_heartbeat_ms else {
            return Decision::Idle;
        };

        let silence_sec = (now_ms - last_ms) / 1000;
        let offline = silence_sec >= self.cfg.offline_threshold_sec as i64;

        if offline {
            track.ok_since = None;
            let since = *track.offline_since.get_or_insert(now);
            let offline_for = now.duration_since(since).as_secs() as i64;

            if track.alerted || new_node {
                return Decision::Idle;
            }
            // Порог уже выдержан фактическим молчанием heartbeat-а.
            if throttled(track, &AlertKind::NodeOffline, now, throttle_sec) {
                return Decision::Idle;
            }
            track.alerted = true;
            track
                .last_sent
                .insert(AlertKind::NodeOffline.as_str(), now);
            return Decision::Fire {
                kind: AlertKind::NodeOffline,
                node_id,
                hostname: hostname.to_string(),
                env: env.to_string(),
                duration_sec: silence_sec.max(offline_for),
            };
        }

        // Heartbeat свежий.
        track.offline_since = None;
        if !track.alerted {
            return Decision::Idle;
        }

        let ok_since = *track.ok_since.get_or_insert(now);
        let stable_for = now.duration_since(ok_since).as_secs();
        if stable_for < self.cfg.recovery_grace_sec {
            return Decision::Idle;
        }

        track.alerted = false;
        track.ok_since = None;
        Decision::Fire {
            kind: AlertKind::NodeRecovered,
            node_id,
            hostname: hostname.to_string(),
            env: env.to_string(),
            duration_sec: stable_for as i64,
        }
    }

    /// Затравка состояния из PG после старта процесса.
    ///
    /// Без неё правило не видит ноды, которые молчат с прошлого запуска:
    /// хранилище метрик живёт в памяти и после рестарта пустое.
    pub fn seed(&mut self, node_id: Uuid, already_alerted: bool) {
        let now = Instant::now();
        let grace = Duration::from_secs(self.cfg.new_node_grace_sec);
        let track = self
            .nodes
            .entry(node_id)
            .or_insert_with(|| NodeTrack::new(now));
        // нода нам известна давно — грейс «новой ноды» на неё не действует
        track.first_seen = now.checked_sub(grace).unwrap_or(now);
        track.alerted = already_alerted;
        if already_alerted {
            track.offline_since = Some(now);
        }
    }

    /// Уже отправляли алерт по этой ноде (и отбоя не было).
    pub fn is_alerted(&self, node_id: &Uuid) -> bool {
        self.nodes.get(node_id).map(|t| t.alerted).unwrap_or(false)
    }

    /// Убирает состояние по нодам, которых больше нет в реестре.
    pub fn retain_nodes(&mut self, alive: &[Uuid]) {
        self.nodes.retain(|id, _| alive.contains(id));
    }
}

/// Отправка в Telegram. Ошибки не содержат токена.
pub async fn send_telegram(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let url = format!(
        "{}/bot{token}/sendMessage",
        api_base.trim_end_matches('/')
    );
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("telegram request failed: {e}"))?;

    if !response.status().is_success() {
        let code = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let short: String = body.chars().take(200).collect();
        return Err(format!("telegram http {code}: {short}"));
    }

    Ok(())
}

/// Текст сообщения в Telegram.
pub fn format_message(kind: &AlertKind, hostname: &str, env: &str, duration_sec: i64) -> String {
    let human = human_duration(duration_sec);
    match kind {
        AlertKind::NodeOffline => format!(
            "🔴 Нода офлайн\n{hostname} ({env})\nмолчит: {human}\nвремя: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ),
        AlertKind::NodeRecovered => format!(
            "🟢 Нода вернулась\n{hostname} ({env})\nстабильна: {human}\nвремя: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ),
    }
}

pub fn human_duration(sec: i64) -> String {
    let sec = sec.max(0);
    let (h, m) = (sec / 3600, (sec % 3600) / 60);
    if h > 0 {
        format!("{h} ч {m} мин")
    } else if m > 0 {
        format!("{m} мин")
    } else {
        format!("{sec} с")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AlertsConfig {
        AlertsConfig {
            enabled: true,
            telegram_bot_token: Some("t".into()),
            telegram_chat_id: Some("c".into()),
            offline_threshold_sec: 600,
            recovery_grace_sec: 120,
            new_node_grace_sec: 0,
            throttle_sec: 1800,
            telegram_api_base: default_api_base(),
        }
    }

    fn cfg_with_new_node_grace() -> AlertsConfig {
        AlertsConfig {
            new_node_grace_sec: 600,
            ..cfg()
        }
    }

    fn node() -> Uuid {
        Uuid::nil()
    }

    #[test]
    fn alerts_only_after_threshold_and_once() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;

        // свежий heartbeat
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        // 5 минут молчания — рано
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 300_000), now),
            Decision::Idle
        );
        // 11 минут — сработало
        match e.observe(node(), "n1", "ru", Some(now - 660_000), now) {
            Decision::Fire { kind, .. } => assert_eq!(kind, AlertKind::NodeOffline),
            other => panic!("ожидали offline, получили {other:?}"),
        }
        // повторно не шлём
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 700_000), now),
            Decision::Idle
        );
    }

    #[test]
    fn new_node_is_not_alerted() {
        let mut e = AlertsEngine::new(cfg_with_new_node_grace());
        let now = 1_700_000_000_000i64;
        // нода только что появилась и сразу молчит 20 минут — молчим
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 1_200_000), now),
            Decision::Idle
        );
    }

    #[test]
    fn node_without_heartbeat_is_silent() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;
        assert_eq!(e.observe(node(), "n1", "ru", None, now), Decision::Idle);
    }

    #[test]
    fn recovery_needs_hysteresis() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;
        e.observe(node(), "n1", "ru", Some(now - 900_000), now); // fired
                                                     // первый свежий heartbeat — ещё не отбой
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
    }

    #[test]
    fn duration_is_human() {
        assert_eq!(human_duration(45), "45 с");
        assert_eq!(human_duration(600), "10 мин");
        assert_eq!(human_duration(3720), "1 ч 2 мин");
    }
}
