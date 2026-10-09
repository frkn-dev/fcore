use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::Notify;
use uuid::Uuid;

use super::subs::SubscriptionSource;

pub const SLOT_TTL_SECS: i64 = 7 * 24 * 3600;
pub const MAX_SLOTS: i64 = 100;
pub const ONLINE_WINDOW_SECS: i64 = 300;
pub const TS_SKEW_SECS: i64 = 300;
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

static WARNED_WIDTH: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshError {
    BadInput(String),
    UnknownUin,
    BadSignature,
    BadTimestamp,
    InboxFull,
    Conflict,
    SubscriptionRejected,
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct DeviceView {
    pub noise_pubkey: String,
}

#[derive(Debug, Clone)]
pub struct Lookup {
    pub uin: String,
    pub name: String,
    pub online: bool,
    pub devices: Vec<DeviceView>,
}

#[derive(Debug, Clone)]
pub struct SlotView {
    pub from_uin: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone)]
pub struct Envelope {
    pub device_pubkey: String,
    pub ciphertext: String,
}

pub struct Store {
    conn: Mutex<Connection>,
    subs: Arc<dyn SubscriptionSource>,
    notifies: Mutex<HashMap<(String, String), Arc<Notify>>>,
    polls: Mutex<HashMap<(String, String), usize>>,
    poll_timeout: Duration,
}

impl Store {
    pub fn open(
        path: &str,
        subs: Arc<dyn SubscriptionSource>,
        poll_timeout: Duration,
    ) -> Result<Self, MeshError> {
        let conn = Connection::open(path).map_err(|e| MeshError::Internal(e.to_string()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS identities (
                uin TEXT PRIMARY KEY,
                subscription_id TEXT NOT NULL,
                name TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS identities_subscription
                ON identities(subscription_id);
            CREATE TABLE IF NOT EXISTS devices (
                uin TEXT NOT NULL,
                noise_pubkey TEXT NOT NULL UNIQUE,
                signing_pubkey TEXT NOT NULL,
                last_seen INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (uin, noise_pubkey)
            );
            CREATE TABLE IF NOT EXISTS slots (
                uin TEXT NOT NULL,
                device_pubkey TEXT NOT NULL,
                from_uin TEXT NOT NULL,
                ciphertext TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS slots_dest ON slots(uin, device_pubkey);
            ",
        )
        .map_err(|e| MeshError::Internal(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
            subs,
            notifies: Mutex::new(HashMap::new()),
            polls: Mutex::new(HashMap::new()),
            poll_timeout,
        })
    }

    pub async fn register(
        &self,
        subscription_id: &str,
        subscription_secret: &str,
        noise_pubkey: &str,
        signing_pubkey: &str,
        name: Option<&str>,
    ) -> Result<String, MeshError> {
        let sub = parse_subscription(subscription_id, subscription_secret)?;
        if !self
            .subs
            .allows(sub)
            .await
            .map_err(MeshError::Internal)?
        {
            return Err(MeshError::SubscriptionRejected);
        }
        let noise = normalize_pubkey(noise_pubkey)?;
        let signing = normalize_pubkey(signing_pubkey)?;
        let name = match name {
            Some(n) => Some(clean_name(n)?),
            None => None,
        };
        let now = Utc::now().timestamp();
        let conn = self.lock();
        if let Some((uin, bound)) = device_owner(&conn, &noise)? {
            if bound != sub.to_string() {
                return Err(MeshError::Conflict);
            }
            conn.execute(
                "UPDATE devices SET signing_pubkey = ?1, last_seen = ?2 WHERE noise_pubkey = ?3",
                params![signing, now, noise],
            )
            .map_err(internal)?;
            if let Some(name) = name {
                conn.execute(
                    "UPDATE identities SET name = ?1 WHERE uin = ?2",
                    params![name, uin],
                )
                .map_err(internal)?;
            }
            return Ok(uin);
        }
        if let Some(uin) = identity_for_sub(&conn, &sub.to_string())? {
            conn.execute(
                "INSERT INTO devices (uin, noise_pubkey, signing_pubkey, last_seen, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![uin, noise, signing, now],
            )
            .map_err(internal)?;
            if let Some(name) = name {
                conn.execute(
                    "UPDATE identities SET name = ?1 WHERE uin = ?2",
                    params![name, uin],
                )
                .map_err(internal)?;
            }
            tracing::debug!(uin = %uin, "mesh: device joined existing uin");
            return Ok(uin);
        }
        let uin = issue_uin(&conn)?;
        let stored_name = name.unwrap_or_default();
        conn.execute(
            "INSERT INTO identities (uin, subscription_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![uin, sub.to_string(), stored_name, now],
        )
        .map_err(internal)?;
        conn.execute(
            "INSERT INTO devices (uin, noise_pubkey, signing_pubkey, last_seen, created_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![uin, noise, signing, now],
        )
        .map_err(internal)?;
        tracing::debug!(uin = %uin, "mesh: registered");
        Ok(uin)
    }

    pub fn lookup(&self, uin: &str) -> Result<Lookup, MeshError> {
        let conn = self.lock();
        let name: String = conn
            .query_row(
                "SELECT name FROM identities WHERE uin = ?1",
                params![uin],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?
            .ok_or(MeshError::UnknownUin)?;
        let now = Utc::now().timestamp();
        let mut stmt = conn
            .prepare("SELECT noise_pubkey, last_seen FROM devices WHERE uin = ?1")
            .map_err(internal)?;
        let rows = stmt
            .query_map(params![uin], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(internal)?;
        let mut devices = Vec::new();
        let mut online = false;
        let polls = self.polls.lock().unwrap_or_else(|e| e.into_inner());
        for row in rows {
            let (pk, seen) = row.map_err(internal)?;
            if now - seen <= ONLINE_WINDOW_SECS || polls.keys().any(|(u, _)| u == uin) {
                online = true;
            }
            devices.push(DeviceView { noise_pubkey: pk });
        }
        Ok(Lookup {
            uin: uin.to_string(),
            name,
            online,
            devices,
        })
    }

    pub fn send(
        &self,
        from_uin: &str,
        to_uin: &str,
        envelopes: &[Envelope],
        ts: i64,
        sig_b64: &str,
    ) -> Result<(), MeshError> {
        if envelopes.is_empty() {
            return Err(MeshError::BadInput("envelopes required".into()));
        }
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;
        let conn = self.lock();
        if identity_missing(&conn, from_uin)? || identity_missing(&conn, to_uin)? {
            return Err(MeshError::UnknownUin);
        }
        let sender = devices_of(&conn, from_uin)?;
        let msg = send_canonical(to_uin, ts, envelopes);
        let sender_pk = match_device(&sender, &msg, sig_b64)?;
        let targets = devices_of(&conn, to_uin)?;
        let target_keys: Vec<String> = targets.into_iter().map(|(n, _)| n).collect();
        for env in envelopes {
            let pk = normalize_pubkey(&env.device_pubkey)?;
            if env.ciphertext.is_empty() {
                return Err(MeshError::BadInput("empty ciphertext".into()));
            }
            if !target_keys.iter().any(|k| k == &pk) {
                return Err(MeshError::BadInput("device is not on the recipient".into()));
            }
            let pending: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM slots WHERE uin = ?1 AND device_pubkey = ?2 AND expires_at > ?3",
                    params![to_uin, pk, now],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if pending >= MAX_SLOTS {
                return Err(MeshError::InboxFull);
            }
            conn.execute(
                "INSERT INTO slots (uin, device_pubkey, from_uin, ciphertext, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![to_uin, pk, from_uin, env.ciphertext, now + SLOT_TTL_SECS],
            )
            .map_err(internal)?;
            self.wake(to_uin, &pk);
        }
        conn.execute(
            "UPDATE devices SET last_seen = ?1 WHERE noise_pubkey = ?2",
            params![now, sender_pk],
        )
        .map_err(internal)?;
        tracing::debug!(from = %from_uin, to = %to_uin, n = envelopes.len(), "mesh: slots stored");
        Ok(())
    }

    pub fn pull(&self, uin: &str, ts: i64, sig_b64: &str) -> Result<Vec<SlotView>, MeshError> {
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;
        let mut conn = self.lock();
        let device = authed_device(&conn, uin, &pull_canonical(uin, ts), sig_b64)?;
        let slots = take_slots(&mut conn, uin, &device, now)?;
        conn.execute(
            "UPDATE devices SET last_seen = ?1 WHERE noise_pubkey = ?2",
            params![now, device],
        )
        .map_err(internal)?;
        tracing::debug!(uin = %uin, n = slots.len(), "mesh: pulled");
        Ok(slots)
    }

    pub async fn poll(&self, uin: &str, ts: i64, sig_b64: &str) -> Result<Vec<SlotView>, MeshError> {
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;
        let device = {
            let conn = self.lock();
            let device = authed_device(&conn, uin, &poll_canonical(uin, ts), sig_b64)?;
            conn.execute(
                "UPDATE devices SET last_seen = ?1 WHERE noise_pubkey = ?2",
                params![now, device],
            )
            .map_err(internal)?;
            device
        };
        let _guard = PollGuard::enter(self, uin, &device);
        let notify = self.notify_for(uin, &device);
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let mut slots = {
            let mut conn = self.lock();
            take_slots(&mut conn, uin, &device, Utc::now().timestamp())?
        };
        if slots.is_empty() {
            tokio::select! {
                _ = &mut notified => {
                    let mut conn = self.lock();
                    slots = take_slots(&mut conn, uin, &device, Utc::now().timestamp())?;
                }
                _ = tokio::time::sleep(self.poll_timeout) => {}
            }
        }
        tracing::debug!(uin = %uin, n = slots.len(), "mesh: poll");
        Ok(slots)
    }

    pub fn set_name(&self, uin: &str, name: &str, ts: i64, sig_b64: &str) -> Result<(), MeshError> {
        let now = Utc::now().timestamp();
        check_ts(ts, now)?;
        let name = clean_name(name)?;
        let conn = self.lock();
        let device = authed_device(&conn, uin, &name_canonical(uin, ts, &name), sig_b64)?;
        let changed = conn
            .execute(
                "UPDATE identities SET name = ?1 WHERE uin = ?2",
                params![name, uin],
            )
            .map_err(internal)?;
        if changed == 0 {
            return Err(MeshError::UnknownUin);
        }
        conn.execute(
            "UPDATE devices SET last_seen = ?1 WHERE noise_pubkey = ?2",
            params![now, device],
        )
        .map_err(internal)?;
        Ok(())
    }

    pub fn gc(&self) {
        let now = Utc::now().timestamp();
        let conn = self.lock();
        match conn.execute("DELETE FROM slots WHERE expires_at <= ?1", params![now]) {
            Ok(n) if n > 0 => tracing::debug!(n, "mesh: gc"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "mesh: gc failed"),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify_for(&self, uin: &str, device: &str) -> Arc<Notify> {
        let mut map = self.notifies.lock().unwrap_or_else(|e| e.into_inner());
        map.entry((uin.to_string(), device.to_string()))
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    fn wake(&self, uin: &str, device: &str) {
        let map = self.notifies.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = map.get(&(uin.to_string(), device.to_string())) {
            n.notify_one();
        }
    }
}

struct PollGuard<'a> {
    store: &'a Store,
    key: (String, String),
}

impl<'a> PollGuard<'a> {
    fn enter(store: &'a Store, uin: &str, device: &str) -> Self {
        let key = (uin.to_string(), device.to_string());
        let mut polls = store.polls.lock().unwrap_or_else(|e| e.into_inner());
        *polls.entry(key.clone()).or_insert(0) += 1;
        Self { store, key }
    }
}

impl Drop for PollGuard<'_> {
    fn drop(&mut self) {
        let mut polls = self.store.polls.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = polls.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                polls.remove(&self.key);
            }
        }
    }
}

fn internal(e: rusqlite::Error) -> MeshError {
    MeshError::Internal(e.to_string())
}

fn parse_subscription(id: &str, secret: &str) -> Result<Uuid, MeshError> {
    let id = Uuid::parse_str(id.trim()).map_err(|_| {
        MeshError::BadInput("subscription_id must be a uuid".into())
    })?;
    let secret = Uuid::parse_str(secret.trim()).map_err(|_| MeshError::SubscriptionRejected)?;
    if id != secret {
        return Err(MeshError::SubscriptionRejected);
    }
    Ok(id)
}

pub fn normalize_pubkey(raw: &str) -> Result<String, MeshError> {
    fcore::mesh::normalize_pubkey_hex(raw).map_err(|e| match e {
        fcore::mesh::MeshError::BadInput(m) => MeshError::BadInput(m),
        _ => MeshError::BadInput("bad pubkey".into()),
    })
}

fn clean_name(name: &str) -> Result<String, MeshError> {
    if name.chars().count() > 32 || name.chars().any(|c| c.is_control()) {
        return Err(MeshError::BadInput(
            "name must be at most 32 printable characters".into(),
        ));
    }
    Ok(name.to_string())
}

fn check_ts(ts: i64, now: i64) -> Result<(), MeshError> {
    if (now - ts).abs() > TS_SKEW_SECS {
        return Err(MeshError::BadTimestamp);
    }
    Ok(())
}

pub fn choose_width(count_at_width: i64, width: u32) -> u32 {
    if width >= 10 {
        return width;
    }
    let space = 9 * 10i64.pow(width - 1);
    if count_at_width * 10 > space * 8 {
        width + 1
    } else {
        width
    }
}

fn issue_uin(conn: &Connection) -> Result<String, MeshError> {
    let mut width = 5u32;
    loop {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identities WHERE length(uin) = ?1",
                params![width],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let next = choose_width(count, width);
        if next == width {
            break;
        }
        if WARNED_WIDTH.swap(next, Ordering::Relaxed) != next {
            tracing::warn!(
                from = width,
                to = next,
                count,
                "mesh uin space above 80%, widening"
            );
        }
        width = next;
    }
    let lo = 10u64.pow(width - 1);
    let hi = 10u64.pow(width) - 1;
    let mut rng = rand::thread_rng();
    for _ in 0..64 {
        let n: u64 = rng.gen_range(lo..=hi);
        let uin = n.to_string();
        let taken: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identities WHERE uin = ?1",
                params![uin],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if taken == 0 {
            return Ok(uin);
        }
    }
    Err(MeshError::Internal("uin space exhausted".into()))
}

fn device_owner(conn: &Connection, noise: &str) -> Result<Option<(String, String)>, MeshError> {
    conn.query_row(
        "SELECT d.uin, i.subscription_id FROM devices d
         JOIN identities i ON i.uin = d.uin WHERE d.noise_pubkey = ?1",
        params![noise],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(internal)
}

fn identity_for_sub(conn: &Connection, sub: &str) -> Result<Option<String>, MeshError> {
    conn.query_row(
        "SELECT uin FROM identities WHERE subscription_id = ?1",
        params![sub],
        |r| r.get(0),
    )
    .optional()
    .map_err(internal)
}

fn identity_missing(conn: &Connection, uin: &str) -> Result<bool, MeshError> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM identities WHERE uin = ?1",
            params![uin],
            |r| r.get(0),
        )
        .map_err(internal)?;
    Ok(n == 0)
}

fn devices_of(conn: &Connection, uin: &str) -> Result<Vec<(String, String)>, MeshError> {
    let mut stmt = conn
        .prepare("SELECT noise_pubkey, signing_pubkey FROM devices WHERE uin = ?1")
        .map_err(internal)?;
    let rows = stmt
        .query_map(params![uin], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(internal)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(internal)
}

fn authed_device(
    conn: &Connection,
    uin: &str,
    msg: &str,
    sig_b64: &str,
) -> Result<String, MeshError> {
    if identity_missing(conn, uin)? {
        return Err(MeshError::UnknownUin);
    }
    let devices = devices_of(conn, uin)?;
    match_device(&devices, msg, sig_b64)
}

fn match_device(devices: &[(String, String)], msg: &str, sig_b64: &str) -> Result<String, MeshError> {
    for (noise, signing) in devices {
        if verify_ed25519(signing, msg, sig_b64).is_ok() {
            return Ok(noise.clone());
        }
    }
    Err(MeshError::BadSignature)
}

fn take_slots(
    conn: &mut Connection,
    uin: &str,
    device: &str,
    now: i64,
) -> Result<Vec<SlotView>, MeshError> {
    let tx = conn.unchecked_transaction().map_err(internal)?;
    let mut stmt = tx
        .prepare(
            "SELECT rowid, from_uin, ciphertext FROM slots
             WHERE uin = ?1 AND device_pubkey = ?2 AND expires_at > ?3
             ORDER BY rowid",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map(params![uin, device, now], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                SlotView {
                    from_uin: r.get(1)?,
                    ciphertext: r.get(2)?,
                },
            ))
        })
        .map_err(internal)?;
    let mut ids = Vec::new();
    let mut slots = Vec::new();
    for row in rows {
        let (id, slot) = row.map_err(internal)?;
        ids.push(id);
        slots.push(slot);
    }
    drop(stmt);
    for id in ids {
        tx.execute("DELETE FROM slots WHERE rowid = ?1", params![id])
            .map_err(internal)?;
    }
    tx.commit().map_err(internal)?;
    Ok(slots)
}

fn send_canonical(to_uin: &str, ts: i64, envelopes: &[Envelope]) -> String {
    let mut msg = format!("send\n{to_uin}\n{ts}");
    for env in envelopes {
        msg.push('\n');
        msg.push_str(&env.ciphertext);
    }
    msg
}

fn pull_canonical(uin: &str, ts: i64) -> String {
    format!("pull\n{uin}\n{ts}")
}

fn poll_canonical(uin: &str, ts: i64) -> String {
    format!("poll\n{uin}\n{ts}")
}

fn name_canonical(uin: &str, ts: i64, name: &str) -> String {
    format!("name\n{uin}\n{ts}\n{name}")
}

fn verify_ed25519(signing_pubkey_hex: &str, msg: &str, sig_b64: &str) -> Result<(), MeshError> {
    let pk_bytes = hex::decode(signing_pubkey_hex).map_err(|_| MeshError::BadSignature)?;
    let pk: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| MeshError::BadSignature)?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| MeshError::BadSignature)?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|_| MeshError::BadSignature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| MeshError::BadSignature)?;
    vk.verify(msg.as_bytes(), &Signature::from_bytes(&sig_arr))
        .map_err(|_| MeshError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ed25519_dalek::{Signer, SigningKey};
    use std::time::Instant;

    struct Yes;
    #[async_trait]
    impl SubscriptionSource for Yes {
        async fn allows(&self, _id: Uuid) -> Result<bool, String> {
            Ok(true)
        }
    }

    struct No;
    #[async_trait]
    impl SubscriptionSource for No {
        async fn allows(&self, _id: Uuid) -> Result<bool, String> {
            Ok(false)
        }
    }

    fn mem(timeout: Duration) -> Arc<Store> {
        Arc::new(Store::open(":memory:", Arc::new(Yes), timeout).unwrap())
    }

    struct Dev {
        sk: SigningKey,
        sign: String,
        noise: String,
    }

    fn dev(seed: u8) -> Dev {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        Dev {
            sign: hex::encode(sk.verifying_key().to_bytes()),
            noise: hex::encode([seed.wrapping_add(9); 32]),
            sk,
        }
    }

    fn sig(sk: &SigningKey, msg: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.sign(msg.as_bytes()).to_bytes())
    }

    fn sub(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{:02x}", n)
    }

    async fn reg(store: &Store, seed: u8, subscription: &str) -> (Dev, String) {
        let d = dev(seed);
        let uin = store
            .register(subscription, subscription, &d.noise, &d.sign, None)
            .await
            .unwrap();
        (d, uin)
    }

    #[test]
    fn width_grows_past_eighty_percent_up_to_ten() {
        assert_eq!(choose_width(72_000, 5), 5);
        assert_eq!(choose_width(72_001, 5), 6);
        assert_eq!(choose_width(i64::MAX, 10), 10);
    }

    #[tokio::test]
    async fn register_idempotent_five_digit_and_conflict() {
        let store = mem(POLL_TIMEOUT);
        let (d, uin) = reg(&store, 1, &sub(1)).await;
        let again = store
            .register(&sub(1), &sub(1), &d.noise, &d.sign, None)
            .await
            .unwrap();
        assert_eq!(uin, again);
        let n: u32 = uin.parse().unwrap();
        assert!((10_000..=99_999).contains(&n));
        let err = store
            .register(&sub(2), &sub(2), &d.noise, &d.sign, None)
            .await
            .unwrap_err();
        assert_eq!(err, MeshError::Conflict);
        let rejected = Store::open(":memory:", Arc::new(No), POLL_TIMEOUT).unwrap();
        let d2 = dev(2);
        let err = rejected
            .register(&sub(1), &sub(1), &d2.noise, &d2.sign, None)
            .await
            .unwrap_err();
        assert_eq!(err, MeshError::SubscriptionRejected);
        let err = store
            .register(&sub(1), &sub(3), &dev(4).noise, &dev(4).sign, None)
            .await
            .unwrap_err();
        assert_eq!(err, MeshError::SubscriptionRejected);
    }

    #[tokio::test]
    async fn expired_subscription_does_not_block() {
        let store = mem(POLL_TIMEOUT);
        let (d, uin) = reg(&store, 7, &sub(9)).await;
        let again = store
            .register(&sub(9), &sub(9), &d.noise, &d.sign, Some("kept"))
            .await
            .unwrap();
        assert_eq!(again, uin);
        let looked = store.lookup(&uin).unwrap();
        assert_eq!(looked.name, "kept");
        assert!(looked.online);
    }

    #[tokio::test]
    async fn two_devices_keep_separate_copies() {
        let store = mem(POLL_TIMEOUT);
        let (a, a_uin) = reg(&store, 10, &sub(1)).await;
        let (b1, b_uin) = reg(&store, 11, &sub(2)).await;
        let (b2, same) = reg(&store, 12, &sub(2)).await;
        assert_eq!(b_uin, same);
        assert_ne!(a_uin, b_uin);
        let ts = Utc::now().timestamp();
        let envelopes = vec![
            Envelope {
                device_pubkey: b1.noise.clone(),
                ciphertext: "cipher-for-1".into(),
            },
            Envelope {
                device_pubkey: b2.noise.clone(),
                ciphertext: "cipher-for-2".into(),
            },
        ];
        let msg = send_canonical(&b_uin, ts, &envelopes);
        store
            .send(&a_uin, &b_uin, &envelopes, ts, &sig(&a.sk, &msg))
            .unwrap();
        let pull_ts = Utc::now().timestamp();
        let got = store
            .pull(&b_uin, pull_ts, &sig(&b1.sk, &pull_canonical(&b_uin, pull_ts)))
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ciphertext, "cipher-for-1");
        let still = store
            .pull(&b_uin, pull_ts, &sig(&b2.sk, &pull_canonical(&b_uin, pull_ts)))
            .unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].ciphertext, "cipher-for-2");
        let empty = store
            .pull(&b_uin, pull_ts, &sig(&b2.sk, &pull_canonical(&b_uin, pull_ts)))
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn poll_wakes_under_a_second() {
        let store = mem(POLL_TIMEOUT);
        let (a, a_uin) = reg(&store, 21, &sub(1)).await;
        let (b, b_uin) = reg(&store, 22, &sub(2)).await;
        let poll_store = store.clone();
        let uin = b_uin.clone();
        let sk = b.sk.clone();
        let started = Instant::now();
        let handle = tokio::spawn(async move {
            let ts = Utc::now().timestamp();
            poll_store
                .poll(&uin, ts, &sig(&sk, &poll_canonical(&uin, ts)))
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        let ts = Utc::now().timestamp();
        let envelopes = vec![Envelope {
            device_pubkey: b.noise.clone(),
            ciphertext: "wake".into(),
        }];
        let msg = send_canonical(&b_uin, ts, &envelopes);
        store
            .send(&a_uin, &b_uin, &envelopes, ts, &sig(&a.sk, &msg))
            .unwrap();
        let slots = handle.await.unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].ciphertext, "wake");
    }

    #[tokio::test]
    async fn empty_poll_waits_for_timeout() {
        let store = mem(Duration::from_millis(200));
        let (b, b_uin) = reg(&store, 23, &sub(4)).await;
        let started = Instant::now();
        let ts = Utc::now().timestamp();
        let slots = store
            .poll(&b_uin, ts, &sig(&b.sk, &poll_canonical(&b_uin, ts)))
            .await
            .unwrap();
        assert!(slots.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn name_set_and_clear() {
        let store = mem(POLL_TIMEOUT);
        let (d, uin) = reg(&store, 30, &sub(5)).await;
        let ts = Utc::now().timestamp();
        store
            .set_name(&uin, "Ada", ts, &sig(&d.sk, &name_canonical(&uin, ts, "Ada")))
            .unwrap();
        assert_eq!(store.lookup(&uin).unwrap().name, "Ada");
        store
            .set_name(&uin, "", ts, &sig(&d.sk, &name_canonical(&uin, ts, "")))
            .unwrap();
        assert_eq!(store.lookup(&uin).unwrap().name, "");
        let err = store.set_name(&uin, "bad\nname", ts, "aa").unwrap_err();
        assert!(matches!(err, MeshError::BadInput(_)));
    }

    #[tokio::test]
    async fn gc_drops_expired_and_bad_auth_fails() {
        let store = mem(POLL_TIMEOUT);
        let (a, a_uin) = reg(&store, 40, &sub(6)).await;
        let (b, b_uin) = reg(&store, 41, &sub(7)).await;
        let ts = Utc::now().timestamp();
        let envelopes = vec![Envelope {
            device_pubkey: b.noise.clone(),
            ciphertext: "old".into(),
        }];
        store
            .send(
                &a_uin,
                &b_uin,
                &envelopes,
                ts,
                &sig(&a.sk, &send_canonical(&b_uin, ts, &envelopes)),
            )
            .unwrap();
        store
            .lock()
            .execute("UPDATE slots SET expires_at = 1", [])
            .unwrap();
        let pull_ts = Utc::now().timestamp();
        let gone = store
            .pull(
                &b_uin,
                pull_ts,
                &sig(&b.sk, &pull_canonical(&b_uin, pull_ts)),
            )
            .unwrap();
        assert!(gone.is_empty());
        store.gc();
        let left: i64 = store
            .lock()
            .query_row("SELECT COUNT(*) FROM slots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        let err = store
            .pull(&b_uin, pull_ts - 10_000, &sig(&b.sk, &pull_canonical(&b_uin, pull_ts - 10_000)))
            .unwrap_err();
        assert_eq!(err, MeshError::BadTimestamp);
        let err = store
            .pull(&b_uin, pull_ts, &sig(&a.sk, &pull_canonical(&b_uin, pull_ts)))
            .unwrap_err();
        assert_eq!(err, MeshError::BadSignature);
        let err = store
            .send(&a_uin, &b_uin, &envelopes, ts, &sig(&b.sk, "nope"))
            .unwrap_err();
        assert_eq!(err, MeshError::BadSignature);
    }
}
