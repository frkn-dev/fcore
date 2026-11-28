# Peers: таблицы и примеры конфигов

Срез продакшена (`api`), миграции `0020`, `0021`. `0022` (kind = legacy, вьюхи по видам) написана, **не накатана**.

## Таблицы

### `env_capacity` — политика шарда (одна строка на шард)

| колонка | тип | null | смысл |
|---|---|---|---|
| `env` | text | no | id шарда (`standard`, `premium-prem03-1`, `personal-…`, `legacy`-энвы). PK |
| `plan` | text | no | какой план сюда назначается: `standard`, `lite`, `premium`, `selfhosted`; у legacy — `legacy` |
| `max_subscriptions` | int | yes | M — лимит подписок на шард. `NULL` = без лимита, ограничивают только адреса |
| `enabled` | bool | no | `false` — шард не получает ни грантов, ни карточек |
| `title` | text | yes | человеческое имя, показывается как заголовок группы |
| `kind` | text | no | `core` \| `premium` \| `personal` \| (`legacy` после 0022); выводится из плана |

### `env_networks` — адресные блоки шарда

| колонка | тип | null | смысл |
|---|---|---|---|
| `env` | text | no | шард |
| `subnet` | cidr | no | блок, например `10.88.0.0/16` |

`EXCLUDE USING gist` — пересечение блоков двух шардов невозможно. Блоков на шард может быть несколько, аллокатор идёт по порядку.

### `subscription_access` — грант (право на шард)

| колонка | тип | null | смысл |
|---|---|---|---|
| `id` | uuid | no | PK |
| `subscription_id` | uuid | no | кто |
| `env` | text | no | куда |
| `protos` | proto[] | no | `amnezia_peer`, `wireguard_peer` и/или v1-протоколы |
| `plan` | text | yes | каким планом выдано (null у ручных) |
| `sources` | text[] | no | `migration`, `manual`, … |
| `expires_at` | timestamptz | yes | `NULL` = бессрочно (квотный тариф) |
| `traffic_limit` | bigint | yes | `NULL` = без квоты (срочный тариф) |
| `revoked_at` | timestamptz | yes | отозван |
| `revoked_reason` | text | yes | почему: `expired`/`quota` (продление возвращает), `user`/`subscription_ended` (продление не трогает); `NULL` у живых и у строк до миграции `0028` |

### `peers` — туннель

| колонка | тип | null | смысл |
|---|---|---|---|
| `id` | uuid | no | PK (= `peer_id` в клиенте) |
| `device_id` | uuid | no | установка |
| `subscription_id` | uuid | no | подписка |
| `env` | text | no | шард |
| `proto` | proto | no | `amnezia_peer` / `wireguard_peer` |
| `public_key` | text | no | ключ устройства (приватник у клиента, у нас нет) |
| `address` | cidr | no | выданный адрес (`10.88.0.2/32`) |
| `label` | text | yes | подпись канала (роутеры) |
| `created_at` | timestamptz | no | |
| `revoked_at` | timestamptz | yes | |
| `revoke_reason` | text | yes | `expired`, `quota`, `user`, `subscription_ended`, `device_kick` |

### `devices` — установка

`id`, `subscription_id`, `device_uid` (uuid из Keychain), `device_name`, `public_key`, `platform`, `support_tag` (показывается в «Поддержке»), `created_at`, `rotated_at`.

### `peer_traffic` — трафик пиров

`peer_id`, `subscription_id`, `env`, `period` (`day`/`month`), `bucket`, `uplink_bytes`, `downlink_bytes`, `updated_at`.

### `subscription_limits` — override лимитов одной подписки

`subscription_id`, `peers_per_device`, `peers_per_subscription`, `updated_at`. Оба лимита —
override дефолтов конфига для этой подписки; `peers_per_device` считается по устройству
(X25519-ключ) суммарно по всем подпискам.

### `pools` — реестр пулов (для админки; карточки строятся по живым нодам)

`env`, `pool`, `pool_translation` (jsonb), `first_seen`, `last_seen`, `node_count`.

### Вьюхи

| вьюха | что |
|---|---|
| `shard_overview` | `shard, title, plan, pools, nodes, peers_live, address_hosts` |
| `shard_overview_legacy` | то же, `kind = 'legacy'` (0022) |
| `shard_overview_premium` | то же, `kind = 'premium'` (0022) |
| `shard_overview_personal` | то же, `kind = 'personal'` (0022) |

## Пример: конфиг api (пиры)

```toml
[service.peers]
enabled = true
peers_per_device = 3            # живые туннели одного устройства (по X25519-ключу), по всем подпискам
max_devices_per_subscription = 15
max_per_subscription = 45
remember_days = 7
prune_legacy_conns = false
prune_dry_run = true
prune_max_per_tick = 50
auto_subnet_pool = "10.128.0.0/9"   # премиум/персонал: по /24 из пула
auto_subnet_prefix = 24
```

`prune_legacy_conns` снимает v1-коннекты, пары `(env, proto)` которых нет в гранте, — **но только у
подписок, у которых уже есть живой пир**. Грант сам по себе не доказывает, что клиент умеет карточки:
активация выдаёт его и сборкам ниже 4.8.14.186, которые знают только `/v1/config`, а v1-коннекты живут
в `dev`/`ru`/`wl`, которых ни один шард не грантует. Поэтому флаг можно включать в любой момент: он
трогает ровно тех, кто уже на пирах, и не может отобрать конфиг у платящего на старой сборке.

Шарды и блоки — **только в базе**, конфиг их не объявляет:

```bash
curl -X PUT https://api.frkn.org/admin/api/shards/standard \
  -H "Authorization: Bearer ***" -H 'Content-Type: application/json' \
  -d '{"plan":"standard","title":"Europe","max_subscriptions":4000,
       "subnets":["10.88.0.0/16"]}'
```

## Пример: конфиг ноды (core-шард, 2 пула)

```toml
[awg_peers]
enabled = true
path = "/etc/amnezia/awg0-peer.conf"
keepalive = 25

[node]
env = "standard"                 # id шарда
uuid = "<постоянный uuid v4>"
hostname = "sh-standard-nl01.beacon.frkn.org"
default_interface = "ens3"
address = "132.243.203.40"
label = "NL STANDARD"
pool = "eu"
pool_translation = { en = "Netherlands", ru = "Нидерланды", uk = "Нідерланди" }
country = "NL"
type = "common"

[api]
endpoint = "https://api.frkn.org"
token = "<node token>"
```

`/etc/amnezia/awg0-peer.conf` — **одинаковый на всех нодах шарда**:

```ini
[Interface]
PrivateKey = <ключ шарда standard>
Address = 10.88.0.1/16
ListenPort = 51820
MTU = 1280
DNS = 1.1.1.1, 1.0.0.1
Jc = 5
Jmin = 50
Jmax = 1000
S1 = 30
S2 = 40
H1 = 1234567
H2 = 2345678
H3 = 3456789
H4 = 4567890
```

## Пример: премиум-слот

```toml
[awg_peers]
enabled = true
path = "/etc/amnezia/awg0-peer.conf"

[node]
env = "premium-prem03-1"
address = "<IP слота>"
pool = "premium"
label = "Premium DE"
country = "DE"
```

```ini
[Interface]
PrivateKey = <ключ слота>
Address = 10.128.7.1/24      # свой /24 из пула; пересечений нет
ListenPort = 51820
MTU = 1280
```

Шард: `plan = premium`, `max_subscriptions` = NULL, грант выдаётся **явно на env**.

## Пример: селфхостед

```toml
[awg_peers]
enabled = true
path = "/etc/amnezia/awg0-peer.conf"

[node]
env = "<personal-энв из install token>"
address = "<IP машины юзера>"
```

```ini
[Interface]
PrivateKey = <ключ юзера>
Address = 10.200.5.1/24     # его собственная подсеть, станет блоком шарда
ListenPort = 51820
```

Шард: `plan = selfhosted`, `kind = personal`, `max_subscriptions = 1`; доступ владельцу — по `scope_env`, без гранта; гранты другим он выдаёт из ЛК.

## Старый fnode и совместимость

- Старые ноды присылают тот же payload: `[awg_peers]` необязателен, `pool` необязателен, новых обязательных полей нет.
- Env старой ноды (`ru`, `dev`, `wl`, `custompremium-…`) парсится как именованный шард и найдёт свою `legacy`-строку после `0022` — строгая проверка их не тронет.
- Пиров на старой ноде нет: у неё нет peer-inbound, поэтому `pool_cards` и `live_shards` её пропускают. v1 работает как работал.
- Порядок обновления: `0022` → api → fnode по одной машине. Обновлённая нода с `env = "standard"` впервые начинает обслуживать пиров.
