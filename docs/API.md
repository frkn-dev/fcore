# ffcore HTTP API

Документация для бинаря `api`. Все запросы/ответы — JSON, если не указано иное.

## Содержание

- [Аутентификация](#аутентификация)
- [Health check](#health-check)
- [Управление подписками](#управление-подписками)
- [Управление подключениями](#управление-подключениями)
- [Управление нодами](#управление-нодами)
- [Кластеры](#кластеры)
- [Ключи активации](#ключи-активации)
- [Trial](#trial)
- [Amnezia gateway](#amnezia-gateway)
- [Admin panel](#admin-panel)
- [Premium panel](#premium-panel)
- [WebSocket метрики](#websocket-метрики)

---

## Аутентификация

| Способ | Где используется | Заголовок |
|---|---|---|
| **Service token** | Управление подписками, нодами, ключами, подключениями | `Authorization: Bearer <settings.service.token>` |
| **Admin token** | Admin API и страница `/admin` | `Authorization: Bearer <settings.service.admin_token>` (API) или `?token=<admin_token>` (страница) |
| **Premium token** | Premium panel | `Authorization: Bearer <subscription.premium_token>` |
| **Без auth** | Публичные endpoint'ы: `/healthcheck`, `/sub`, `/subscription/*`, `/nodes`, `/node/{id}`, `/clusters/*`, `/info/connections/*`, `/key/validate`, `/key/activate`, `/trial`, `/v1/*` | — |

---

## Health check

### `GET /healthcheck`

- **Auth:** нет
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "Ok",
    "response": null
  }
  ```

---

## Управление подписками

### `GET /sub`

Публичная ссылка/конфиг для пользователя.

- **Auth:** нет
- **Query:**
  - `id` — UUID подписки
  - `format` — `Txt | Base64 | Clash`
  - `env` — `all` или имя env (`production`, `experimental`, `dev`, `ru`, `wl`, `custom...`)
  - `proto` — `xray | proxy | wireguard | amneziawg | hysteria2 | vlesstcpreality | vlessgrpcreality | vlessxhttpreality | vlessxhttpcdn | mtproto`
- **Response:**
  - `200 OK text/plain` (format=`Txt`)
  - `200 OK text/base64` (format=`Base64`)
  - `200 OK application/yaml` (format=`Clash`)
  - `400/404` при ошибке

### `GET /subscription/{id}`

Информация о подписке и агрегированный трафик.

- **Auth:** нет
- **Response:** `200 OK`
  ```json
  {
    "id": "uuid",
    "expires": "2026-07-29T18:21:42Z",
    "days": 30,
    "ref_code": "abc123",
    "invited_count": 0,
    "locations": [
      {
        "env": "production",
        "has_xray": true,
        "has_h2": true,
        "has_mtproto": false,
        "has_wg": true,
        "has_awg": true
      }
    ],
    "downlink": 0,
    "uplink": 0,
    "daily_downlink": 0,
    "daily_uplink": 0,
    "monthly_downlink": 0,
    "monthly_uplink": 0,
    "limit_bytes": 10737418240,
    "env_traffic": [
      {
        "env": "production",
        "downlink": 0,
        "uplink": 0,
        "daily_downlink": 0,
        "daily_uplink": 0,
        "monthly_downlink": 0,
        "monthly_uplink": 0
      }
    ]
  }
  ```

### `GET /subscription/{id}/traffic`

История трафика по бакетам.

- **Auth:** нет
- **Query:**
  - `period` — `day | month` (по умолчанию `day`)
  - `from` — опционально, ISO datetime
  - `to` — опционально, ISO datetime
- **Response:** `200 OK`
  ```json
  {
    "subscription_id": "uuid",
    "period": "day",
    "buckets": [
      {
        "bucket": "2026-06-29T00:00:00Z",
        "uplink": 0,
        "downlink": 0,
        "envs": [
          { "env": "production", "uplink": 0, "downlink": 0 }
        ]
      }
    ]
  }
  ```

### `POST /subscription`

Создать подписку.

- **Auth:** service token
- **Body:**
  ```json
  {
    "referred_by": "REFCODE",
    "refer_code": "MYCUSTOM",
    "days": 30,
    "limit_bytes": 10737418240
  }
  ```
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "Subscription <uuid> has been created",
    "response": {
      "id": "uuid",
      "instance": { "Subscription": { /* ... */ } }
    }
  }
  ```

### `PUT /subscription?id={id}&env={env}`

Обновить подписку (продлить, изменить лимит).

- **Auth:** service token
- **Query:**
  - `id` — UUID подписки
  - `env` — опционально
- **Body:**
  ```json
  {
    "days": 30,
    "limit_bytes": 10737418240,
    "referred_by": null,
    "refer_code": null
  }
  ```
- **Response:** `200 OK` / `404` / `500`

---

## Управление подключениями

### `GET /connection?id={id}`

- **Auth:** service token
- **Query:** `id` — UUID подключения
- **Response:** `200 OK` с обёрткой `Instance::Connection` или `404`

### `POST /connection`

Создать подключение.

- **Auth:** service token
- **Body:**
  ```json
  {
    "env": "production",
    "subscription_id": "uuid",
    "proto": "Wireguard",
    "days": 30
  }
  ```
  Доступные значения `proto`: `Wireguard`, `AmneziaWg`, `VlessTcpReality`, `VlessGrpcReality`, `VlessXhttpReality`, `VlessXhttpCdn`, `Vmess`, `Shadowsocks`, `Hysteria2`, `Mtproto`.
- **Response:** `200 OK` с обёрткой `Instance::Connection`
- **Примечание:** для `Wireguard`/`AmneziaWg` IP выделяется автоматически из `wireguard_network` / `amnezia_wireguard_network`.

### `DELETE /connection?id={id}`

- **Auth:** service token
- **Query:** `id` — UUID подключения
- **Response:** `200 OK` / `404`

### `POST /connections/sync`

Публикация подключений в ZMQ-топик.

- **Auth:** service token
- **Body:**
  ```json
  {
    "proto": "VlessTcpReality",
    "last_update": 1710000000,
    "env": "production",
    "topic": "production"
  }
  ```
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "Sync completed",
    "response": { "id": null, "instance": { "Count": 42 } }
  }
  ```

### `GET /info/connections/wireguard?id={id}&env={env}`
### `GET /info/connections/amneziawg?id={id}&env={env}`
### `GET /info/connections/mtproto?id={id}&env={env}`

- **Auth:** нет
- **Query:**
  - `id` — UUID подписки
  - `env` — env
- **Response:** `200 OK`
  ```json
  {
    "nodes": [
      {
        "conn_id": "uuid",
        "label": "FRA-01",
        "env": "production",
        "config": "..."
      }
    ]
  }
  ```

---

## Управление нодами

### `GET /nodes?env={env}`

- **Auth:** нет
- **Query:** `env` — опциональный фильтр
- **Response:** `200 OK` с обёрткой `ResponseMessage<Option<Vec<NodeResponse>>>`

### `GET /node/{id}`

- **Auth:** нет
- **Response:** `200 OK`
  ```json
  {
    "uuid": "uuid",
    "env": "production",
    "hostname": "node1",
    "interface": "eth0",
    "address": "10.0.0.1",
    "inbounds": [],
    "status": "Online",
    "label": "FRA-01",
    "cores": 4,
    "max_bandwidth_bps": 1000000000,
    "metrics": [],
    "country": "DE",
    "type": "Node",
    "cluster": "eu-west"
  }
  ```

### `POST /node`

Регистрация/обновление ноды.

- **Auth:** service token
- **Body:**
  ```json
  {
    "env": "production",
    "hostname": "node1.example.com",
    "address": "10.0.0.1",
    "inbounds": { "VlessTcpReality": { /* ... */ } },
    "uuid": "uuid",
    "label": "FRA-01",
    "interface": "eth0",
    "cores": 4,
    "max_bandwidth_bps": 1000000000,
    "country": "DE",
    "type": "Node",
    "cluster": "eu-west"
  }
  ```
- **Response:** `200 OK` `{ status, message, response: { id: "uuid" } }`

### `DELETE /node/{id}`

- **Auth:** service token
- **Response:** `200 OK` / `404` / `400` / `500`

---

## Кластеры

### `GET /clusters`

- **Auth:** нет
- **Response:** `200 OK` `{ status, message, response: ["eu-west", ...] }`

### `GET /cluster/{name}`

- **Auth:** нет
- **Response:** `200 OK` `{ status, message, response: [NodeResponse, ...] }`

---

## Ключи активации

Ключи подписываются с помощью `settings.service.key_sign_token`.

### `GET /key/validate?key={code}`

- **Auth:** нет
- **Query:** `key` — код вида `XXXXX-XXXXX-...`
- **Response:** `200 OK` с обёрткой `Instance::Key` или `400`/`404`

### `POST /key`

Сгенерировать ключ.

- **Auth:** service token
- **Body:**
  ```json
  { "days": 30, "distributor": "FRKN" }
  ```
- **Response:** `200 OK` с обёрткой `Instance::Key`

### `POST /key/activate`

Активировать ключ для подписки.

- **Auth:** нет
- **Body:**
  ```json
  { "code": "XXXXX-XXXXX-...", "subscription_id": "uuid" }
  ```
- **Response:** `200 OK` с обёрткой `Instance::Key`; `400` если ключ уже активирован; `404` если ключ/подписка не найдены.

---

## Trial

### `POST /trial`

Создать trial-подписку и подключения.

- **Auth:** нет
- **Body:**
  ```json
  {
    "user": "user-hmac-id",
    "email": "user@example.com",
    "referred_by": "WEB",
    "language": "en"
  }
  ```
  Разрешено только одно из полей `user` или `email`.
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "Trial activated. Check email",
    "response": { "id": "uuid", "instance": "None" }
  }
  ```
- **Примечание:** используются `enabled_envs`, `enabled_tags`, `trial_limit_days`, `trial_limit_bytes`, `system_refer_codes`.

---

## Amnezia gateway

Endpoint'ы `/v1/*` используют шифрование RSA+AES, когда настроен `service.agw_private_key_path`.

### Формат запроса

```json
{
  "keyPayload": "<base64(RSA-encrypted AES key+IV)>",
  "apiPayload": "<base64(AES-256-CBC encrypted JSON)>"
}
```

- AES-256-CBC, PKCS#7 padding
- IV — первые 16 байт клиентского 32-байтного IV
- Ответ возвращается как `application/octet-stream` с зашифрованным JSON

### `POST /v1/services`

- **Auth:** нет (шифрование)
- **Body:** `GatewayServicesRequest`
  ```json
  {
    "os_version": "...",
    "app_language": "en",
    "auth_data": { "id": "subscription-uuid" }
  }
  ```
- **Response:** `200 OK` → `GatewayServicesResponse` (зашифрован)

### `POST /v1/account_info`

- **Auth:** нет
- **Body:** `GatewayAccountInfoRequest`
  ```json
  {
    "user_country_code": "RU",
    "service_type": "amnezia-free",
    "auth_data": { "id": "uuid" }
  }
  ```
- **Response:** `200 OK` → `GatewayAccountInfoResponse` (зашифрован)

### `POST /v1/config`

- **Auth:** нет
- **Body:** `GatewayConfigRequest`
  ```json
  {
    "os_version": "...",
    "app_version": "...",
    "app_language": "en",
    "installation_uuid": "uuid",
    "user_country_code": "RU",
    "server_country_code": "DE",
    "service_type": "amnezia-free",
    "service_protocol": "awg",
    "auth_data": { "id": "uuid" },
    "public_key": "...",
    "connection_id": "uuid"
  }
  ```
- **Response:** `200 OK` → `GatewayConfigResponse` с base64-конфигом Amnezia (зашифрован)

---

## Admin panel

Требует `admin_enabled = true` и непустой `admin_token`.

### `GET /admin`

- **Auth:** query token `?token=<admin_token>`
- **Response:** `200 OK text/html`

### `GET /admin/api/state`

- **Auth:** `Authorization: Bearer <admin_token>`
- **Response:** `200 OK`
  ```json
  {
    "nodes": { "total": 5, "online": 4, "offline": 1 },
    "connections": { "total": 100, "active": 95 },
    "subscriptions": { "total": 200, "active": 180 }
  }
  ```

### `GET /admin/api/nodes`

- **Auth:** admin token
- **Response:** `200 OK` → `AdminNodeList`
  ```json
  {
    "nodes": [
      {
        "id": "uuid",
        "env": "production",
        "hostname": "...",
        "address": "10.0.0.1",
        "status": "Online",
        "label": "FRA-01",
        "cluster": "eu-west",
        "country": "DE",
        "metrics": { "memory_used_bytes": 1, "cpu_percent": 5.5 }
      }
    ]
  }
  ```

### `GET /admin/api/subscriptions`

- **Auth:** admin token
- **Response:** `200 OK` → `AdminSubscriptionList`
  ```json
  {
    "subscriptions": [
      {
        "id": "uuid",
        "expires_at": "...",
        "is_active": true,
        "limit_bytes": 10737418240,
        "connections_count": 3,
        "traffic": { "uplink": 0, "downlink": 0 }
      }
    ]
  }
  ```

### `GET /admin/api/subscriptions/{id}/connections`

- **Auth:** admin token
- **Response:** `200 OK` → `AdminConnectionList` с трафиком по каждому подключению.

### `GET /admin/api/connections`

- **Auth:** admin token
- **Response:** `200 OK` → `AdminConnectionList` (поля трафика в этом endpoint всегда `0`).

### `POST /admin/api/subscriptions/{id}/premium`

Назначить подписке премиум-env и сгенерировать `premium_token`.

- **Auth:** admin token
- **Body:**
  ```json
  { "env": "production" }
  ```
- **Response:** `200 OK`
  ```json
  { "premium_token": "prem_xxxxxxxxxxxx" }
  ```
- **Ошибки:** `403` admin disabled, `401` unauthorized, `404` подписка не найдена, `500` ошибка БД.

---

## Premium panel

Аутентификация по `premium_token` подписки.

### `GET /premium/state`

- **Auth:** `Authorization: Bearer <premium_token>`
- **Response:** `200 OK`
  ```json
  {
    "children_count": 5,
    "active_children": 4,
    "connections_count": 12,
    "total_traffic": { "uplink": 0, "downlink": 0 }
  }
  ```

### `GET /premium/child`

- **Auth:** premium token
- **Response:** `200 OK`
  ```json
  [
    {
      "id": "uuid",
      "refer_code": "...",
      "expires_at": "...",
      "is_active": true,
      "created_at": "...",
      "limit_bytes": 10737418240
    }
  ]
  ```

### `POST /premium/child`

- **Auth:** premium token
- **Body:**
  ```json
  { "days": 30, "limit_bytes": 5368709120 }
  ```
- **Response:** `201 Created`
  ```json
  { "id": "uuid" }
  ```

### `PUT /premium/child/{id}`

- **Auth:** premium token
- **Body:**
  ```json
  { "days": 30, "limit_bytes": 5368709120 }
  ```
- **Response:** `200 OK`
  ```json
  { "id": "uuid" }
  ```

### `GET /premium/child/{id}/connections`

- **Auth:** premium token
- **Response:** `200 OK`
  ```json
  [
    {
      "id": "uuid",
      "env": "production",
      "proto": "Wireguard",
      "subscription_id": "uuid",
      "is_deleted": false,
      "traffic": { "uplink": 0, "downlink": 0 }
    }
  ]
  ```

### `POST /premium/child/{id}/connections`

- **Auth:** premium token
- **Body:**
  ```json
  { "env": "production", "proto": "AmneziaWg", "days": 30 }
  ```
- **Response:** `201 Created`
  ```json
  { "id": "uuid" }
  ```
- **Примечание:** child-подписка должна быть активна; `env` должен совпадать с `scope_env` parent, если он задан.

### `DELETE /premium/connections/{id}`

- **Auth:** premium token
- **Response:** `204 No Content`

### `GET /premium/child/{id}/traffic`

- **Auth:** premium token
- **Response:** `200 OK`
  ```json
  {
    "total": { "uplink": 0, "downlink": 0 },
    "daily": { "uplink": 0, "downlink": 0 },
    "monthly": { "uplink": 0, "downlink": 0 },
    "by_env": {
      "production": {
        "total": { "uplink": 0, "downlink": 0 },
        "daily": { "uplink": 0, "downlink": 0 },
        "monthly": { "uplink": 0, "downlink": 0 }
      }
    }
  }
  ```

---

## WebSocket метрики

### `GET /ws/metrics`

- **Auth:** нет
- **Query:**
  - `metric` — имя метрики (обязательно)
  - `from` — опционально, стартовый timestamp (миллисекунды)
  - `mode` — `range` (по умолчанию) | `aggregated` | `multiline`
  - `group_by` — имя тега, например `node`
  - дополнительные фильтры по тегам передаются как query-параметры
- **Response:** WebSocket-поток, по одному JSON-сообщению в секунду.

  `mode=range`:
  ```json
  { "type": "range", "metric": "...", "data": [[ts, value], ...] }
  ```

  `mode=aggregated`:
  ```json
  { "type": "aggregated", "metric": "...", "data": [[ts, avg], ...] }
  ```

  `mode=multiline&group_by=node`:
  ```json
  { "type": "multiline", "metric": "...", "group_by": "node", "data": { "node-id": [[ts, value], ...] } }
  ```

---

## Глобальные замечания

- CORS настраивается через `settings.service.cors_origins`; разрешённые методы: `GET, POST, PUT, DELETE, OPTIONS`.
- Endpoint'ы под service token возвращают `401 Unauthorized`, если заголовок `Authorization: Bearer <token>` отсутствует или неверен.
- Admin endpoint'ы возвращают `404`, если admin отключён; `401`, если admin token неверен. Endpoint назначения premium возвращает `403`, если admin отключён.
- Amnezia `/v1/*` endpoint'ы требуют настроенного `service.agw_private_key_path` и принимают только зашифрованные envelope; без ключа отвечают ошибкой "AGW private key is not configured".
- При создании `Wireguard`/`AmneziaWG` подключений IP выделяется автоматически из соответствующих сетей.

---

# Примеры curl

Ниже примеры для всех endpoint'ов. Замените:

- `http://localhost:8080` — адрес API
- `<service_token>` — `settings.service.token`
- `<admin_token>` — `settings.service.admin_token`
- `<premium_token>` — `subscription.premium_token`
- `<subscription_id>`, `<connection_id>`, `<node_id>`, `<child_id>` — UUID

---

## Health check

```bash
curl -s http://localhost:8080/healthcheck
```

---

## Управление подписками

### `GET /sub`

```bash
curl -s "http://localhost:8080/sub?id=<subscription_id>&format=Txt&env=all&proto=xray"
```

### `GET /subscription/{id}`

```bash
curl -s "http://localhost:8080/subscription/<subscription_id>"
```

### `GET /subscription/{id}/traffic`

```bash
curl -s "http://localhost:8080/subscription/<subscription_id>/traffic?period=day"
```

### `POST /subscription`

```bash
curl -s -X POST http://localhost:8080/subscription \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "referred_by": null,
    "refer_code": "MYCUSTOM",
    "days": 30,
    "limit_bytes": 10737418240
  }'
```

### `PUT /subscription`

```bash
curl -s -X PUT "http://localhost:8080/subscription?id=<subscription_id>" \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "days": 30,
    "limit_bytes": 10737418240,
    "referred_by": null,
    "refer_code": null
  }'
```

---

## Управление подключениями

### `GET /connection`

```bash
curl -s "http://localhost:8080/connection?id=<connection_id>" \
  -H "Authorization: Bearer <service_token>"
```

### `POST /connection`

```bash
curl -s -X POST http://localhost:8080/connection \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "env": "production",
    "subscription_id": "<subscription_id>",
    "proto": "Wireguard",
    "days": 30
  }'
```

### `DELETE /connection`

```bash
curl -s -X DELETE "http://localhost:8080/connection?id=<connection_id>" \
  -H "Authorization: Bearer <service_token>"
```

### `POST /connections/sync`

```bash
curl -s -X POST http://localhost:8080/connections/sync \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "proto": "VlessTcpReality",
    "last_update": 1710000000,
    "env": "production",
    "topic": "production"
  }'
```

### `GET /info/connections/*`

```bash
curl -s "http://localhost:8080/info/connections/wireguard?id=<subscription_id>&env=production"
curl -s "http://localhost:8080/info/connections/amneziawg?id=<subscription_id>&env=production"
curl -s "http://localhost:8080/info/connections/mtproto?id=<subscription_id>&env=production"
```

---

## Управление нодами

### `GET /nodes`

```bash
curl -s "http://localhost:8080/nodes?env=production"
```

### `GET /node/{id}`

```bash
curl -s "http://localhost:8080/node/<node_id>"
```

### `POST /node`

```bash
curl -s -X POST http://localhost:8080/node \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "env": "production",
    "hostname": "node1.example.com",
    "address": "10.0.0.1",
    "inbounds": {},
    "uuid": "<node_id>",
    "label": "FRA-01",
    "interface": "eth0",
    "cores": 4,
    "max_bandwidth_bps": 1000000000,
    "country": "DE",
    "type": "Node",
    "cluster": "eu-west"
  }'
```

### `DELETE /node/{id}`

```bash
curl -s -X DELETE "http://localhost:8080/node/<node_id>" \
  -H "Authorization: Bearer <service_token>"
```

---

## Кластеры

```bash
curl -s http://localhost:8080/clusters
curl -s "http://localhost:8080/cluster/eu-west"
```

---

## Ключи активации

### `GET /key/validate`

```bash
curl -s "http://localhost:8080/key/validate?key=XXXXX-XXXXX-XXXXX"
```

### `POST /key`

```bash
curl -s -X POST http://localhost:8080/key \
  -H "Authorization: Bearer <service_token>" \
  -H "Content-Type: application/json" \
  -d '{"days": 30, "distributor": "FRKN"}'
```

### `POST /key/activate`

```bash
curl -s -X POST http://localhost:8080/key/activate \
  -H "Content-Type: application/json" \
  -d '{
    "code": "XXXXX-XXXXX-XXXXX",
    "subscription_id": "<subscription_id>"
  }'
```

---

## Trial

```bash
curl -s -X POST http://localhost:8080/trial \
  -H "Content-Type: application/json" \
  -d '{
    "user": "user-hmac-id",
    "email": null,
    "referred_by": "WEB",
    "language": "en"
  }'
```

---

## Admin panel

### `GET /admin`

```bash
open "http://localhost:8080/admin?token=<admin_token>"
```

### `GET /admin/api/state`

```bash
curl -s http://localhost:8080/admin/api/state \
  -H "Authorization: Bearer <admin_token>"
```

### `GET /admin/api/nodes`

```bash
curl -s http://localhost:8080/admin/api/nodes \
  -H "Authorization: Bearer <admin_token>"
```

### `GET /admin/api/subscriptions`

```bash
curl -s http://localhost:8080/admin/api/subscriptions \
  -H "Authorization: Bearer <admin_token>"
```

### `GET /admin/api/subscriptions/{id}/connections`

```bash
curl -s "http://localhost:8080/admin/api/subscriptions/<subscription_id>/connections" \
  -H "Authorization: Bearer <admin_token>"
```

### `GET /admin/api/connections`

```bash
curl -s http://localhost:8080/admin/api/connections \
  -H "Authorization: Bearer <admin_token>"
```

### `POST /admin/api/subscriptions/{id}/premium`

```bash
curl -s -X POST "http://localhost:8080/admin/api/subscriptions/<subscription_id>/premium" \
  -H "Authorization: Bearer <admin_token>" \
  -H "Content-Type: application/json" \
  -d '{"env": "production"}'
```

---

## Premium panel

### `GET /premium/state`

```bash
curl -s http://localhost:8080/premium/state \
  -H "Authorization: Bearer <premium_token>"
```

### `GET /premium/child`

```bash
curl -s http://localhost:8080/premium/child \
  -H "Authorization: Bearer <premium_token>"
```

### `POST /premium/child`

```bash
curl -s -X POST http://localhost:8080/premium/child \
  -H "Authorization: Bearer <premium_token>" \
  -H "Content-Type: application/json" \
  -d '{"days": 30, "limit_bytes": 5368709120}'
```

### `PUT /premium/child/{id}`

```bash
curl -s -X PUT "http://localhost:8080/premium/child/<child_id>" \
  -H "Authorization: Bearer <premium_token>" \
  -H "Content-Type: application/json" \
  -d '{"days": 30, "limit_bytes": 5368709120}'
```

### `GET /premium/child/{id}/connections`

```bash
curl -s "http://localhost:8080/premium/child/<child_id>/connections" \
  -H "Authorization: Bearer <premium_token>"
```

### `POST /premium/child/{id}/connections`

```bash
curl -s -X POST "http://localhost:8080/premium/child/<child_id>/connections" \
  -H "Authorization: Bearer <premium_token>" \
  -H "Content-Type: application/json" \
  -d '{"env": "production", "proto": "AmneziaWg", "days": 30}'
```

### `DELETE /premium/connections/{id}`

```bash
curl -s -X DELETE "http://localhost:8080/premium/connections/<connection_id>" \
  -H "Authorization: Bearer <premium_token>"
```

### `GET /premium/child/{id}/traffic`

```bash
curl -s "http://localhost:8080/premium/child/<child_id>/traffic" \
  -H "Authorization: Bearer <premium_token>"
```

---

## WebSocket метрики

```bash
websocat "ws://localhost:8080/ws/metrics?metric=cpu_percent&mode=range&group_by=node&node=node1"
```

Или через `wscat`:

```bash
wscat -c "ws://localhost:8080/ws/metrics?metric=cpu_percent&mode=range"
```

---

# Пример шифрования для Amnezia gateway

Amnezia endpoint'ы (`/v1/services`, `/v1/account_info`, `/v1/config`) принимают и отдают данные в зашифрованном виде, когда настроен `service.agw_private_key_path`.

Алгоритм:
1. Генерируется 32-байтный IV.
2. Из IV берутся первые 16 байт как AES IV.
3. Генерируется 32-байтный AES ключ.
4. JSON-запрос шифруется AES-256-CBC с PKCS#7 padding.
5. AES ключ + IV (32 байта + 16 байт = 48 байт) шифруются RSA PKCS#1 v1.5 публичным ключом сервера.
6. В тело запроса кладутся `keyPayload` (base64 RSA) и `apiPayload` (base64 AES).
7. Ответ — `application/octet-stream` с base64-зашифрованным JSON. Для расшифровки используется приватный ключ клиента (если он есть) или тот же AES контекст.

Ниже пример на Python 3 для **запроса** `/v1/services`:

```python
import base64
import json
import os
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
import requests

SERVER_PUBLIC_KEY_PEM = """
-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA...
-----END PUBLIC KEY-----
"""

public_key = serialization.load_pem_public_key(SERVER_PUBLIC_KEY_PEM.encode())

# 1. Генерируем IV (32 байта, первые 16 — AES IV)
full_iv = os.urandom(32)
aes_iv = full_iv[:16]

# 2. Генерируем AES ключ (32 байта)
aes_key = os.urandom(32)

# 3. Подготавливаем JSON-запрос
payload = json.dumps({
    "os_version": "Android 14",
    "app_language": "en",
    "auth_data": { "id": "<subscription_id>" }
}).encode()

# 4. Шифруем AES-256-CBC PKCS#7
padder_len = 16 - (len(payload) % 16)
padded = payload + bytes([padder_len]) * padder_len

cipher = Cipher(algorithms.AES(aes_key), modes.CBC(aes_iv))
encryptor = cipher.encryptor()
encrypted_payload = encryptor.update(padded) + encryptor.finalize()

# 5. Шифруем AES ключ + IV RSA PKCS#1 v1.5
encrypted_key = public_key.encrypt(
    aes_key + aes_iv,
    padding.PKCS1v15()
)

# 6. Формируем тело
body = {
    "keyPayload": base64.b64encode(encrypted_key).decode(),
    "apiPayload": base64.b64encode(encrypted_payload).decode()
}

resp = requests.post("http://localhost:8080/v1/services", json=body)
print("status:", resp.status_code)
print("content-type:", resp.headers.get("content-type"))

# 7. Расшифровка ответа
encrypted_response = base64.b64decode(resp.content)
decryptor = Cipher(algorithms.AES(aes_key), modes.CBC(aes_iv)).decryptor()
decrypted_padded = decryptor.update(encrypted_response) + decryptor.finalize()
decrypted = decrypted_padded[:-decrypted_padded[-1]]  # удаляем PKCS#7
print(json.loads(decrypted))
```

Зависимости:

```bash
pip install cryptography requests
```

**Важно:** сервер использует свой приватный ключ (`agw_private.pem`) для расшифровки `keyPayload`. Если ключ не настроен, endpoint отвечает ошибкой `AGW private key is not configured`.
