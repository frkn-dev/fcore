# Пиры/шарды: состояние на 2026-11-20

Что оставлено «как есть» после сессии, чтобы продолжить без восстановления контекста.

## Прод

| | |
|---|---|
api | **0.7.16** на `darkmachine2.frkn.org` (`/opt/api/api`, sha `6260c709…b88027`), `api.service` active, `api.bak-0.7.14` рядом
`/healthcheck` | `{"status":200,"message":"Ok (v0.7.16)"}`
БД | миграции `0022` (kind=legacy, вьюхи по видам, `shard_overview.nodes` = count from `nodes`) **накатана**; `0023` (drop unused), `0024` (node_type core/selfhosted), `0025` (peers.pool) — **не накатаны**
шарды | `0001` (plan standard, блок 10.88.0.0/16, nodes=2), `super-duper` (plan lite, блока нет), 54 legacy-энва
тестовый грант | подписка `f76c2814-f9ed-4a40-a231-d34ec51f7584`, ключ `RNXGZ-HUHBE-AB4RS-SJNHN-KFT4J-E`, env `0001`

## Ноды шарда 0001

| | `sh-0001-nl01.beacon.frkn.org` | `aeza-0001-nl02.beacon.frkn.org` |
|---|---|---|
IP | `132.243.203.40`, `132.243.203.41` (оба `ens3`) | `176.124.198.154/32` (`net0`) |
ОС | Ubuntu 24.04, kernel 6.8.0-111 | Arch, kernel 7.3.5 (обновлён с 7.0.14 — иначе нет headers) |
AmneziaWG | kernel-модуль через DKMS (`/usr/src/amneziawg-3.1`) | то же |
Интерфейс | `awg0-peer`, `10.88.0.1/16`, udp/443 | то же, тот же ключ шарда |
fnode | 0.7.16, `fnode.service` active | то же |
Конфиги | `/opt/fnode/config.toml`, `/etc/amnezia/amneziawg/awg0-peer.conf` (awg-quick ищет именно там) | то же |
Ключ шарда | `/root/awg-peer.key`, pub `1Yg8SQiFwbwPi7J2n/M45FE5gqQ03a9zznVhLYyM0jU=` | тот же |

Обязательные поля конфига ноды, на которых спотыкались: `[node] max_bandwidth_bps`, `[service] updates_endpoint_zmq` (именно в `[service]`), `[metrics] publisher/interval/enabled/endpoint`, `ExecStart=/opt/fnode/node /opt/fnode/config.toml` (путь к конфигу — аргумент).

## Доступы

- ssh: `sh-nl01` / `sh-nl02` в `~/.ssh/config`, ключ `~/.ssh/id_ed25519_nodes` (пароли не сохранялись).
- DNS в ClouDNS (`beacon.frkn.org`): `sh-0001-nl01 A 132.243.203.40`, `aeza-0001-nl02 A 176.124.198.154`, ttl 300.
- API-токен ноды взят из `eu01.frkn.org:/opt/fnode/config.toml`, в репозиторий не попадал.
- `admin_token` api — `/opt/api/config.toml` на `darkmachine2`.

## Проверено живьём

Полный цикл: `POST /key {"days":30,"product":"standard"}` → `POST /v1/activate {"code":…}` → ответ `env=0001`, `capacity{plan:standard,used:1,limit:58979,overflow:false}`, в БД грант с `protos` включая `amnezia_peer`.

Ключевое: **создание ключа — `POST /key`** (не `/v1/key`, отдаёт 405); активация — `/v1/activate` или `/key/activate`.

## Обновление вечером 2026-11-20

- Прод — **0.7.17**, миграции `0024` (node_type core/selfhosted) и `0025` (peers.pool) накатаны, `0023` нет. Бинарь `sha256 56784a7e…04ed`, бэкап `/opt/api/api.bak-0.7.16-20261120-203359`.
- Шард `0001` проверен живым туннелем: пир по `card_id` → ядро на обеих нодах → хендшейк и ping `10.88.0.1` с `ee.frkn.org` (nl01 29 мс, nl02 38 мс). Шард снова пуст (0 пиров, 0 устройств).
- Ноды шарда переведены под systemd: `awg-quick@awg0-peer` был поднят руками и inactive; `systemctl restart` не работает (юнит без ExecStop) — нужен `awg-quick down` + `systemctl start`.
- Открыто: реестр `pools` (`shard_overview.pools`) пуст — фикс `75928e8` (jsonb/int4 в `note_pool`) собран но не выкачен, ждёт `0.7.18`.

## Осталось

1. **0.7.18 (сервер):** выкатить `75928e8` (запись реестра пулов). Сам `0.7.17` уже на проде: `card_id` в `PeerView`, `reap_stale_devices`, фикс `GET /admin/api/shards`.
2. **Клиент:** адаптер `PoolCard → ServersModel` (без нового экрана: пулы и `localizedPoolName` уже есть), кэш конфигов + инвалидация по `revoke_reason` (подключение без api, если конфиг есть), экран устройств с удалением, приватный ключ в Keychain.
3. Сборка клиента: `QT_BIN_DIR=/Users/2pizza/c/6.10.1/6.10.1/macos/bin bash deploy/build_macos_local.sh`; для iOS — реконфиг cmake в `build-ios-upload` (новые 4 файла в `sources.cmake`).
