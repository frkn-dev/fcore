# Конверт для клиентских ручек пиров (требование приложения)

Статус: **сделано в 0.7.2** — шесть клиентских ручек переведены на POST + конверт, plain-варианты
удалены, `POST /v1/activate` отдаёт `response.error`. Источник истины по контракту — `api-peers.md`;
здесь — правка транспорта, без которой приложение не могло ходить в пиры.

## 1. Что не так

Ручки пиров объявлены на «голом» warp, а весь остальной клиентский api — через AGW-конверт
(RSA-PKCS#1 ключ + AES-256-CBC тело, `src/bin/api/http/crypto.rs`).

Места (ветка `feature/peers`):

| ручка | где | чем принимает |
|---|---|---|
| `POST /v1/peers` | `routes.rs:1719` | `warp::body::json::<PeerCreateRequest>()` |
| `GET /v1/peers` | `routes.rs:1748` | `warp::query::<HashMap<String,String>>()` |
| `DELETE /v1/peers/{id}` | `routes.rs:1784` | `warp::body::json()` + `path::param` |
| `GET /v1/devices` | `routes.rs:1806` | `warp::query` |
| `PATCH /v1/devices/{id}` | `routes.rs:1831` | `body::json()` + `path::param` |
| `DELETE /v1/devices/{id}` | `routes.rs:1853` | `body::json()` + `path::param` |
| `GET /v1/locations` | `routes.rs:1875` | `warp::query` |

Для сравнения, как сделано в рабочем клиентском пути: `POST /v1/services` (`routes.rs:1026`)
— `crypto::with_agw_decryption::<GatewayServicesRequest>(agw_key)` + `crypto::encrypt_gateway_reply`.

Почему это блокер, а не вкусовщина:

1. **Приложение умеет только конверт.** Все клиентские запросы идут через `GatewayController`
   (`client/core/controllers/gatewayController.{h,cpp}`) — он умеет исключительно POST с
   `{keyPayload, apiPayload}` и расшифровкой ответа тем же AES-ключом.
   Точка входа: `client/ui/controllers/api/apiConfigsController.cpp:3305`.
2. **Второй HTTP-стек — это не «ещё один QNetworkAccessManager».** В `GatewayController`
   зашито: таймауты (`apiDefs::requestTimeoutMsecs`), ретрай на фолбэк-хост
   (`gatewayController.cpp:188-204`), прокси-байпас для РФ (`bypassProxy`, URL-ы из S3) и
   обход kill-switch на десктопе (`prepareRequest`, `gatewayController.cpp:88-101`).
   Голый GET/POST этих путей не получает, и пиры начнут отваливаться там, где конфиги
   работают.
3. **`subscription_id` — секрет подписки.** В query он оказывается в access-логах nginx,
   в crash-репортах, в истории прокси и в аналитике. В конверте он живёт только внутри
   шифрованного тела. Это ровно та же причина, по которой `/v1/services` и `/v1/config`
   когда-то ушли в конверт.
4. **GET/DELETE/PATCH зашифровать нельзя.** Конверт — это POST с JSON-телом: AES-блок
   кладётся в `apiPayload`, AES-ключ — в `keyPayload`. Значит «оставить GET с конвертом»
   невозможно, только POST.

Отдельно: комментарий `crypto.rs:20-21` («если тело — plain JSON, возвращаем как есть»)
не соответствует коду — `decrypt_request` в этом случае возвращает `keyPayload missing`.
На dual-mode (plain + конверт в одном роуте) не рассчитывать.

## 2. Что сделать

Все шесть ручек переводим на POST + конверт. Тела запросов собираются из того, что раньше
было в query/path. id из пути переезжает в тело (path остаётся, только чтобы клиенту было
проще роутить: `v1/peers/list`, `v1/peers/delete` и т.д.).

| было | стало | тело запроса |
|---|---|---|
| `POST /v1/peers` | `POST /v1/peers` | `PeerCreateRequest` (без изменений) |
| `GET /v1/peers?subscription_id=&device_uid=` | `POST /v1/peers/list` | `{subscription_id, device_uid?}` |
| `DELETE /v1/peers/{id}` + `{subscription_id}` | `POST /v1/peers/delete` | `{subscription_id, peer_id}` |
| `GET /v1/locations?subscription_id=` | `POST /v1/locations` | `{subscription_id}` |
| `GET /v1/devices?subscription_id=` | `POST /v1/devices/list` | `{subscription_id}` |
| `PATCH /v1/devices/{id}` | `POST /v1/devices/update` | `{subscription_id, device_id, device_name}` |
| `DELETE /v1/devices/{id}` | `POST /v1/devices/delete` | `{subscription_id, device_id}` |

`POST /v1/activate` **остаётся plain** — приложение уже ходит plain на `/key/validate` и
`/key/activate` (`client/ui/controllers/keyActivationController.cpp:14`), а тело там POST и в
логи не попадает. Менять не нужно; нужен только внятный `error` в ответе (см. §3).

Не трогаем: `GET/POST/DELETE /v1/access`, `GET /v1/admin/support/{tag}` (Bearer, сервер-сервер),
`/v1/nodes/load`, `/v1/report/*` — у них свои схемы.

### Образец (locations)

```rust
#[derive(Clone, Debug, Deserialize)]
pub struct LocationsRequest {
    pub subscription_id: Uuid,
}

let post_locations_route = warp::post()
    .and(warp::path!("v1" / "locations"))
    .and(warp::path::end())
    .and(with_param_bool(peers_enabled))
    .and(crypto::with_agw_decryption::<peers_handlers::LocationsRequest>(
        agw_key.clone(),
    ))
    .and(with_sync(self.sync.clone()))
    .and_then(
        |enabled: bool,
         req: peers_handlers::LocationsRequest,
         ctx: Option<AesContext>,
         sync: MemSync<N, C, S>| async move {
            if !enabled {
                return crypto::encrypt_gateway_reply(
                    peers_handlers::not_enabled().into_response(),
                    ctx,
                )
                .await;
            }
            let reply = peers_handlers::get_locations_handler(req.subscription_id, sync).await;
            crypto::encrypt_gateway_reply(reply.into_response(), ctx).await
        },
    );
```

Три вещи, на которых это ломается:

* `with_agw_decryption` отдаёт **два** значения — `(T, Option<AesContext>)`. Порядок аргументов
  в замыкании должен совпадать с порядком `.and(...)`, иначе `warp` не соберётся или молча
  подставит чужой параметр.
* Хендлеры пиров возвращают `Box<dyn warp::Reply>`, а `encrypt_gateway_reply` принимает
  `warp::reply::Response` → нужен `reply.into_response()` (см. образец; `use warp::Reply;`).
* `warp::query::<HashMap<..>>()` и `warp::path::param::<Uuid>()` из цепочки убрать, иначе
  останется требование query-параметра и ручка будет отбивать зашифрованный запрос.

## 3. Контракт ошибок (не терять при заворачивании)

`encrypt_gateway_reply` сохраняет `parts.status` (`crypto.rs:164-166`) — HTTP-код доходит до
клиента как есть. Клиент мапит поведение по HTTP-статусу **и** по `response.error` из
`refusal_reply` (`handlers/peers.rs:65-81`, коды — `api/peers/mod.rs:154-173`).

Требования:

1. HTTP-статус ответа = `Refusal::status()`: 400/402/403/404/409/503 (`api/peers/mod.rs:139-152`).
   Не заворачивать в 200 с полем `status` в теле — приложение читает реальный код ответа.
2. `response.error` — стабильная строка из `Refusal::code()`: `quota_exhausted`,
   `grant_expired`, `no_grant`, `proto_not_granted`, `env_not_served`, `peers_limit`,
   `subscription_limit`, `key_taken`, `label_taken`, `no_free_address`, `device_not_found`,
   `peer_not_found`. Приложение различает 402 «кончился трафик» и 402 «истёк срок» только по
   ней — по статусу они одинаковые. `peers_limit` — лимит туннелей одного устройства (по
   X25519-ключу, по всем подпискам), `devices_limit` — лимит числа устройств подписки.
3. Ошибка конверта (`keyPayload missing`, битый AES) остаётся 400 — приложение трактует это
   как «апи не тот», а не как сетевую ошибку.
4. `/v1/activate` при 503 обязан отдавать `error: "no_live_shard"` / `"plan_not_configured"`
   (`handlers/key.rs:371,464`) — по этому коду приложение показывает «нет серверов, ключ не
   потрачен».

## 4. Совместимость

Клиентов на ручках пиров сейчас **ноль** — в dopamine нет ни одного вызова
`/v1/peers|locations|devices` (проверено grep по `client/`). Значит можно заменять роуты,
а не держать две версии; plain-варианты удалить, чтобы не оставить дыру в логах.

`docs/api-peers.md` и `docs/app-peers-tz.md` после правки должны описывать POST, а не GET —
иначе следующая сессия сделает клиент по старому контракту.

## 5. Что приложение сделает со своей стороны

- `GatewayController` получит вариант, возвращающий `{ httpStatus, decryptedBody }`:
  текущий `post()` на любой ошибке теряет и статус, и тело — `gatewayController.cpp:260-266`
  возвращает только `ErrorCode`, а `responseBody` заполняется лишь на успехе. Поведение
  существующих вызовов не меняется, добавляется отдельный метод.
- Новый `PeersController` ходит только через конверт: `POST /v1/locations`, `POST /v1/peers`,
  `POST /v1/peers/list`, `POST /v1/peers/delete`, `POST /v1/devices/{list,update,delete}`.
- UI-тексты ошибок — по `response.error` из §3 (402 `quota_exhausted` и 402 `grant_expired`
  показываются по-разному).

## 6. Проверка после правки

```bash
# 1. plain больше не принимается — ожидаем 400 с текстом про keyPayload, а не 405
curl -s -X POST -H 'Content-Type: application/json' -d '{}' \
  https://api.frkn.org/v1/locations

# 2. зашифрованный запрос проходит и расшифровывается
#    (тот же конверт, что у POST /v1/services: RSA-ключ agw_public.pem + AES-256-CBC)
```

Живой прогон клиентом: приложение делает `POST /v1/locations` через `GatewayController` и
получает `access[]` + `pools[]`; в логах api видно расшифрованное `subscription_id`, в логах
nginx — только base64-блоки.

## 7. Проверка на проде 2026-11-19 (api v0.7.2)

Проверено не по диффу, а живым запросом с тем же конвертом, что собирает клиент
(RSA PKCS#1v1.5 по `client/agw_public.pem` + AES-256-CBC, iv режется до 16 байт):

```
POST /v1/locations   {subscription_id}          -> 200 {"access":[],"pools":[]}
POST /v1/peers/list  {subscription_id,device_uid} -> 404 {"error":"device_not_found"}
POST /v1/devices/list{subscription_id}          -> 200 {"devices":[]}
POST /v1/peers       {…,"env":"dev"}            -> 404 {"error":"no_subscription"}
POST /v1/peers       {…,"env":"standard-ru"}    -> 400 {"error":"bad_env"}
GET  /v1/locations|peers|devices                 -> 405 (plain-вариантов больше нет)
POST /v1/locations   {} (plain)                  -> 400 {"error":"keyPayload missing"}
```

Выводы:

1. Конверт работает в обе стороны: ответ расшифровывается тем же AES-ключом, HTTP-статусы и
   `response.error` доходят как задумано (§3). Пиры на проде **включены**.
2. Plain-роуты закрыты — 405, дыры в логах нет.
3. **Инфра для запуска не готова.** `Env::from_str` знает только `dev`; `standard-eu`,
   `standard-ru`, `super-duper-eu`, `lite-eu` → `400 bad_env`, потому что строк в
   `env_networks` нет. Пока их нет: `/v1/locations` не отдаст ни одной карточки (пулы строятся
   из живых нод env), `POST /v1/peers` не создаст пир, и на телефоне тестировать нечего.
   Нужно: `env_networks` (env + подсеть + plan) → `env_capacity` → ноды с `awg_peers`/`wg_peers`
   в этом env → грант (`subscription_access` либо `/v1/activate`).

Коды, которых нет в §3 и которые клиент тоже должен пережить: `bad_env`, `unknown_proto`
(400), `peers_disabled` (404), `address_taken` (409, ретрай со следующего адреса),
`support_tag_taken` (409), `assign_failed` (503 при активации).
