# Контракт v2-карточки: peer vs client_config

Статус: черновик на согласование. Один экран, без реализации.

## 1. Задача

Сейчас карточка описывает только пир-протоколы (AWG/WG): клиент берёт `nodes[]`, зовёт
`POST /v1/peers` и получает `.conf`. Hysteria2 и VLESS пиров не выдают — им нужен клиентский
конфиг. Нужно, чтобы пул в списке показывал все протоколы пилюлями, не меняя форму карточки
для уже работающих AWG/WG.

## 2. Два вида карточки

`PoolCard` получает поле `kind`:

| `kind` | Кто | Что делает клиент | Что в `nodes[]` |
|---|---|---|---|
| `peer` (по умолчанию, поле может отсутствовать) | `amnezia_peer`, `wireguard_peer` | `POST /v1/peers` с `card_id` + `public_key` → INI | `endpoint`, `server_public_key`, `mtu`, `dns`, `keepalive`, `obfuscation` |
| `client_config` | `hysteria2`, `vless_*` | `POST /v1/peers` **не зовёт**; собирает конфиг из `payload` | `endpoint`, `country`, `payload` |

Правила совместимости:
- `card_id` не меняется: `<shard>:<pool>:<tag>`; группировка в пилюли уже идёт по `<shard>:<pool>`,
  поэтому hysteria2-карточка пула автоматически становится третьей пилюлей в той же строке.
- Отсутствие `kind` = `peer`. Старый клиент на `client_config`-карточке просто её не покажет
  (фильтр по известным протоколам), ничего не сломав.
- Версия контракта едет в ответе `/v1/locations` (`contract: 2`), чтобы клиент мог отличить.

## 3. Payload hysteria2 (пример)

```json
{
  "card_id": "0001:prem:hysteria2",
  "pool": "prem",
  "proto": "hysteria2",
  "kind": "client_config",
  "nodes": [
    {
      "endpoint": "prem-0001.beacon.frkn.org:8443",
      "country": "DE",
      "payload": {
        "server": "prem-0001.beacon.frkn.org",
        "port": 8443,
        "sni": "prem-0001.beacon.frkn.org",
        "auth": "<uuid подписки>",
        "insecure": false,
        "obfs": { "type": "salamander", "password": "<psk пула>" }
      }
    }
  ]
}
```

`auth` — **тот же uuid, который грант уже выдаёт v1-плоскости** (`grants_to_conns_map`,
`src/bin/api/access.rs:33-54`); нода валидирует его через сервис `auth`
(`src/bin/auth/handlers.rs:9-30`). Ни нового пароля, ни новой таблицы: карточка — второй способ
отдать существующий креденшел.

## 4. Payload VLESS (пример)

```json
{
  "kind": "client_config",
  "nodes": [
    {
      "endpoint": "1.2.3.4:443",
      "country": "NL",
      "payload": {
        "server": "1.2.3.4",
        "port": 443,
        "uuid": "<uuid подписки>",
        "flow": "",
        "transport": { "type": "xhttp", "path": "/x", "host": "cdn.example" },
        "security": {
          "type": "reality",
          "sni": "www.microsoft.com",
          "public_key": "<reality public key>",
          "short_id": "ab12",
          "fingerprint": "chrome"
        }
      }
    }
  ]
}
```

Reality-ключи — **общие на пул** (как `server_public_key` у WG): это снимает `409
pool_inbounds_conflict`, который сравнивает целые `stream_settings`, и делает мульти-нодовый
VLESS-пул возможным. Цена — ключ лежит на всех нодах пула.

## 4.1 Реализовано: `vless_xhttp_cdn` (CDN, plain TLS)

Карточка строится только из **объявленного** инбаунда ноды + существующего uuid подписки
(`Proto::Xray(VlessXhttpCdn)`, id коннекта) — см. `vless_xhttp_cdn_payload` в
`src/bin/api/http/handlers/peers.rs`. Нода должна объявить в `xraySettings.inbounds[]`:

```json
"streamSettings": {
  "network": "xhttp",
  "xhttpSettings": {
    "path": "/downloads",
    "mode": "packet-up",
    "extra": { "padding": { "header": "X-Cache", "key": "_dc", "bytes": "100-1000" } }
  },
  "tlsSettings": { "serverName": "bs.frkn.org", "alpn": ["h2"], "certificates": [] }
}
```

- `path` и `tlsSettings.serverName` обязательны: без них карточка не выдаётся (в лог — warn).
- `host` и `sni` в payload = `serverName`; порт — `inbound.port`; `endpoint` — IP ноды.
- `mode`, `alpn`, `padding` необязательны: не объявлены — полей в payload нет (клиент берёт свои
  дефолты). `padding` проходит как есть, только если есть все три поля.
- `uuid` = id живого VLESS-коннекта подписки в этом env; коннекта нет — карточки нет.

## 5. Что это меняет в api

1. `PoolCard`/`PoolNode` (`src/bin/api/http/handlers/peers.rs:222-251`): `+ kind`, `+ payload`
   (`Option<serde_json::Value>`, `skip_serializing_if`).
2. Список протоколов карточек перестаёт быть константой `[Amnezia, Wireguard]`
   (`handlers/peers.rs:597`) — берётся из объявленных инбаундов ноды, пересечённых с грантом.
3. `POST /v1/peers` для `client_config` возвращает `400 card_kind_not_peer` (явная ошибка вместо
   «нет адреса» на стороне клиента).
4. Правки `access.rs` запрещены в рамках M1: он кормит v1-конфиги ~10k премиум-пользователей h2
   и ~25k VLESS, любое изменение там меняет их молча.

## 6. Что это меняет в клиенте

1. `peersImportController` — ветка по `kind`: `peer` как сейчас, `client_config` собирает конфиг из
   `payload` и не зовёт `/v1/peers`.
2. `peerConfigBuilder` перестаёт быть только-WG генератором (`peerConfigBuilder.cpp:100-136`):
   для hysteria2 отдаёт YAML амнезия-xray (транспорт уже есть), для VLESS — свой JSON.
3. `containerForProto` (`:187-194`) не должен отправлять не-WG в `DockerContainer::Awg`.
4. Пилюли/`protocolRank`/`protocolLabel` — без изменений, они уже протокол-агностичны.

## 7. Порядок внедрения

M0 (этот документ) → M1 hysteria2 (`client_config`, общий uuid через `auth`) → M3 VLESS
(общий Reality-keypair). Миграции не нужны ни на одном шаге: v1-конфиги продолжают работать,
v2-карточка — дополнительный путь к тому же креденшелу.
