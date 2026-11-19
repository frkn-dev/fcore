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
| лимиты 5/50 | `device_limit`, `subscription_limit` | 409 |
| ключ уже занят в env | `key_taken` | 409 |
| нет свободного адреса | `no_free_address` | 503 |
| пиры выключены | `peers_disabled` | 404 |

## Идемпотентность

`POST /v1/peers` с тем же `(env, public_key)` возвращает существующий пир — повтор после
таймаута безопасен.

## Шарды: имя env на проводе

`Env` в api знает фиксированный список (`dev`, `ru`, `wl`, `gaming`, `bs`, `bs-prem`,
`experimental`, `production`); всё остальное — `Env::Custom`, который печатается с префиксом.
Поэтому шард `standard-01` приходит в JSON **`customstandard-01`** и точно так же лежит в
`env_networks`, `env_capacity` и `subscription_access`. В `access[].env` и в `peer.env` клиент
видит именно это имя; в `POST /v1/peers` его надо вернуть дословно. Пользователю имя не
показывается — карточка рисуется по `pool` + `pool_translation`.
