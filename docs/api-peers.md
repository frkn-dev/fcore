# Peers API — контракт для клиента

Все клиентские ручки пиров говорят **только в AGW-конверте** и **только POST**.
Плейн-JSON тела нет вовсе: запрос без `keyPayload` отбивается `400 keyPayload missing` —
именно так приложение отличает «наш api» от чужого.

Причина: `subscription_id`, `peer_id`, `device_id` не попадают ни в query string, ни в путь,
значит их нет в access-логах, в истории браузера и в крэш-репортах.

## Конверт

Запрос:

```json
{
  "keyPayload": "<base64(RSA-PKCS#1v1.5({aes_key, aes_iv, aes_salt}))>",
  "apiPayload": "<base64(AES-256-CBC(JSON тела, IV = aes_iv[0..16], PKCS#7))>"
}
```

Ответ — **не** JSON, а сырые байты AES-256-CBC с тем же `aes_key` и `aes_iv[0..16]`.
Заголовки авторизации не нужны: подлинность обеспечивает RSA-ключ api.

Публичная часть ключа апи: `agw_public.pem` (в приложении уже используется для `/v1/config`).
Готовый образец — `dopamine/test_agw_encryption.py`.

## Клиентские ручки (конверт, POST)

| ручка | тело | ответ |
|---|---|---|
| `POST /v1/locations` | `{subscription_id}` | `{access: [...], pools: [...]}` |
| `POST /v1/peers` | `{subscription_id, device_uid, device_name?, env, proto, public_key?, label?}` | `{peer: PeerView, support_tag, pools: [...]}` |
| `POST /v1/peers/list` | `{subscription_id, device_uid?}` | `{peers: [PeerView]}` |
| `POST /v1/peers/delete` | `{subscription_id, peer_id}` | `{status: "ok"}` |
| `POST /v1/devices/list` | `{subscription_id}` | `{devices: [DeviceView]}` |
| `POST /v1/devices/update` | `{subscription_id, device_id, device_name?}` | `{device: DeviceView}` |
| `POST /v1/devices/delete` | `{subscription_id, device_id}` | `{status: "ok"}` |

`PeerView`:

```json
{ "peer_id": "…", "device_id": "…", "env": "standard-eu", "proto": "amnezia_peer",
  "address": "10.88.0.2/32", "label": null, "created_at": "…",
  "revoked_at": null, "revoke_reason": null }
```

`PeerCreateResponse` — то же плюс `support_tag` (строка вида `7K2M-9XQ4P`) и `pools[]`
с `endpoint` / `server_public_key` / `mtu` / `dns` / `keepalive` / `obfuscation`.

`DeviceView`: `{id, device_uid, device_name, support_tag, created_at, rotated_at}`.

## Панель управления (без конверта)

Эти ручки — для сайта и админки, авторизация `Authorization: Bearer <service token>`,
обычный JSON. Приложение их не вызывает.

| ручка | назначение |
|---|---|
| `GET /v1/access?subscription_id=…` | гранты подписки |
| `POST /v1/access` | выдать/продлить грант (`plan` или фиксированный `env`) |
| `DELETE /v1/access` | отозвать грант |
| `GET /v1/access/capacity?plan=…` | ёмкость шардов плана |
| `GET /v1/admin/support/{tag}` | подписка/устройство по `support_tag` |

## Активация

`POST /v1/activate` — плейн JSON, как и `/v1/key/activate`: это ручка сайта, у которого
нет AGW-клиента. Тело то же, что у старой активации.

## Ошибки

Тело ошибки уходит **внутри конверта**, как и успешный ответ:

```json
{ "error": "quota_exhausted" }
```

| код | `error` | HTTP |
|---|---|---|
| ключ не base64/не X25519 | `bad_public_key` | 400 |
| нет гранта на env / протокол | `no_grant`, `proto_not_granted` | 403 |
| env не обслуживается | `env_not_served` | 403 |
| срок вышел | `grant_expired` | 402 |
| квота | `quota_exhausted` | 402 |
| туннелей на устройство больше 3 | `device_limit` | 409 |
| туннелей на подписку больше 45 | `subscription_limit` | 409 |
| устройств на подписку больше 15 | `devices_limit` | 409 |
| ключ уже занят в env | `key_taken` | 409 |
| нет свободного адреса | `no_free_address` | 503 |
| пиры выключены | `peers_disabled` | 404 |

Ёмкость шарда считается **в адресах**, а не в подписках: лимит шарда = сумма блоков env из
`env_networks` минус резерв 10% (`no_free_address` приходит на 90% блока, а не на последнем
адресе). `/v1/capacity?plan=` отдаёт `used`/`limit` в адресах; `max_subscriptions` в
`env_capacity` — админский override, если нужно ограничить шард жёстче. Один `/24` = 253 пира,
из них раздаются 227.

## Идемпотентность

`POST /v1/peers` с тем же `(env, public_key)` возвращает существующий пир — повтор после
таймаута безопасен.

## Шарды: `env` — непрозрачный id

`env` — идентификатор шарда, а не описание: `0001`, `0002`, а не `standard-ru`. Имя несётся
**дословно** везде — в конфиге ноды, в `env_networks` / `env_capacity` / `subscription_access`,
в `access[].env`, `peer.env` и в топике `peers-update-<env>`. Никакого префикса `custom` больше
нет: раньше неизвестное имя превращалось в `Env::Custom` и в базу уезжало `customstandard-01` —
шард, которого не обслуживает ни одна нода.

План живёт в `env_capacity.plan`, локации — в `pools`, человеческое имя — в `env_capacity.title`.
Вид шарда — `env_capacity.kind`: `core` (наш регион, может быть пустым, пока ноды не заведены),
`premium` (слот аккаунта), `personal` (своя машина юзера). Вид **не выводится из имени**: шард
`premium-eu` не становится премиум-слотом сам по себе. Пустой `core`-шард имеет право минтить
коннекты v1, `premium`/`personal` — только вместе со своей нодой.
Ключ иммутабелен: переименование ломает гранты, пиры, подсеть и конфиги нод.

Понять, что такое `0001`, без чтения ключа:

```sql
SELECT * FROM shard_overview;
-- shard | title          | plan     | pools   | nodes | peers_live | address_hosts
-- 0001  | Standard NL/DE | standard | {de,nl} | 3     | 210        | 506
```

`GET /status` отдаёт у каждой ноды `env` и `env_title`:

```bash
curl -s https://api.frkn.org/status | jq '.response[] | {hostname, env, env_title, pool}'
```

В конфиге ноды `shard = "0001"` — алиас `env`, оба ключа равнозначны.
Пользователю имя шарда не показывается никогда: карточка рисуется по `pool_translation`,
заголовок группы — по `plan_title`.
