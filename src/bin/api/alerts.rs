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
    /// Тишина короче порога офлайна, но длиннее этого значения, считается «миганием».
    #[serde(default = "default_flap_gap")]
    pub flap_gap_sec: u64,
    /// Окно, за которое считаем мигания.
    #[serde(default = "default_flap_window")]
    pub flap_window_sec: u64,
    /// Сколько переходов offline↔online в окне считаем «нода мигает».
    #[serde(default = "default_flap_threshold")]
    pub flap_threshold: u64,
    /// Через сколько секунд офлайна шлём эскалацию.
    #[serde(default = "default_offline_long")]
    pub offline_long_sec: u64,
    /// Порог CPU, выше которого копим время.
    #[serde(default = "default_cpu_threshold")]
    pub cpu_threshold_pct: f64,
    /// Сколько секунд держится высокая нагрузка, чтобы алертить.
    #[serde(default = "default_cpu_hold")]
    pub cpu_hold_sec: u64,
    /// Ниже этого значения считаем, что нагрузка спала (гистерезис).
    #[serde(default = "default_cpu_reset")]
    pub cpu_reset_pct: f64,
    /// Порог заполнения диска, %.
    #[serde(default = "default_disk_threshold")]
    pub disk_threshold_pct: f64,
    /// Сколько секунд держится заполнение, чтобы алертить.
    #[serde(default = "default_disk_hold")]
    pub disk_hold_sec: u64,
    /// Ниже этого значения считаем, что место освободили (гистерезис).
    #[serde(default = "default_disk_reset")]
    pub disk_reset_pct: f64,
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
fn default_offline_long() -> u64 {
    3600
}
fn default_disk_threshold() -> f64 {
    90.0
}
fn default_disk_hold() -> u64 {
    900
}
fn default_disk_reset() -> f64 {
    85.0
}
fn default_cpu_threshold() -> f64 {
    85.0
}
fn default_cpu_hold() -> u64 {
    600
}
fn default_cpu_reset() -> f64 {
    70.0
}
fn default_flap_gap() -> u64 {
    120
}
fn default_flap_window() -> u64 {
    3600
}
fn default_flap_threshold() -> u64 {
    3
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
            offline_long_sec: default_offline_long(),
            disk_threshold_pct: default_disk_threshold(),
            disk_hold_sec: default_disk_hold(),
            disk_reset_pct: default_disk_reset(),
            cpu_threshold_pct: default_cpu_threshold(),
            cpu_hold_sec: default_cpu_hold(),
            cpu_reset_pct: default_cpu_reset(),
            flap_gap_sec: default_flap_gap(),
            flap_window_sec: default_flap_window(),
            flap_threshold: default_flap_threshold(),
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
    /// Нода лежит дольше часа — эскалация к обычному node_offline.
    NodeOfflineLong,
    /// Нода мигает: много переходов за окно (короткие обрывы, не ловимые порогом).
    NodeFlapping,
    /// CPU держится выше порога дольше заданного времени.
    NodeCpuHigh,
    /// Раздел диска заполнен выше порога дольше заданного времени.
    NodeDiskHigh,
}

impl AlertKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NodeOffline => "node_offline",
            Self::NodeOfflineLong => "node_offline_long",
            Self::NodeRecovered => "node_recovered",
            Self::NodeFlapping => "node_flapping",
            Self::NodeCpuHigh => "node_cpu_high",
            Self::NodeDiskHigh => "node_disk_high",
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
        /// Уточнение для сообщения (раздел диска, значение).
        detail: Option<String>,
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
    /// Последние переходы offline↔online (для правила «мигает»).
    transitions: std::collections::VecDeque<Instant>,
    /// Последнее наблюдённое состояние: true = онлайн.
    last_online: Option<bool>,
    /// Уже отправили алерт «мигает» и ждём, пока окно очистится.
    flap_alerted: bool,
    /// Сейчас длится эпизод тишины (чтобы не считать его много раз).
    hiccup_active: bool,
    /// Уже отправили эскалацию «лежит больше часа».
    long_alerted: bool,
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
            transitions: std::collections::VecDeque::new(),
            last_online: None,
            flap_alerted: false,
            hiccup_active: false,
            long_alerted: false,
            last_sent: HashMap::new(),
        }
    }
}

/// Чистая логика правил — без побочных эффектов, чтобы её можно было тестировать.
pub struct AlertsEngine {
    cfg: AlertsConfig,
    nodes: HashMap<Uuid, NodeTrack>,
    /// Состояние правила CPU: когда нагрузка поднялась и отправлен ли алерт.
    cpu: HashMap<Uuid, CpuTrack>,
    /// Состояние правила диска: (нода, раздел) → с какого момента заполнен.
    disks: HashMap<(Uuid, String), CpuTrack>,
    /// Последний uptime ноды — для обнаружения перезапусков.
    restarts: HashMap<Uuid, RestartTrack>,
}

/// Перезапуски ноды по метрике sys.uptime (ловит короткие мигания 10–48 с).
#[derive(Debug, Clone)]
struct RestartTrack {
    last_uptime: f64,
    count: u32,
    window_start: Instant,
    alerted: bool,
}

#[derive(Debug, Clone)]
struct CpuTrack {
    high_since: std::time::Instant,
    alerted: bool,
}

impl AlertsEngine {
    pub fn new(cfg: AlertsConfig) -> Self {
        Self {
            cfg,
            nodes: HashMap::new(),
            cpu: HashMap::new(),
            disks: HashMap::new(),
            restarts: HashMap::new(),
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
        let flap_window = Duration::from_secs(self.cfg.flap_window_sec);
        let flap_threshold = self.cfg.flap_threshold.max(2) as usize;
        let flap_gap_sec = self.cfg.flap_gap_sec.max(60) as i64;
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

        // Правило «нода мигает»: эпизоды тишины длиннее flap_gap_sec, но короче
        // порога офлайна. Именно такие обрывы не видит правило node_offline.
        if silence_sec >= flap_gap_sec {
            if !track.hiccup_active {
                track.hiccup_active = true;
                track.transitions.push_back(now);
                while let Some(front) = track.transitions.front() {
                    if now.duration_since(*front) > flap_window {
                        track.transitions.pop_front();
                    } else {
                        break;
                    }
                }
                if track.transitions.len() < flap_threshold {
                    track.flap_alerted = false;
                }
                if track.transitions.len() >= flap_threshold && !track.flap_alerted && !new_node {
                    track.flap_alerted = true;
                    let count = track.transitions.len() as i64;
                    return Decision::Fire {
                        kind: AlertKind::NodeFlapping,
                        node_id,
                        hostname: hostname.to_string(),
                        env: env.to_string(),
                        duration_sec: count,
                        detail: None,
                    };
                }
            }
        } else if silence_sec * 2 < flap_gap_sec {
            track.hiccup_active = false;
        }

        if offline {
            track.ok_since = None;
            let since = *track.offline_since.get_or_insert(now);
            let offline_for = now.duration_since(since).as_secs() as i64;

            // Эскалация: нода лежит дольше часа — отдельное сообщение,
            // чтобы 10-минутный алерт не потерялся в потоке.
            let silence = silence_sec.max(offline_for);
            if track.alerted
                && !track.long_alerted
                && !new_node
                && silence >= self.cfg.offline_long_sec.max(600) as i64
            {
                track.long_alerted = true;
                return Decision::Fire {
                    kind: AlertKind::NodeOfflineLong,
                    node_id,
                    hostname: hostname.to_string(),
                    env: env.to_string(),
                    duration_sec: silence,
                    detail: None,
                };
            }

            if track.alerted || new_node {
                return Decision::Idle;
            }
            // Порог уже выдержан фактическим молчанием heartbeat-а.
            if throttled(track, &AlertKind::NodeOffline, now, throttle_sec) {
                return Decision::Idle;
            }
            track.alerted = true;
            track.last_sent.insert(AlertKind::NodeOffline.as_str(), now);
            return Decision::Fire {
                kind: AlertKind::NodeOffline,
                node_id,
                hostname: hostname.to_string(),
                env: env.to_string(),
                duration_sec: silence_sec.max(offline_for),
                detail: None,
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
        track.long_alerted = false;
        track.ok_since = None;
        Decision::Fire {
            kind: AlertKind::NodeRecovered,
            node_id,
            hostname: hostname.to_string(),
            env: env.to_string(),
            duration_sec: stable_for as i64,
            detail: None,
        }
    }

    /// Правило CPU: держится выше порога дольше `cpu_hold_sec`.
    ///
    /// Состояние копим сами (хранилище метрик отдаёт только последнее значение),
    /// поэтому после рестарта api отсчёт начинается заново — это осознанно.
    pub fn observe_cpu(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        cpu_pct: Option<f64>,
    ) -> Decision {
        self.observe_cpu_at(node_id, hostname, env, cpu_pct, Instant::now())
    }

    fn observe_cpu_at(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        cpu_pct: Option<f64>,
        now: Instant,
    ) -> Decision {
        let Some(cpu) = cpu_pct else {
            return Decision::Idle;
        };
        let hold = Duration::from_secs(self.cfg.cpu_hold_sec.max(60));
        let threshold = self.cfg.cpu_threshold_pct;
        let reset = self.cfg.cpu_reset_pct.min(threshold - 1.0);

        if cpu >= threshold {
            let track = self.cpu.entry(node_id).or_insert(CpuTrack {
                high_since: now,
                alerted: false,
            });
            if track.alerted {
                return Decision::Idle;
            }
            let held = now.duration_since(track.high_since);
            if held >= hold {
                track.alerted = true;
                return Decision::Fire {
                    kind: AlertKind::NodeCpuHigh,
                    node_id,
                    hostname: hostname.to_string(),
                    env: env.to_string(),
                    duration_sec: held.as_secs() as i64,
                    detail: None,
                };
            }
        } else if cpu < reset {
            // нагрузка спала — сбрасываем накопление
            self.cpu.remove(&node_id);
        }

        Decision::Idle
    }

    /// Правило диска: раздел заполнен выше порога дольше `disk_hold_sec`.
    pub fn observe_disk(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        disks: &[(String, f64)],
    ) -> Option<Decision> {
        self.observe_disk_at(node_id, hostname, env, disks, Instant::now())
    }

    fn observe_disk_at(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        disks: &[(String, f64)],
        now: Instant,
    ) -> Option<Decision> {
        let hold = Duration::from_secs(self.cfg.disk_hold_sec.max(60));
        let threshold = self.cfg.disk_threshold_pct;
        let reset = self.cfg.disk_reset_pct.min(threshold - 1.0);
        let mut fire = None;

        for (mount, pct) in disks {
            let key = (node_id, mount.clone());
            if *pct >= threshold {
                let track = self.disks.entry(key).or_insert(CpuTrack {
                    high_since: now,
                    alerted: false,
                });
                if track.alerted {
                    continue;
                }
                let held = now.duration_since(track.high_since);
                if held >= hold {
                    track.alerted = true;
                    fire = Some(Decision::Fire {
                        kind: AlertKind::NodeDiskHigh,
                        node_id,
                        hostname: hostname.to_string(),
                        env: env.to_string(),
                        duration_sec: held.as_secs() as i64,
                        detail: Some(format!("{mount}: {}", pct.round())),
                    });
                }
            } else if *pct < reset {
                self.disks.remove(&key);
            }
        }

        fire
    }

    /// Правило перезапусков: uptime уменьшился → нода перезапускалась.
    ///
    /// Это единственный сигнал, который ловит короткие мигания (10–48 с):
    /// тишина такого размера неотличима от штатного интервала heartbeat-а.
    pub fn observe_restarts(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        uptime_sec: Option<f64>,
    ) -> Decision {
        self.observe_restarts_at(node_id, hostname, env, uptime_sec, Instant::now())
    }

    fn observe_restarts_at(
        &mut self,
        node_id: Uuid,
        hostname: &str,
        env: &str,
        uptime_sec: Option<f64>,
        now: Instant,
    ) -> Decision {
        let Some(uptime) = uptime_sec else {
            return Decision::Idle;
        };
        let window = Duration::from_secs(self.cfg.flap_window_sec);
        let threshold = self.cfg.flap_threshold.max(2);

        let track = match self.restarts.get_mut(&node_id) {
            Some(t) => t,
            None => {
                self.restarts.insert(
                    node_id,
                    RestartTrack {
                        last_uptime: uptime,
                        count: 0,
                        window_start: now,
                        alerted: false,
                    },
                );
                return Decision::Idle;
            }
        };

        // Перезапуск: аптайм упал существенно (не путаем с округлением).
        let restarted = uptime + 5.0 < track.last_uptime;
        track.last_uptime = uptime;
        if !restarted {
            return Decision::Idle;
        }

        if now.duration_since(track.window_start) > window {
            track.count = 0;
            track.window_start = now;
            track.alerted = false;
        }
        track.count += 1;
        let count = track.count;
        if track.alerted || u64::from(count) < threshold {
            return Decision::Idle;
        }
        track.alerted = true;
        Decision::Fire {
            kind: AlertKind::NodeFlapping,
            node_id,
            hostname: hostname.to_string(),
            env: env.to_string(),
            duration_sec: count as i64,
            detail: Some(format!("перезапусков за окно: {count}")),
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
    let url = format!("{}/bot{token}/sendMessage", api_base.trim_end_matches('/'));
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
    format_message_full(kind, hostname, env, duration_sec, None)
}

/// То же, но с уточнением (раздел диска и т.п.).
pub fn format_message_full(
    kind: &AlertKind,
    hostname: &str,
    env: &str,
    duration_sec: i64,
    detail: Option<&str>,
) -> String {
    let extra = detail.map(|d| format!("\n{d}")).unwrap_or_default();
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
        AlertKind::NodeOfflineLong => format!(
            "🚨 Нода лежит больше часа\n{hostname} ({env})\nмолчит: {human}\nвремя: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ),
        AlertKind::NodeDiskHigh => format!(
            "💾 Диск почти полон\n{hostname} ({env}){extra}\nдержится: {human}\nвремя: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ),
        AlertKind::NodeCpuHigh => format!(
            "🔥 Высокая нагрузка CPU\n{hostname} ({env})\nдержится: {human}\nвремя: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ),
        AlertKind::NodeFlapping => format!(
            "🟠 Нода мигает\n{hostname} ({env})\n{duration_sec} перехода за окно\nвремя: {}",
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
            offline_long_sec: 3600,
            disk_threshold_pct: 90.0,
            disk_hold_sec: 900,
            disk_reset_pct: 85.0,
            cpu_threshold_pct: 85.0,
            cpu_hold_sec: 600,
            cpu_reset_pct: 70.0,
            flap_gap_sec: 120,
            flap_window_sec: 3600,
            flap_threshold: 3,
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
    fn restarts_detected_by_uptime_drop() {
        let mut e = AlertsEngine::new(cfg());
        let base = std::time::Instant::now();
        let step = |e: &mut AlertsEngine, up: f64, secs: u64| {
            e.observe_restarts_at(
                node(),
                "n1",
                "ru",
                Some(up),
                base + Duration::from_secs(secs),
            )
        };

        assert_eq!(step(&mut e, 1000.0, 0), Decision::Idle); // первое наблюдение
        assert_eq!(step(&mut e, 1030.0, 30), Decision::Idle); // растёт — всё хорошо
                                                              // 1-й перезапуск
        assert_eq!(step(&mut e, 5.0, 60), Decision::Idle);
        assert_eq!(step(&mut e, 40.0, 100), Decision::Idle);
        // 2-й перезапуск
        assert_eq!(step(&mut e, 3.0, 120), Decision::Idle);
        assert_eq!(step(&mut e, 30.0, 150), Decision::Idle);
        // 3-й перезапуск → алерт
        match step(&mut e, 2.0, 180) {
            Decision::Fire {
                kind, duration_sec, ..
            } => {
                assert_eq!(kind, AlertKind::NodeFlapping);
                assert_eq!(duration_sec, 3);
            }
            other => panic!("ожидали node_flapping по перезапускам, получили {other:?}"),
        }
        // повторно не шлём
        assert_eq!(step(&mut e, 20.0, 210), Decision::Idle);
    }

    #[test]
    fn long_offline_escalates_once() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;
        let hour_ago = Some(now - 3_700_000);
        // первое срабатывание — обычный офлайн
        match e.observe(node(), "n1", "ru", hour_ago, now) {
            Decision::Fire { kind, .. } => assert_eq!(kind, AlertKind::NodeOffline),
            other => panic!("ожидали node_offline, получили {other:?}"),
        }
        // следующий тик — эскалация «больше часа»
        match e.observe(node(), "n1", "ru", Some(now - 3_730_000), now) {
            Decision::Fire {
                kind, duration_sec, ..
            } => {
                assert_eq!(kind, AlertKind::NodeOfflineLong);
                assert!(duration_sec >= 3600);
            }
            other => panic!("ожидали node_offline_long, получили {other:?}"),
        }
        // повторно не шлём
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 3_760_000), now),
            Decision::Idle
        );
    }

    #[test]
    fn disk_fires_after_hold_with_mount_in_detail() {
        let mut e = AlertsEngine::new(cfg());
        let base = std::time::Instant::now();
        let disks = vec![("/".to_string(), 93.0), ("/opt".to_string(), 40.0)];
        // первый тик — только начинаем отсчёт
        assert!(e
            .observe_disk_at(node(), "n1", "ru", &disks, base)
            .is_none());
        // 15 минут — алерт, в уточнении раздел и процент
        match e.observe_disk_at(node(), "n1", "ru", &disks, base + Duration::from_secs(901)) {
            Some(Decision::Fire { kind, detail, .. }) => {
                assert_eq!(kind, AlertKind::NodeDiskHigh);
                assert_eq!(detail.as_deref(), Some("/: 93"));
            }
            other => panic!("ожидали node_disk_high, получили {other:?}"),
        }
        // повторно не шлём
        assert!(e
            .observe_disk_at(node(), "n1", "ru", &disks, base + Duration::from_secs(1200))
            .is_none());
    }

    #[test]
    fn disk_below_threshold_stays_quiet() {
        let mut e = AlertsEngine::new(cfg());
        let base = std::time::Instant::now();
        let disks = vec![("/".to_string(), 88.0)];
        assert!(e
            .observe_disk_at(node(), "n1", "ru", &disks, base + Duration::from_secs(3600))
            .is_none());
    }

    #[test]
    fn cpu_high_needs_full_hold_window() {
        let mut e = AlertsEngine::new(cfg());
        let base = std::time::Instant::now();
        // всплеск 3 минуты — не алертим
        for m in 0..3 {
            assert_eq!(
                e.observe_cpu_at(
                    node(),
                    "n1",
                    "ru",
                    Some(92.0),
                    base + Duration::from_secs(60 * m)
                ),
                Decision::Idle
            );
        }
        // спокойно 10 минут выше порога — алерт ровно один
        let mut fired = 0;
        for m in 3..=13 {
            if let Decision::Fire { kind, .. } = e.observe_cpu_at(
                node(),
                "n1",
                "ru",
                Some(92.0),
                base + Duration::from_secs(60 * m),
            ) {
                assert_eq!(kind, AlertKind::NodeCpuHigh);
                fired += 1;
            }
        }
        assert_eq!(fired, 1, "алерт должен быть ровно один");
        // повторно не шлём, пока нагрузка не спадёт
        assert_eq!(
            e.observe_cpu_at(
                node(),
                "n1",
                "ru",
                Some(95.0),
                base + Duration::from_secs(60 * 20)
            ),
            Decision::Idle
        );
    }

    #[test]
    fn cpu_drop_below_hysteresis_resets_accumulation() {
        let mut e = AlertsEngine::new(cfg());
        let base = std::time::Instant::now();
        assert_eq!(
            e.observe_cpu_at(node(), "n1", "ru", Some(90.0), base),
            Decision::Idle
        );
        // на 6-й минуте просадка ниже гистерезиса — накопление сбрасывается
        assert_eq!(
            e.observe_cpu_at(
                node(),
                "n1",
                "ru",
                Some(50.0),
                base + Duration::from_secs(360)
            ),
            Decision::Idle
        );
        // ещё 9 минут выше порога — до 10 не дотянули
        let mut fired = false;
        for m in 7..=15 {
            if let Decision::Fire { .. } = e.observe_cpu_at(
                node(),
                "n1",
                "ru",
                Some(90.0),
                base + Duration::from_secs(60 * m),
            ) {
                fired = true;
            }
        }
        assert!(!fired, "после сброса окно должно считаться заново");
    }

    #[test]
    fn flapping_counts_silence_episodes() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;
        // три эпизода тишины по 150с (> flap_gap 120с), между ними свежие heartbeat
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 150_000), now),
            Decision::Idle
        );
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 150_000), now),
            Decision::Idle
        );
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        match e.observe(node(), "n1", "ru", Some(now - 150_000), now) {
            Decision::Fire { kind, .. } => assert_eq!(kind, AlertKind::NodeFlapping),
            other => panic!("ожидали node_flapping, получили {other:?}"),
        }
        // дальше молчим, пока окно не очистится
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 150_000), now),
            Decision::Idle
        );
    }

    #[test]
    fn single_short_glitch_does_not_trigger_flapping() {
        let mut e = AlertsEngine::new(cfg());
        let now = 1_700_000_000_000i64;
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now), now),
            Decision::Idle
        );
        // один короткий обрыв 40 секунд — ниже и порога офлайна, и flap_gap
        assert_eq!(
            e.observe(node(), "n1", "ru", Some(now - 40_000), now),
            Decision::Idle
        );
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
