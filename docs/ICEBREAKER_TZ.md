# FRKN Icebreaker — ТЗ на интеграцию в ffcore

Статус: draft для реализации.
Область: ffcore + управление нодой. Клиент — отдельный контракт.

## 1. Назначение

Добавить в FRKN второй stealth-транспорт `Icebreaker`:

- клиент получает конфиг из `/v1/config`;
- трафик идёт по WebTransport/HTTP3;
- ffcore выдаёт и отзывает доступ per-user/per-device;
- нода принимает несколько клиентов, а не один статический токен;
- оплата, срок подписки и лимиты устройств остаются в существующей модели FRKN.

## 2. Именование

В ffcore использовать только:

- `ProtoTag::Icebreaker` — **последним** в enum;
- `Proto::Icebreaker { ... }` — **последним** в enum;
- строковое имя протокола: `"icebreaker"`;
- UI/API label: `Icebreaker`.

Не добавлять `Stealth` в ffcore. Клиентский `Dopamine` пока может использовать рабочее имя `Stealth`; переименование клиента — отдельная задача.

Причина «последним»: rkyv-дискриминанты и совместимость со старыми нодами/API. Вставка в середину enum ломает бинарный протокол.

## 3. Текущее состояние и ограничение

Сейчас тестовый сервер `frkn-stealth`:

- один статический `--token`;
- одна активная сессия на процесс;
- нет реестра пользователей, IP, срока действия и revoke;
- TUN `10.205.0.0/24`;
- тестовый endpoint `cz2.frkn.org:51821`.

Это годится только для стенда. Для продукта нужен реестр токенов и управление сессиями.

## 4. Пользовательский сценарий

1. Пользователь оплачивает подписку.
2. ffcore проверяет доступ: подписка активна, лимит устройств не превышен.
3. Пользователь создаёт устройство/connection.
4. ffcore выбирает ноду и создаёт/находит `Icebreaker`-клиента на ноде.
5. Нода выдаёт уникальный token и VPN IP, сохраняет хеш токена и привязку к connection.
6. ffcore сохраняет зашифрованный токен и metadata, возвращает клиенту конфиг.
7. Windows-клиент запускает sidecar и подключается.
8. При удалении устройства, окончании подписки или возврате:
   - ffcore вызывает revoke на ноде;
   - нода закрывает активную сессию;
   - токен больше не принимается.
9. Трафик и байты считаются по connection/токену, а не по wire-объёму.

## 5. Данные ffcore

### 5.1 Enum

Файлы:

- `src/memory/tag.rs` — `ProtoTag`;
- `src/memory/connection/proto.rs` — `Proto`.

Добавить:

```rust
// последним
ProtoTag::Icebreaker,
```

```rust
// последним
Proto::Icebreaker {
    token: uuid::Uuid,
    // endpoint/transport metadata, если она нужна биллингу/статистике
},
```

Обновить:

- `Display` для `ProtoTag`;
- `FromStr` для `ProtoTag`;
- `Proto::proto()`;
- `Proto::token()` при необходимости;
- `is_*` helpers при необходимости;
- `sync_conn_matches` и все match по протоколам.

### 5.2 БД

Добавить значение enum:

```sql
ALTER TYPE proto ADD VALUE 'icebreaker';
```

Не хранить Icebreaker-токен в `connections.wg_privkey`.

Отдельная таблица:

```sql
CREATE TABLE icebreaker_connections (
    connection_id UUID PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
    node_id UUID NOT NULL,
    token_ciphertext BYTEA NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    vpn_ip INET NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    last_seen_at TIMESTAMPTZ
);
```

Требования:

- `token_hash` — HMAC/SHA-256 от токена с серверным pepper; plaintext токена в БД не хранить;
- `token_ciphertext` — только если он реально нужен для повторной выдачи; иначе хранить только hash;
- `vpn_ip` уникален в пределах ноды/пула;
- `expires_at` не позже `subscriptions.expires_at`;
- `revoked_at` — soft revoke для аудита.

## 6. API `/v1/config`

### 6.1 Запрос

Новые поля/значения:

```json
{
  "service_type": "vpn",
  "service_protocol": "icebreaker",
  "auth_data": { "...": "..." },
  "connection_id": "uuid",
  "node_id": "uuid"
}
```

Обновить:

- `src/bin/api/http/request.rs`:
  - `TagReq::Icebreaker`;
  - `tags()`;
  - `for_tag()`.
- `src/bin/api/http/handlers/amnezia.rs`:
  - `proto_matches` → `"icebreaker"`;
  - `proto_label` → `"icebreaker"`;
  - `supported_protocols` → добавить `"icebreaker"`;
  - ветка выбора config.

### 6.2 Ответ

Не переиспользовать Amnezia `config` как основной канал. Добавить optional-поле:

```json
{
  "config": "",
  "supported_protocols": ["vless", "awg", "awg-mobile", "hysteria2", "wireguard", "icebreaker"],
  "service_info": { "name": "FRKN Icebreaker", "type": "vpn" },
  "api_config": {
    "service_type": "vpn",
    "service_protocol": "icebreaker",
    "user_country_code": "RU",
    "server_country_code": "NL"
  },
  "icebreaker_config": {
    "url": "https://node.example:443/tunnel",
    "token": "opaque-per-connection-token",
    "sni": "node.example",
    "mtu": 1280,
    "transport": "webtransport"
  }
}
```

Требования:

- `icebreaker_config` — только для owner-запроса и share-токена, которому разрешён именно этот connection;
- не отдавать token в `/v1/services`, логах, telemetry и share feed;
- `config` оставить пустым/отсутствующим, чтобы старые клиенты не пытались парсить его как Amnezia;
- `service_protocol` в `api_config` — `"icebreaker"`.

### 6.3 Ошибки

- нет подписки/истекла → `402 subscription_expired`;
- нет прав на connection → `403`;
- нет доступной ноды/инбаунда → `404`;
- лимит устройств/токенов → `409`;
- нода недоступна → `503` с кодом `icebreaker_node_unavailable`;
- revoke во время запроса → `403`/`410`.

## 7. Управление нодой

### 7.1 Текущая проблема

Сейчас нода принимает один `--token` и одну сессию. Для ffcore этого недостаточно.

### 7.2 Реестр на ноде

Нода должна хранить:

```text
token_hash
connection_id
vpn_ip
expires_at
revoked
created_at
last_seen_at
bytes_up
bytes_down
```

Требования:

- token принимается только если hash найден, не revoked и не expired;
- один connection — один активный token;
- несколько одновременных клиентов на одной ноде;
- выдача VPN IP из пула без коллизий;
- закрытие сессии по revoke;
- периодическая очистка expired/revoked;
- лимит подключений на connection;
- журнал create/revoke/auth-fail без plaintext token.

### 7.3 Management API

Предпочтительно — Unix-socket management API на ноде, доступный только fnode/агенту. fnode проксирует наружу.

Минимальные операции:

```text
POST   /icebreaker/clients
GET    /icebreaker/clients/:connection_id
GET    /icebreaker/clients/:connection_id/connection-key
DELETE /icebreaker/clients/:connection_id
GET    /icebreaker/status
```

`POST /icebreaker/clients`:

```json
{
  "connection_id": "uuid",
  "subscription_id": "uuid",
  "expires_at": "2026-12-01T00:00:00Z",
  "device_label": "Windows",
  "requested_ip": null
}
```

Ответ:

```json
{
  "connection_id": "uuid",
  "token": "opaque-token",
  "vpn_ip": "10.205.0.42",
  "expires_at": "2026-12-01T00:00:00Z"
}
```

Правила:

- `token` возвращается **один раз** при создании/ротации;
- `GET .../connection-key` — только для восстановления/ротации, с аудитом;
- `DELETE` идемпотентен;
- revoke должен закрыть активную сессию, а не только пометить запись;
- management API не должен быть доступен из интернета.

## 8. Клиентский контракт

Windows-клиент (Dopamine):

- получает `icebreaker_config` из `/v1/config`;
- передаёт `url`, `token`, `mtu` sidecar-у;
- не генерирует token самостоятельно;
- при `402`/`403`/`410` показывает соответствующую ошибку и не ретраит бесконечно;
- при revoke завершает туннель;
- не хранит token в открытом логе;
- для split tunneling позже используется отдельный WFP-скоуп; в этот ТЗ не входит.

## 9. Биллинг и статистика

- Считать payload-байты, не wire;
- учитывать per-connection;
- `Icebreaker` должен быть в `metered_conns`, если тариф трафиковый;
- если тариф по дням — трафик всё равно писать в usage, но не блокировать;
- не смешивать байты Icebreaker с AWG/Hysteria;
- связка: `subscription_id` → `connection_id` → `icebreaker_connections` → node.

## 10. Безопасность

- TLS/SNI — обязательны для production; тестовый plain TCP fallback в production не использовать;
- token — криптостойкий, минимум 32 байта;
- хранить только hash; plaintext — только в ответе клиенту;
- token не писать в логи, метрики, crash reports;
- management API — только локально/через аутентифицированный fnode;
- rate-limit auth failures;
- не логировать полный URL с token;
- revoke и expiry проверять на каждом подключении;
- отдельный тестовый token на стенде, не production.

## 11. Тесты и acceptance

Обязательные проверки:

1. Оплаченная подписка → `/v1/config` возвращает `icebreaker_config`, клиент подключается.
2. Трафик идёт через туннель (`example.com`/Instagram/Telegram).
3. Второй пользователь получает отдельный token/IP, обе сессии живут одновременно.
4. Истёкшая подписка → `402`, новая сессия не создаётся.
5. Удаление устройства → revoke, активная сессия закрывается.
6. Повторный запрос после revoke → отказ.
7. Перезапуск ноды: токены/привязки сохраняются, revoked не воскресают.
8. Token не найден в БД в plaintext и не светится в логах.
9. Несколько нод: connection привязан к своей ноде, config не выдаётся с чужой.
10. Старые клиенты без `Icebreaker` не ломаются: неизвестный протокол игнорируется/получает ошибку, но не падает.

## 12. Точки в коде

ffcore:

- `src/memory/tag.rs:26` — `ProtoTag`;
- `src/memory/connection/proto.rs:12` — `Proto`;
- `src/bin/api/http/request.rs:42` — `TagReq`;
- `src/bin/api/http/handlers/amnezia.rs:281` — `GatewayConfigResponse`;
- `src/bin/api/http/handlers/amnezia.rs:333` — `proto_matches`;
- `src/bin/api/http/handlers/amnezia.rs:351` — `proto_label`;
- `src/bin/api/http/handlers/amnezia.rs:1950` — `supported_protocols`;
- `src/bin/api/http/handlers/connection.rs:62` — `sync_conn_matches`;
- миграции — `ffcore/dev/dev.sql` и production migration.

Клиент/сервер Icebreaker:

- `spike/tun/src/main.rs` — текущий прототип;
- `dopamine-stealth` — Windows-клиент и `StealthProtocol`.

## 13. Вне scope этого ТЗ

- per-process split tunneling;
- iOS/Android-клиенты;
- смена WFP-драйвера;
- UI-дизайн карточки;
- биллинг-тарифы и цены;
- TCP-fallback как продуктовый транспорт.

## 14. Порядок внедрения

1. ffcore enum/response/migration — без выдачи пользователям.
2. Management API ноды + мультиюзерный реестр.
3. fnode proxy + create/revoke.
4. `/v1/config` на тестовой подписке.
5. Windows-клиент + один тестовый пользователь.
6. Revoke/expiry/несколько пользователей.
7. Только после этого — production rollout.
