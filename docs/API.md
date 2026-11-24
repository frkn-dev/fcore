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
- [Гео-листы роутера](#гео-листы-роутера)
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
| **Без auth** | Публичные endpoint'ы: `/healthcheck`, `/sub`, `/subscription/*`, `/status`, `/clusters/*`, `/info/connections/*`, `/key/validate`, `/key/activate`, `/trial`, `/v1/*` | — |
| **Без auth (nginx)** | `/router/geo/*` отдаёт nginx перед api из файлов зон; в бинаре api этого маршрута нет | — |

---

## Health check

### `GET /healthcheck`

- **Auth:** нет
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "Ok (v0.6.56)",
    "response": null
  }
  ```
  Сообщение содержит версию бинарника — удобно проверять, что после деплоя
  поднялась именно новая сборка.

---

## Управление подписками

### `GET /sub`

Публичная ссылка/конфиг для пользователя.

- **Auth:** нет
- **Query:**
  - `id` — UUID подписки
  - `format` — `Txt | Base64 | Clash`
  - `env` — `all` или имя env (`production`, `experimental`, `dev`, `ru`, `wl`, `gaming`, `bs`, `bs-prem`, `custom...`)
  - `proto` — `xray | proxy | wireguard | amneziawg | hysteria2 | vlesstcpreality | vlessgrpcreality | vlessxhttpreality | vlessxhttpcdn | mtproto`
- **Response:**
  - `200 OK text/plain` (format=`Txt`)
  - `200 OK text/base64` (format=`Base64`)
  - `200 OK application/yaml` (format=`Clash`)
  - `400/404` при ошибке

Clash отдаётся для `xray`, `proxy` и `hysteria2` (VLESS/Reality/xhttp,
VMess и Hysteria2). WG-семейство и `mtproto` в подписках не отдаются.

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
    "is_child": false,
    "parent_id": null,
    "plane": "v1",
    "v2_since": null,
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

- `is_child` — подписка создана владельцем selfhosted-аккаунта (пользователь,
  `parent_id != null`). Такой аккаунт **не может** держать своих нод:
  `/private/install-token` отвечает ему 403. Кабинет по этому полю показывает бейдж
  «Пользователь» и не рисует selfhosted-блоки. `parent_id` — только id владельца,
  никаких других его данных в ответе нет. Старые сборки api поля не отдают —
  сайт обязан трактовать отсутствие как `false`.

- `own_nodes_limit` — сколько своих нод аккаунт может зарегистрировать (0 — нельзя).
  Своя нода — возможность **любого** аккаунта, а не тариф: `plan_kind` в гейте
  `/private/install-token` не участвует, решают только `parent_id`, этот лимит и
  ёмкости (ноды/живые токены). Новые подписки получают квоту из
  `[service.private] default_own_nodes_limit` (по умолчанию 3), старые строки не
  пересчитываются. Кабинет показывает блок «своя нода» при `> 0`, независимо от
  `plan_kind`. Старые сборки api поля не отдают — трактовать отсутствие как `0`.

- `plane` — плоскость подписки: `v1` (только per-protocol `connections`),
  `v2` (только shard-`peers`) или `dual` (есть и то, и другое). Свежая подписка
  начинает с `v1`; первый живой peer переводит её в `v2`, а при живой v1-connection
  — в `dual`. `v2_since` — момент этого перехода, `null` для чистой `v1`. Поля
  пишет миграция `0030` (backfill по существующим данным) и создание peer;
  старые сборки api их не отдают — трактовать отсутствие как `v1` / `null`.

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

### Два тарифа на одной подписке (`plans[]`)

Тарифы продаются отдельно и живут в отдельных шардах: `standard` (срок,
трафик не режется) и `lite` — в витрине «Super Duper» (пакет трафика, без
срока). Права хранятся **грантами** (`subscription_access`), по одному на
план, поэтому у одной подписки может быть и тот, и другой: у каждого свой
срок и своя квота.

`GET /subscription/{id}` и `GET /cabinet/{id}` отдают список `plans`. В нём
всегда оба продаваемых плана — этого достаточно, чтобы нарисовать «Standard
активен» и кнопку «Докупить Super Duper», не зная, какие ещё планы бывают.
План, которого нет у подписки, приходит с `active: false`.

```json
"plans": [
  {
    "plan": "standard",
    "title": "Standard",
    "active": true,
    "expires_at": "2027-01-19T12:00:00Z",
    "traffic_used": 3221225472,
    "traffic_limit": null,
    "purchasable": true
  },
  {
    "plan": "lite",
    "title": "Super Duper",
    "active": false,
    "expires_at": null,
    "traffic_used": 0,
    "traffic_limit": null,
    "purchasable": false
  }
]
```

- `active` — живой грант этого плана: его собственный срок не вышел и его
  квота не выбрана. `expires_at`/`traffic_used`/`traffic_limit` — тоже только
  про этот план; `traffic_limit: null` у `standard` — «без лимита».
- `purchasable: false` — купить сейчас нельзя: ни один шард не объявлен под
  этот план (`plan_not_configured`), либо объявлен, но у него нет живой ноды
  или адресного блока (`no_live_shard`). Это состояние кнопки, а не ошибка.
- Подписка без грантов (легаси) показывает план из собственной строки
  подписки: `plan_kind` + `limit_bytes` + срок, чтобы платный v1-юзер не
  выглядел как «ничего не куплено».
- План гранта, которого нет среди продаваемых (например `gaming`), тоже
  попадает в список — выданное вручную не должно исчезать из кабинета.

### `POST /subscription/{id}/buy`

Начало покупки второго (или первого) плана из ЛК: «Докупить обычный» /
«Докупить Super Duper». Ключ при такой покупке не выдаётся — оплата сразу
покупает грант.

- **Auth:** нет; идентификатор подписки — это capability из пути (как
  `GET /cabinet/{id}`). Тело не может назвать другую подписку или цену.
- **Body:**
  ```json
  { "plan": "lite" }
  ```
  `plan` — `standard` или `lite`; `super-duper` принимается как alias.
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "plan available",
    "response": {
      "subscription_id": "uuid",
      "plan": "lite",
      "title": "Super Duper",
      "active": false,
      "expires_at": null,
      "traffic_limit": null,
      "payment": {
        "method": "POST",
        "routes": {
          "platega": "/payment/platega/subscription/create",
          "planta": "/payment/planta/subscription/create"
        },
        "body": { "subscription_id": "uuid", "plan": "lite" }
      }
    }
  }
  ```
  Цены здесь нет и быть не может: она живёт в `[[app.plans]]` платёжного
  шлюза. Api отвечает на два вопроса, которые ЛК не должен угадывать, —
  **можно ли продать этот план сейчас** и **какой запрос в шлюз послать**.
- **Ошибки:** `400 unknown_plan`; `404 subscription_not_found`;
  `503 plan_not_configured` — под план не объявлен ни один шард (никуда не
  сажаем, в частности не подменяем стандартным шардом); `503 no_live_shard` —
  шард объявлен, но живой ноды/адресов нет.

### `POST /subscription/{id}/plan`

Исполнение оплаченной покупки плана — её зовёт payment-gw после успешного
платежа. Создаёт грант плана или продлевает существующий.

- **Auth:** service token (как у остальных внутренних ручек).
- **Body:**
  ```json
  {
    "plan": "lite",
    "days": 0,
    "traffic_bytes": 107374182400,
    "payment_ref": "platega-transaction-uuid",
    "source": "payment",
    "protos": ["amnezia_peer", "wireguard_peer"]
  }
  ```
  - `plan` — `standard` | `lite` (`super-duper` — alias).
  - `days` — купленный срок; для `standard` обязателен.
  - `traffic_bytes` (или `traffic_gib`) — купленный объём; для `lite`
    **прибавляется** к квоте гранта (второй пакет — это топ-ап, не сброс).
  - `payment_ref` — **обязателен**, это ключ идемпотентности (id транзакции
    шлюза; принимаются также `trace_id`/`transaction_id`).
  - `source`, `protos` — опционально: метка в `sources` (по умолчанию
    `payment`) и теги гранта (по умолчанию оба peer-протокола).
- **Что пишется:** грант на шарде плана (`env_capacity.plan`), `plan`,
  `expires_at` = «остаток срока + `days`» (просроченный стартует с сегодня),
  `traffic_limit` = прежняя квота + `traffic_bytes`. Ссылка на платёж
  сохраняется в `sources` как `pay:<payment_ref>`; в ответах ЛК она не
  показывается.
- **Идемпотентность:** повторный вебхук с тем же `payment_ref` отвечает
  `200` с `"duplicate": true` и ничего не меняет (проверка и запись идут в
  одной транзакции под advisory-lock по ссылке).
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "plan purchase fulfilled",
    "response": {
      "subscription_id": "uuid",
      "plan": "lite",
      "title": "Super Duper",
      "duplicate": false,
      "grant": {
        "plan": "lite",
        "plan_title": "Super Duper",
        "protos": ["amnezia_peer", "wireguard_peer"],
        "sources": ["payment"],
        "expires_at": null,
        "traffic_limit": 107374182400,
        "used_bytes": 0
      },
      "capacity": { "plan": "lite", "used": 1, "limit": 5000, "overflow": false }
    }
  }
  ```
- **Ошибки:** `400 unknown_plan | payment_ref_required | empty_purchase |
  days_required | bad_days | bad_traffic | unknown_proto`; `404
  subscription_not_found`; `503 plan_not_configured | no_live_shard` (грант не
  пишется, деньги не превращаются в права, которые никто не обслуживает).

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
- **Идемпотентность:** для протоколов-одиночек (всё, кроме семейства WireGuard) на пару `(subscription, env, proto)` живёт один коннект. Повторный `POST` с той же парой возвращает `200 OK` и **существующий** id, а не создаёт второй. Удалённый коннект идемпотентности не мешает: после `DELETE` создаётся новый (новые ключи/токен — удаление отзывает старые).
- **Ошибки:** `400` — env не обслуживается (`Env not served: … нет нод` / ни одна нода env не отдаёт этот протокол), подписка не найдена или истекла, IP вне пула; `409` — живой коннект уже есть, но пара занята особым образом (plan-issued); `500` — прочее (детали только в логе).

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

### `GET /status`

Публичный санитизированный статус нод (для status.frkn.org): без инбаундов, тегов, UUID и метрик; `ports` — дедуплицированный список портов ноды для проверок доступности.

- **Auth:** нет
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "message": "List of node statuses",
    "response": [
      {
        "hostname": "node1.example.com",
        "address": "10.0.0.1",
        "label": "FRA-01",
        "country": "DE",
        "status": "Online",
        "type": "Node",
        "ports": [443, 8443],
        "pool": "EU Рилзопровод",
        "pool_translation": { "ru": "EU Рилзопровод", "en": "EU Reel pipeline" },
        "speedtest_url": "https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443"
      }
    ]
  }
  ```

Поля `pool` / `pool_translation` (0.6.133+):

- `pool` — значение `[node] pool` из конфига ноды **как есть**: без нормализации, перевода и slug. Это идентификатор пула, клиент группирует по нему.
- `pool_translation` — карта `язык → отображаемое имя` (`[node] pool_translation`); отсутствующий язык = показывать `pool`.
- Оба поля **всегда присутствуют** в JSON и равны `null`, если у ноды пула нет (легаси-ноды). Так потребитель отличает «пула нет» от «старый api, который поля не знает».
- Personal-ноды (self-hosted) в ответ не попадают, как и раньше.

Поле `speedtest_url` (0.6.134+):

- **Base URL собственного бенча ноды**: `https://<host>:<port>`, всегда `https`, порт указан явно (сейчас 19443), **без завершающего слэша и без пути**. Потребитель сам добавляет `/speedtest/1mb.test`, `/speedtest/10mb.test`, `/speedtest/ping.txt` (с `?cb=` против кэша).
- Значение отдаётся **дословно** из `nodes.bench_url`, ничего не достраивается: имена бенча живут в зоне `beacon.frkn.org` как `<hoster>-<cc>-<city>-bench-<nn>.bench.beacon.frkn.org`, и вывести их из hostname/адреса ноды нельзя.
- **Per node**, а не per address и не per protocol: у ноды с несколькими `node_ips` и с несколькими протоколами адрес бенча один.
- `null` — у ноды нет бенча (премиум-слоты). Потребитель показывает «нет тест-файла», а не «0 Мбит/с».
- Источник — **конфиг ноды**: `[node] bench_url` в `/opt/fnode/config.toml`. Нода присылает поле `bench_url` в payload регистрации (`POST /node` / `/private/nodes`), api нормализует его так же, как нода (обязателен `http(s)://` со хостом, завершающий слэш срезается), и пишет в `nodes.bench_url`. Значение обновляется на рестарте ноды — без SQL и без рестарта api.
- **Отсутствие поля не стирает значение**: upsert делает `bench_url = COALESCE(EXCLUDED.bench_url, nodes.bench_url)`, поэтому старая сборка ноды, нода без бенча или URL, выставленный руками в SQL (`deploy/seed-nodes-bench-url.sql`), сохраняются. Явное значение — перезаписывает.
- Невалидный непустой `bench_url` (нет схемы/хоста, `ftp://`, пробелы) — регистрация отклоняется с 400, чтобы опечатка не оставила ноду без бенча молча; пустая строка трактуется как «бенча нет».
- Не проксировать: URL ведёт на саму ноду, иначе замер покажет канал прокси.

### `GET /nodes?env={env}`

- **Auth:** service token
- **Query:** `env` — опциональный фильтр
- **Response:** `200 OK` с обёрткой `ResponseMessage<Option<Vec<NodeResponse>>>`

### `GET /node/{id}`

- **Auth:** service token
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
  { "days": 30, "distributor": "FRKN", "product": "standard-gaming" }
  ```
  - `product` (опционально) — тариф: `standard`, `gaming`, `bs-prem`,
    `standard-gaming`, `standard-bs-prem`, `full`. Отсутствует = `standard`
    (классический доступ из `enabled_conns`, включая `wl`) — так старые
    подписки не меняются.
  - Что даёт тариф: `standard` — классический набор; `gaming` / `bs-prem` —
    только этот env; `standard-gaming` / `standard-bs-prem` — классический
    набор + env; `full` — классический набор + `gaming` + `bs-prem`
    (включая `wl`, он часть классического набора).
  - `product` кроме `standard` — только для безлимитных (`standard`) ключей:
    в паре с `kind: "lite"` возвращается `400`.
  - Строка самого кода активации не меняется: `product` хранится отдельно.
- **Response:** `200 OK` с обёрткой `Instance::Key` (поле `product` в ответе)

### `POST /key/activate`

Активировать ключ для подписки.

- **Auth:** нет
- **Body:**
  ```json
  { "code": "XXXXX-XXXXX-...", "subscription_id": "uuid" }
  ```
- **Response:** `200 OK` с обёрткой `Instance::Key`; `400` если ключ уже активирован; `404` если ключ/подписка не найдены; `409` если `product` ключа не совпадает с `product` подписки.

Продление возможно только ключом того же тарифа: подписка сохраняет свой
`product` навсегда. Ключ другого тарифа активируется **без** `subscription_id`
— так создаётся отдельная подписка. Проверка выполняется до изменения срока
подписки, списания ключа и создания подключений. Если для env тарифа не задан
протокол (`env_proto_templates`), активация отвечает `500`, ключ не расходуется.

**Второй тариф — грант на той же подписке, а не вторая подписка.** Ключ
кладётся на уже существующую подписку, если её можно опознать: в теле передан
`subscription_id`, либо сам ключ выпущен под подписку (`keys.subscription_id`,
так платёжка привязывает ключ к известному покупателю). В обоих случаях план
ключа добавляется как **ещё один грант** этой подписки (`subscription_access`
по одному на план, со своим сроком и своей квотой), а новая подписка не
создаётся. Если опознать пользователя нечем — поведение прежнее, создаётся
новая подписка. В ответе `POST /v1/activate` поле `merged: true` говорит
клиенту, что план добавлен к существующей подписке.

Ключ `lite` («Super Duper») сажается только на шард плана `lite`: если под
этот план не объявлен ни один шард — `503 plan_not_configured`, если объявлен,
но без живой ноды/адресов — `503 no_live_shard`. Ключ при этом не расходуется,
и на стандартный шард такой пользователь не попадает. Ключ `lite` на
подписке, у которой грант этого плана уже есть, — это **топ-ап**: купленный
объём прибавляется к квоте, а не заменяет её.

---

## Гео-листы роутера и нод

Публичные plain-text ручки, из которых потребители берут списки IPv4.
**Отдаёт их nginx с хоста api, не бинарь api** (njs-обработчик
`/etc/nginx/frkn-geo.js`, файлы из `/var/lib/frkn/geo`); в api маршрута
`/router/geo/*` больше нет — прямой запрос в api отвечает `405`, как на любой
неизвестный GET-путь (`/foobar`), и зону не отдаёт:
`ru.zone` — свежий список российских подсетей (ipdeny `ru-aggregated.zone`,
раз в сутки), `ai4.zone` — IP-адреса AI-сервисов, каждый проверен TLS/SNI перед
публикацией (раз в час), `aicn4.zone` — то же для китайских AI, но это
**direct whitelist**: ноды не маркируют такие адреса, и они выходят с IP самой
ноды, `exch4.zone` — то же для китайских криптобирж (ноды
маркируют их в RU-выход), `adult4.zone` — то же для взрослых платформ и их
CDN (второй **direct whitelist**: ноды не маркируют такие адреса, и они всегда
выходят с IP NL-входа, никогда через RU/US/KZ), `gov4.zone` — проверенные адреса
российских государственных сайтов (свой одноцелевой выход, `mark 0x7` → таблица
700 → msk-tw, fail-closed). Подробности —
[`docs/router-geo.md`](router-geo.md).
Отдаются только зоны из whitelist в nginx-сниппете
`/etc/nginx/snippets/frkn-geo.conf` (`ru.zone`, `ai4.zone`, `aicn4.zone`,
`exch4.zone`, `youtube4.zone`, `gov4.zone`, `adult4.zone`); отдачу делает
nginx, не api — **новой зоне релиз api не нужен**, см.
[`docs/router-geo.md`](router-geo.md) § «Зоны отдаёт nginx». Неизвестное имя —
`404`, файл по нему не читается.

### `GET /router/geo/ru.zone` (+ `HEAD`)

- **Auth:** нет
- **Ответ:** `200 OK`, `text/plain; charset=utf-8`, один CIDR на строку
- **Заголовки:** `ETag: "sha256:<hex>"` (сильный, от содержимого файла),
  `Last-Modified`, `Cache-Control: public, max-age=3600`
- **`If-None-Match`** с текущим ETag → `304 Not Modified` без тела
- `404` — файл ещё не опубликован, `503` — файл не прошёл валидацию (обрезанный
  список не отдаётся никогда)
- Файл: `/var/lib/frkn/geo/ru.zone` (константа `GEO_DIR` в
  `/etc/nginx/frkn-geo.js`)

### `GET /router/geo/ru.zone.sha256` (+ `HEAD`)

- **Ответ:** `200 OK`, `text/plain; charset=utf-8`, тело `<sha256>  ru.zone\n`
  (формат `sha256sum`), те же кэш-заголовки и `If-None-Match` → `304`

### `GET /router/geo/<zone>.count` (+ `HEAD`)

- **Ответ:** `200 OK`, `text/plain; charset=utf-8`, тело — число CIDR в
  валидированной генерации, одной строкой (`180\n`)
- **Заголовки/семантика:** тот же сильный `ETag` (`sha256:…`) и `Last-Modified`,
  что у самой зоны, `304` по `If-None-Match`, `503` если файл не прошёл
  проверку формы/порога (счётчик невалидного списка не отдаётся)
- **Зачем:** однострочная сверка «на ноде применено столько же, сколько
  опубликовано» без скачивания списка:
  `nft list set inet frkn_geo exch4 | wc -l` против
  `curl -s .../exch4.zone.count`. Ноды считают элементы сами, эндпойнт —
  для оператора и мониторинга

### `GET /router/geo/ai4.zone` и `/router/geo/ai4.zone.sha256`

Точно те же заголовки, коды и `304`-семантика, что у `ru.zone`, только список —
проверенные `/32` AI-сервисов (Anthropic, OpenAI, Google Gemini, xAI, Mistral,
Perplexity, Copilot; источник — `domains/ai.txt` в `env`). Порог валидации у
зоны свой (8 строк): размер генерации защищает публикатор (порог 60 % от
предыдущей + TLS-проверка каждого IP), а не отдача.

### `GET /router/geo/aicn4.zone` и `/router/geo/aicn4.zone.sha256`

Тот же контракт (заголовки, `ETag`, `304`, `HEAD`, свой порог 8 строк), список —
проверенные `/32` китайских AI (DeepSeek, Qwen, Moonshot/Kimi, Zhipu/GLM,
MiniMax, Volcengine/Doubao; источник — `domains/ai-cn.txt` в `env`).
Использование обратное `ai4`: ноды ставят `ip daddr @aicn4 return` **выше всех
марок**, поэтому трафик выходит с IP самой ноды и никогда не уходит в exit-группу
(`router-geo.md`, «Маршрутизация: группы `ai` и `ai-cn`»).

### `GET /router/geo/adult4.zone` и `/router/geo/adult4.zone.sha256`

Тот же контракт (заголовки, `ETag`, `304`, `HEAD`, свой порог 8 строк), список —
проверенные `/32` взрослых платформ (Pornhub, XVideos, XNXX, xHamster, Redtube,
Youporn, Spankbang, …) и их image/media CDN; источник — `domains/adult.txt` в
`env`. Использование — как у `aicn4`: ноды ставят `ip daddr @adult4 return`
**вторым, сразу после `@aicn4` и выше всех марок**, поэтому такие адреса всегда
выходят с IP NL-входа ноды и никогда через RU/US/KZ (`router-geo.md`,
«Маршрутизация: группа `adult`»).

### `GET /router/geo/exch4.zone` и `/router/geo/exch4.zone.sha256`

Тот же контракт (заголовки, `ETag`, `304`, `HEAD`, свой порог 8 строк), список —
проверенные `/32` китайских криптобирж (Binance, OKX, Bybit, HTX, Gate, KuCoin,
MEXC, Bitget, …; источник — `domains/exchanges.txt` в `env`). Использование
другое: ноды ставят `ip daddr @exch4 meta mark set 0x2` и пускают трафик в
RU-выход (`router-geo.md`, «Маршрутизация: группа `exch`»).

### `GET /router/geo/gov4.zone` и `/router/geo/gov4.zone.sha256`

Тот же контракт (заголовки, `ETag`, `304`, `HEAD`, `.count`, свой порог 8 строк),
список — проверенные `/32` российских государственных сайтов (Госуслуги, ЕСИА,
Почта, `mos.ru`, ФНС, СФР, суды, министерства; источник — `domains/gov.txt` в
`env`). Использование — единственная группа с **одним** выходом: ноды ставят
`ip daddr @gov4 meta mark set 0x7 return` **выше всех остальных групп**
(таблица 700 → `msk-tw` `217.149.29.118`), потому что общие RU-диапазоны
WAF-зафлагены для гос-сайтов. Выход fail-closed: упал — трафик блокируется, а не
утекает через NL или RU-группу (`router-geo.md`, «группа `gov`»).

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

Поля пула и бенча в каждом `connections[]` (0.6.134+):

```json
{
  "connection_uuid": "1ac65b82-…",
  "node_id": "7f3c9a01-…",
  "endpoints": [{ "ip": "177.177.20.172", "port": 443, "weight": 100 }],
  "node_ips": ["177.177.20.172"],
  "node_ports": { "177.177.20.172": 443 },
  "node_weights": { "177.177.20.172": 100 },
  "pool": "EU Рилзопровод",
  "speedtest_url": "https://mis-at-vie-bench-01.bench.beacon.frkn.org:19443"
}
```

- `endpoints[]` — **единая форма точки входа** (`ip`, `port`, `weight`,
  опционально `label`): то же, что `[[node.endpoints]]` в конфиге ноды. Новый
  клиент читает только её; `node_ips` + `node_ports` + `node_weights` остаются
  на проводе для старых сборок и всегда описывают тот же набор адресов —
  `port` резолвится (порт инбаунда, если нода свой не задала), `weight` по
  умолчанию 100. То же поле есть в `/v1/config` и в карточках `/v1/locations`
  (`nodes[].endpoints`, по записи на строку — рядом с легаси `endpoint`).

- `speedtest_url` — base URL бенча **этой ноды** (`https://host:port`, без пути и слэша), тот же, что в `/status`. Один на ноду: не зависит от адреса и протокола.
- **Отсутствует** (не `null`), если у ноды бенча нет — премиум-слоты, легаси-ноды. Клиент показывает «нет тест-файла».
- Выводить адрес из hostname нельзя: имена в зоне `beacon.frkn.org` — `<hoster>-<cc>-<city>-bench-<nn>.bench…`.

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
    "os_version": "ios",
    "app_version": "...",
    "app_language": "ru",
    "installation_uuid": "uuid",
    "user_country_code": "RU",
    "server_country_code": "SWE",
    "service_type": "amnezia-free",
    "service_protocol": "awg",
    "auth_data": {
      "id": "subscription-uuid"
    },
    "publicKey": "TTUZIyQdgxf9Yx4MiwzX6oICsh6ajjgxlWC8sVBMC8I=",
    "connection_id": "connection-uuid"
  }
  ```
  - `publicKey` для `awg` можно не передавать / игнорировать: сервер сам берёт ключи из структуры connection.
  - `connection_id` опционален. Если передан — выбирается именно этот connection, иначе первый подходящий по стране и протоколу.

- **Response:** `200 OK` → `GatewayConfigResponse` (зашифрованный envelope)
  ```json
  {
    "config": "<base64(gzip(inner_json))>",
    "config_version": 2,
    "supported_protocols": ["vless", "awg"],
    "api_config": {
      "server_country_code": "SWE",
      "service_protocol": "awg",
      "service_type": "amnezia-free",
      "speedtest_url": "https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443",
      "user_country_code": "RU"
    },
    "service_info": {
      "name": "SWE",
      "type": "amnezia-free"
    }
  }
  ```

  Для `awg` поле `config` — это **gzip-сжатый** JSON, закодированный в base64 (стандартный алфавит с паддингом). После декодирования получается:

  ```json
  {
    "config_version": 2,
    "defaultContainer": "amnezia-awg",
    "description": "FRKN AWG",
    "dns1": "1.1.1.1",
    "dns2": "1.0.0.1",
    "hostName": "91.186.218.62",
    "name": "FRKN",
    "containers": [
      {
        "container": "amnezia-awg",
        "amnezia-awg": {
          "config": "[Interface]\nPrivateKey = Axhy7oMt1BfsuXw9qD3Nbkx5pVD/Yu/RPGzwqsjMlaY=\nAddress = 100.64.0.4/32\nMTU = 1280\nDNS = 1.1.1.1\nJc = 4\nJmin = 56\nJmax = 134\nS1 = 70\nS2 = 55\nH1 = 100000-200000\nH2 = 300000-400000\nH3 = 500000-600000\nH4 = 700000-800000\nS3 = 11\nS4 = 12\nI1 = <r 128>\n[Peer]\nPublicKey = hf0ZY6WULIRaBbeVtR8ox3y7LLEqocPB2DaSif2PTw0=\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = 91.186.218.62:51820\nPersistentKeepalive = 25",
          "isThirdPartyConfig": true,
          "last_config": "{\"client_priv_key\":\"Axhy7oMt1BfsuXw9qD3Nbkx5pVD/Yu/RPGzwqsjMlaY=\",\"client_pub_key\":\"B6m2PYhhec2Pe+I50I63pdFB/bR6Psd77MhpWj4TlQE=\",\"client_ip\":\"100.64.0.4/32\",\"mtu\":\"1280\",\"server_pub_key\":\"hf0ZY6WULIRaBbeVtR8ox3y7LLEqocPB2DaSif2PTw0=\",\"psk_key\":\"\",\"port\":51820,\"hostName\":\"91.186.218.62\",\"persistent_keepalive\":\"25\",\"junkPacketCount\":\"4\",\"junkPacketMinSize\":\"56\",\"junkPacketMaxSize\":\"134\",\"initPacketJunkSize\":\"70\",\"responsePacketJunkSize\":\"55\",\"cookieReplyPacketJunkSize\":\"11\",\"transportPacketJunkSize\":\"12\",\"initPacketMagicHeader\":\"100000-200000\",\"responsePacketMagicHeader\":\"300000-400000\",\"underloadPacketMagicHeader\":\"500000-600000\",\"transportPacketMagicHeader\":\"700000-800000\",\"specialJunk1\":\"<r 128>\"}"
        }
      }
    ]
  }
  ```

  Важные моменты для AWG:
  - `config` содержит готовую INI-строку WireGuard/AmneziaWG с **реальным** клиентским приватным ключом.
  - `last_config` — JSON-строка с теми же ключами и параметрами обфускации.
  - `client_priv_key` и `client_pub_key` — это ключи из connection, сгенерированные сервером. Публичный ключ пира на сервере создаётся именно для этой пары.

---

## Алерты

### `POST /v1/admin/alerts/exit`

Событие health **группы выходов** от watchdog'а на ноде (`ru`, `ai`, `gov`, …).
Нода видит туннели и exit IP, api владеет почтовым каналом и дедупликацией —
поэтому SMTP-креденшелов на нодах нет, у ноды только токен вызова.

- **Auth:** сервисный или админский токен (`Authorization: Bearer <service token>`),
  фильтр `with_service_or_admin_auth`; `node_`-токен здесь не принимается.
- **Body:**
  ```json
  {
    "group": "ru",
    "node": "sh-0001-nl02",
    "table": 200,
    "event": "exit_dead",
    "duration_sec": 42,
    "detail": "both exits of group ru are dead; table 200 holds only the unreachable guard 1000",
    "active_iface": "",
    "probes": [
      { "iface": "wg-ru-ufo", "role": "primary", "expected_ip": "171.22.134.156",
        "observed_ip": null, "alive": false, "handshake_age_sec": 999999 },
      { "iface": "wg-ru-aeza", "role": "secondary", "expected_ip": "171.22.130.17",
        "observed_ip": null, "alive": false, "handshake_age_sec": 999999 }
    ]
  }
  ```
  `event` — `exit_dead` (все выходы мертвы, трафик группы заблокирован),
  `exit_degraded` (primary мёртв, везёт secondary), `exit_recovered`.
- **Дедупликация** — по паре (группа, нода), в процессе api; после рестарта
  восстанавливается из `alert_events`. Переход состояния → письмо сразу; повтор
  того же состояния → не чаще `[alerts] exit_reminder_sec` (1800 с). Поэтому нода
  может присылать текущее состояние на каждом тике (10 с).
- **Response:** `200` `{"delivered":true,"action":"sent","email":true,"telegram":null,"reminder":false}`.
  `action:"suppressed"` + `reason` (`repeat` | `nothing-to-recover`) — письма не было
  и повторять не нужно. `502` — доставка не удалась, событие надо повторить.
- **Запись:** отправленное событие попадает в `alert_events` (`rule=exit_*`,
  `env=<группа>`, `hostname=<нода>`, `details->>'group'`, `details->'probes'`).

Тема письма: `[FRKN] ⛔ <нода> — группа <g>: выход мёртв`.

## Сводка (digest)

### `GET /v1/admin/digest?kind=daily|weekly|monthly&from=<rfc3339>&to=<rfc3339>`

Данные системной сводки, из которых mrkting собирает письмо владельцу. Раньше
api сам собирал, рендерил и отправлял сводку; теперь у него только данные, а
расписание, шаблон и SMTP живут в mrkting (`[digest]` в его конфиге).

- **Auth:** admin token (`Authorization: Bearer <admin_token>`)
- **Query:** `kind` — влияет на форму тренда (7 дней / 8 недель / 12 месяцев);
  `from`/`to` — готовое окно в UTC, его считает отправитель по своим локальным
  суткам. Без окна берётся прошлый период от `kind`.
- **Response:** `200 OK`
  ```json
  {
    "status": 200,
    "window": { "from": "2026-11-14T21:00:00Z", "to": "2026-11-15T21:00:00Z" },
    "digest": {
      "kind": "daily",
      "nodes_total": 64, "nodes_online": 55,
      "by_env": [["dev", 10, 9], ["премиум", 20, 18]],
      "alerts_total": 3, "alerts_by_rule": [["node_offline", 2]],
      "alert_hosts": [["mis-at-vie-core-01.beacon.frkn.org", 2]],
      "downtime_minutes": 12,
      "connects": 3422, "connecting_subs": 890,
      "by_protocol": [["AmneziaWg", 2100]],
      "by_country": [["RU", 1500]],
      "active_now": 1071, "active_at_start": 1062,
      "created": 9, "expired": 9, "expiring_7d": 31,
      "by_product": [["standard", 900]],
      "trend": [["08.11", 1040], ["09.11", 1052]]
    }
  }
  ```
- Деньги (покупки, выручка) в этом ответе **не** отдаются: их mrkting берёт
  прямо из payment-gw `/analytics/sales`.

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

## Гео-листы роутера

```bash
# Заголовки (ETag, Last-Modified, Content-Length) без скачивания файла
curl -sI https://api.frkn.org/router/geo/ru.zone

# Контрольная сумма в формате sha256sum
curl -s https://api.frkn.org/router/geo/ru.zone.sha256

# Условный GET: 304, если список не менялся
curl -s -o /dev/null -w '%{http_code}\n' \
  -H 'If-None-Match: "sha256:<hex>"' \
  https://api.frkn.org/router/geo/ru.zone
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
