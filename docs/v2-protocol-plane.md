# Hysteria2 и VLESS как first-class протоколы в v2 (peers/card)

Дизайн-документ. Только чтение кода: `~/c/f/ffcore` @ `feature/peers`, `~/c/f/dopamine` @ `dev`
(+ `~/c/f/env` как источник фактов о живой инфре). Ничего не собрано и не выкачено.

**Решение в двух строках.** Карточка получает `kind: peer | client_config` и понодный
`payload`; клиент диспатчит по `kind`. Hysteria2 в v2 — бесплатно на транспорте (он есть),
но требует модели авторизации: живая нода уже ходит за auth в `https://api.frkn.org/auth`,
значит per-subscription токен — это расширение **уже существующего** auth-сервиса, а не новый
контур. VLESS дороже: нужен UUID на подписку + `AddUser` в xray на ноде + понодные
reality-параметры в карточке.

---

## 0. TL;DR для нетерпеливых

| | факт | вывод |
|---|---|---|
| `/v1/locations` | `cards_for_grants` перебирает ровно два прототипа: `[PeerProto::Amnezia, PeerProto::Wireguard]` (`src/bin/api/http/handlers/peers.rs:597`) | hysteria2/VLESS не попадут в ответ **никогда**, пока в этом массиве два элемента |
| хранение прав | `subscription_access.protos` — колонка типа `proto[]` (`deploy/migrations/0020_peers.sql:41`), `tag_from_wire` принимает **любой** `Tag` (`src/bin/api/peers/mod.rs:263-279`) | БД и API грантов уже умеют `hysteria2` / `vless_*`; миграция enum **не нужна** |
| нода | h2 и все vless-инбаунды **уже** объявляются в `node.inbounds` (`src/memory/node.rs:306-315`, `416-429`) | api видит их в `InboundResponse` с `stream_settings` / `h2` — данных для карточки хватает |
| клиент | hysteria2-транспорт **реальный** и рабочий: xray-core + `XrayProtocol` + транслятор схемы (`client/core/api/apiUtils.cpp:233`) | новый транспорт писать не надо; надо научить v2-строку отдавать xray-outbound вместо WG-INI |
| плюс | pills, `protocolLabel("hysteria2"|"vless")`, `protocolRank`, multi-IP failover по `node_ips` — **уже** протокол-агностичны (`ServersListView.qml:71-111`, `connectionController.cpp:44-135`) | UI-часть почти готова |

Три блокера, в порядке появления:

1. `PeerProto` — enum из двух значений, он же тип `card_id`, он же гейт прав (`Grant::allows`,
   `check_grant`) — `src/bin/api/peers/mod.rs:78-113`, `303-314`, `460-475`.
2. `POST /v1/peers` — единственный способ «получить доступ»: требует `public_key`, выдаёт
   `address`, а клиент без непустого `peer.address` не собирает конфиг
   (`src/bin/api/http/handlers/peers.rs:990-1165`, `client/ui/controllers/peersImportController.cpp:495-506`).
3. Ключ клиента строится из WG-параметров: `PeerConfigBuilder::build` пишет `[Interface]/[Peer]`
   (`client/core/peerConfigBuilder.cpp:100-136`), а `containerForProto` любого «не wireguard»
   отправляет в `DockerContainer::Awg` (`peersImportController.cpp:187-194`).

---

## 1. Текущее состояние (file:line)

### 1.1 Как `/v1/locations` строит карточки

Ручка: `POST /v1/locations` (AGW-конверт), `src/bin/api/http/handlers/peers.rs:1392-1439`.

```
get_locations_handler
  ├─ grants = mem.peers_state.access.of_sub(&subscription_id)        :1410-1418
  ├─ access[] = access_view(grant, used)                              :1420-1427
  ├─ cards   = cards_for_grants(&mem.nodes, &grants)                  :1429-1432
  └─ { "access": access, "pools": cards }                             :1434-1438
```

`cards_for_grants` (`:594-628`) — тот самый жёсткий фильтр:

```rust
for proto in [PeerProto::Amnezia, PeerProto::Wireguard] {   // :597
    if grant.protos.contains(&proto.tag()) {                // :598
        out.extend(pool_cards(nodes, &grant.env, proto));   // :599
    }
}
```

`pool_cards` (`:465-592`) группирует живые ноды env по `node.pool` и берёт из инбаунда
клиентские параметры:

- поиск инбаунда по тегу: `node.inbounds.get(&tag)` (`:486`) — нода без этого инбаунда в
  карточку не попадает;
- разбор по типу протокола: `match proto` (`:496-529`), где `PeerProto::Amnezia` берёт
  `inbound.awg.interface.private_key.pubkey()`, MTU, DNS, keepalive, obfuscation, а
  `PeerProto::Wireguard` — `inbound.wg.keys.pubkey()`, MTU 1420, DNS, keepalive (`:514-528`);
- endpoint-ы: `card_endpoints(node.entry_ips(), inbound.port)` — один `PoolNode` на каждый
  entry-IP (`:555-568`, `:453-458`);
- карточка = пул: `card_handle(env, pool, proto)` = `<shard>:<pool>:<proto>` (`:323-325`,
  `:580`).

Форма ответа — `PoolCard` (`:236-251`) и `PoolNode` (`:222-233`):

```rust
pub struct PoolCard { card_id, pool, pool_translation, country, country_name, proto, nodes: Vec<PoolNode> }
pub struct PoolNode { endpoint, country, server_public_key, mtu, dns, keepalive, obfuscation }
```

Обратите внимание: **`kind` отсутствует, `proto` — просто строка**, а «payload» размазан по
типизированным полям WG-семейства (`server_public_key`, `mtu`, `dns`, `keepalive`,
`obfuscation`). Это и есть место расширения — не ломая старые поля.

Имя протокола на проводе: `proto_wire_name` (`:309-316`) — peer-протоколы уходят в snake_case
(`amnezia_peer`), всё остальное — `tag.to_string()`, т.е. CamelCase (`Hysteria2`,
`VlessXhttpCdn`). Клиент это переживает: `protoOfCard` приводит к нижнему регистру
(`peersImportController.cpp:174-183`), а `protocolLabel` знает `"hysteria2"` и `"vless"`
(`ServersListView.qml:71-79`).

### 1.2 Как хранятся теги и права

`ProtoTag` (`src/memory/tag.rs:26-59`) — Postgres-enum `proto` (`rename_all = "snake_case"`,
`:25`) и rkyv-архив. Значения hysteria2/VLESS там есть с v1:

```
VlessTcpReality, VlessGrpcReality, VlessXhttpReality, VlessXhttpCdn, Vmess,
Shadowsocks, Wireguard, AmneziaWg, Hysteria2, Mtproto, AmneziaWgMobile,
AmneziaPeer, WireguardPeer                                   // tag.rs:27-53
```

Важно: `AmneziaWgMobile`, `AmneziaPeer`, `WireguardPeer` **дописаны в конец** намеренно —
rkyv-дискриминанты позиционные (`tag.rs:48-58`). Любой новый тег — только в конец, иначе
старые ноды/апи перестанут читать архив.

`is_peer()` — ровно два значения (`tag.rs:104-106`). На нём висят: назначение шардов
(`peers.rs:668`), регистрация ноды (`handlers/node.rs:26`, `:299`), активация (`key.rs:425`,
`:437`), «что из гранта идёт в v1-коннекты» (`access.rs:46`).

Права: `Grant { env, protos: Vec<Tag>, sources, expires_at, traffic_limit }`
(`src/bin/api/peers/mod.rs:303-309`), проверка —

```rust
pub fn allows(&self, proto: PeerProto) -> bool { self.protos.contains(&proto.tag()) }   // :312-314
pub fn check_grant(grant, proto: PeerProto, now) -> Result<(), Refusal>               // :460-475
```

Обе принимают **`PeerProto`**, не `Tag`. Это и есть типизированный блокер №1: набор прав
физически не может выразить «hysteria2 разрешён» для карточки, хотя `protos` его хранит.

Запись грантов: `POST /v1/access` → `tag_from_wire(name)` (`peers.rs:1505-1516`) принимает любой
тег, включая `hysteria2`, `vless_xhttp_reality`. Активация ключом уже подмешивает v1-теги из
каталога в грант (`key.rs:414-451`), т.е. **подписка с `hysteria2` в `protos` — это сегодня
нормальный, живой случай**, просто карточек по нему нет.

### 1.3 Как нода объявляет инбаунды

`ServiceSettings` (`src/bin/node/config.rs:19-56`): `xray` (фича `xray`), `wg`, `awg`,
`awg_mobile`, **`awg_peers`**, **`wg_peers`**, **`h2`**, **`mtproto`**.

Сборка `Node::inbounds` — `src/memory/node.rs:291-430`:

| инбаунд | строки | тег |
|---|---|---|
| xray (`config.inbounds`, из yaml ноды) | `306-315` | `VlessTcpReality` / `VlessGrpcReality` / `VlessXhttpReality` / `VlessXhttpCdn` / `Vmess` / `Shadowsocks` |
| `[wg]` | `317-331` | `Wireguard` |
| `[awg]` | `333-347` | `AmneziaWg` |
| `[awg_mobile]` | `349-363` | `AmneziaWgMobile` |
| **`[awg_peers]`** | `369-383` | `AmneziaPeer` |
| **`[wg_peers]`** | `385-399` | `WireguardPeer` |
| `[mtproto]` | `401-414` | `Mtproto` |
| **`[h2]`** | `416-429` | `Hysteria2` |

То есть **нода уже анонсирует `Hysteria2` и все VLESS-теги**, если они подняты; `/v1/locations`
их просто не спрашивает. Прямая цитата комментария рядом с peer-инбаундами
(`memory/node.rs:365-368`): «the api serves peers from these and never creates connections for
them» — это про WG/AWG; h2/vless сейчас живут в **connection**-плоскости.

`H2Settings` (`src/config/h2.rs:73-83`) содержит `host, port, sni, insecure, obfs, alpn,
up_mbps, down_mbps, auth_info` — **пароля в нём нет**; `auth_info` (`:38-42`) несёт только
`{auth_type, has_password, has_url}`, т.е. метаданные, а не секрет. Это ключевой факт для §5.

`Inbound` (`src/config/inbound.rs:100-115`) несёт `stream_settings: Option<StreamSettings>`, а
`RealitySettings` (`:47-57`) — `server_names, private_key, public_key, short_ids, target`.
`public_key` — **понодный** (генерируется на ноде при установке xray), поэтому клиентский
конфиг VLESS-Reality не может быть общим на пул.

### 1.4 Что клиент делает с карточкой

Конвейер целиком:

```
GatewayController (AGW) → PeersController::locations  client/core/controllers/peersController.cpp:144-167
  → parseCard / parseNode            peersController.cpp:38-67   (+ PoolCard/PoolNode: peersController.h:22-44)
  → groupCardsByPool                 peersImportController.cpp:251-278
  → buildPoolRow                     peersImportController.cpp:299-425
  → ServersModel::syncPeerCards      servers_model.cpp:1330-1420
  [connect]
  → ensureTunnelForRow               peersImportController.cpp:774-788   (вызов из connectionController.cpp:928)
  → buildTunnel → POST /v1/peers     peersImportController.cpp:450-508
  → protoForNode → PeerConfigBuilder peersImportController.cpp:123-163 + peerConfigBuilder.cpp:100-136
  → setPeerConfig                    peersImportController.cpp:830
  → createVpnConfiguration           connectionController.cpp:1523 → vpnConfigurationController.cpp:30-56, 271
  → VpnProtocol::factory             vpnprotocol.cpp:104-114
```

Конкретика по каждому шагу:

- **`buildTunnel`** (`peersImportController.cpp:450-508`) — жёсткий контракт peer-плоскости:
  ```
  :469-481  выбрать ноду: пропустить, если serverPublicKey пуст / endpoint битый / хост отключён
  :495      createPeer(subscription_id, device_uid, card_id, publicKey) → PeerInfo
  :500      if (peer.address.isEmpty()) -> ошибка, конфига нет
  :504      protoForNode(card, *node, privateKey, publicKey, peer.address, peer.peerId)
  ```
  Для hysteria2/VLESS здесь нет ни `public_key` (нечего посылать), ни `address` (нечего получать).
- **`protoForNode`** (`:123-163`) кладёт `config = PeerConfigBuilder::build(...)` (WG-INI),
  `client_ip`, `server_pub_key`, `hostName`, `port`, `mtu`, `allowed_ips`,
  `persistent_keep_alive`, junk-параметры и `last_config` (JSON-строка того же объекта).
- **`PeerConfigBuilder::build`** (`peerConfigBuilder.cpp:100-136`) — единственный генератор:
  `[Interface] PrivateKey/Address/DNS/MTU` + `[Peer] PublicKey/AllowedIPs/Endpoint/
  PersistentKeepalive`. Ветка «не-amnezia» — это просто WireGuard.
- **`containerForProto`** (`:187-194`): `wireguard*`/`wg*` → `DockerContainer::WireGuard`,
  **всё остальное → `DockerContainer::Awg`**. `protocolForProto` (`:203-208`) и
  `protocolKeyForProto` (`:210-213`) наследуют это через
  `ContainerProps::containerTypeToProtocolString/ToString` (`containers_defs.cpp:202-208`,
  `:44-56`).
- **модель/строки**: `buildPoolRow` (`:299-425`) пишет `peerCardId`, `peerPoolKey`,
  `peerVariants` (по варианту на протокол), `peerNodes`, `node_ips`/`node_ports`,
  `displayInfo.protocol/proto`, `api_config.service_protocol`; `variants[i]` =
  `{cardId, proto, protocol, container, country, countryName, nodes}` (`:245-267`).
- **роли**: `ServiceProtocolRole` / `ServiceProtocolFilterRole` читают
  `api_config.service_protocol`, иначе `displayInfo.protocol` (`servers_model.cpp:400-425`).
- **переключение протокола**: `setPeerPoolProtocol(serverIndex, cardId)` (`servers_model.cpp:1555-1600`)
  паркует текущий туннель в `peerVariantConfigs`, применяет вариант
  (`applyPeerVariant`, `:158-236`) и запоминает выбор (`Settings::setPeerPoolProtocol`).
- **pills**: `ServersListView.qml:717-790` — `Repeater` по `variant.peerVariants`, активная
  подсвечена по `variant.peerCardId`, тап → `ServersModel.setPeerPoolProtocol`. Видимость — по
  `variant.isPeerPool && peerVariants.length > 1` (`:738-741`). **Список протоколов уже
  рисуется из данных, а не из кода.**
- **connect-диспетчер**: `VpnProtocol::factory` (`vpnprotocol.cpp:104-114`) —
  `Awg/Awg2/WireGuard → WireguardProtocol`, `Xray/SSXray → XrayProtocol`. Больше ничего.

---

## 2. Где hysteria2 и VLESS уже есть в коде

### 2.1 ffcore — всё живое, кроме одного места

| место | что это | вердикт |
|---|---|---|
| `src/config/h2.rs:73-83` (`H2Settings`), `:1-20` (`Hysteria2Settings`), `:90-125` (validate) | разбор yaml ноды hysteria2 | рабочее, v1-плоскость |
| `src/config/inbound.rs:686-706` (`fn h2`) | `hysteria2://token@host:port?...` для подписки | рабочее, **v1**; использует `conn.get_token()` |
| `src/proto/conn.rs:207-247` (`pub fn h2`) | тот же линк через `Inbound`, плюс obfs/alpn (сейчас **не** выносятся в URL: `_obfs_type`, `_obfs_pass`, `_alpn` — `:217-228`) | рабочее, v1; obfs **не пробрасывается** — тихая дыра |
| `src/config/inbound.rs:520-600, 610-660` + `src/proto/conn.rs:100-204` | vless `tcp/reality`, `grpc/reality`, `xhttp/reality`, `xhttp/cdn` линки (pbk/sid/sni/path/flow) | рабочее, v1; **готовый источник полей для payload** |
| `src/bin/node/tasks.rs:465-496` | создание VLESS-юзера на ноде: `Proto::new_xray(tag)` + `handler_client.create(conn_id, tag, None)` (xray proxyman gRPC `AddUser`) | рабочее; ключ — uuid `conn_id` |
| `src/bin/node/tasks.rs:687-742` (`apply_peer`) | v2-плоскость: только `AmneziaPeer` / `WireguardPeer`, иначе `Err("peer message for a non-peer tag")` (`:735-739`) | **вот сюда h2/vless не влезают** |
| `src/bin/auth/service.rs:74-80` | `POST /auth` — HTTP-auth hysteria2 | рабочее, **готовый вход для v2-токенов** |
| `src/bin/auth/handlers.rs:9-31` | `auth_handler`: `mem.validate_token(&req.auth)` → `{ok, id}` | рабочее, но вселенная токенов = v1 `connections` |
| `src/memory/storage/connection.rs:51-60` | `validate_token`: ищет `Proto::Hysteria2 { token }` среди коннектов | узкое место, см. §5 |
| `src/bin/auth/http.rs:24-100` | синк токенов: `POST /connections/sync` с `proto=Hysteria2` | рабочее; второго источника нет |
| `src/bin/node/metrics.rs:64-67` | `if matches!(tag, Tag::Hysteria2 \| Tag::Mtproto) { continue; }` | **дыра: у h2 нет метрик трафика вообще** |
| `src/bin/api/access.rs:30-52` (`grants_to_conns_map`) | грант → v1-коннекты, peer-теги отбрасываются | **коллизия плоскостей**, см. §4.1 |
| `src/bin/api/entitlements.rs:116-124` | в тестах: `Env::Dev → [Hysteria2]`, `Env::Wl → [VlessXhttpCdn]` | каталог умеет эти теги как v1-права |

### 2.2 dopamine — 14 файлов (без build/ и 3rd/)

Клиент действительно имеет **рабочий** hysteria2-транспорт, но он целиком в v1-семантике
(«строка = готовый xray-outbound в `container.amnezia-xray.config`»).

| файл | строки | что делает | вердикт |
|---|---|---|---|
| `client/protocols/xrayprotocol.cpp` | `23-60`, `52-66` | старт xray-ядра через IPC (`iface->xrayStart`), tun2socks | **рабочий транспорт**, переиспользуется как есть |
| `client/core/api/apiUtils.cpp` | `233-283` | `translateLegacyHysteria2Outbound`: `protocol:"hysteria2"`,`settings.servers[]`,`network:"udp"` → `protocol:"hysteria"`,`network:"hysteria"`,`hysteriaSettings{version:2,auth}`,`alpn:["h3"]` | **несущая вещь**: без неё xray-ядро не понимает бэкендовый h2 |
| `client/core/controllers/vpnConfigurationController.cpp` | `231-243` | применяет транслятор для `DockerContainer::Xray`, добавляет socks-inbound на случайный порт (`:245-269`) | рабочее; вызывается и для v2-строки, если контейнер = `amnezia-xray` |
| `client/core/controllers/vpnConfigurationController.cpp` | `30-56`, `271-280` | `createVpnConfiguration`: читает `container[proto].last_config/config`, кладёт `xray_config_data` | рабочее; **точка входа для v2** |
| `client/ui/controllers/healthCheckController.cpp` | `476-500`, `906-940` | h2-проба: `xray`-контейнер → outbound → `LibXrayPing` через socks | рабочее; читает ровно ту форму, которую надо будет писать в v2-строку |
| `client/ui/models/servers_model.cpp` | `2483-2560` | `hysteriaDialOutbound/Port/Domain/Host` — разбор `settings.servers[0]` | рабочее, v1-разбор для отображения |
| `client/ui/models/servers_model.cpp` | `2596-2601` | порт строки из outbound для h2 | рабочее |
| `client/ui/controllers/connectionController.cpp` | `44-135` | `patchAddressInJsonObject`: multi-IP failover для `settings.servers[]` (h2 legacy), `settings.address` (h2 translated), `vnext[]` (vless) | **готовый failover по `node_ips` для обоих протоколов** |
| `client/ui/controllers/connectionController.cpp` | `2022`, `2046-2057` | тиринг авто-выбора: `hysteria2 → 2`, `vless → 4/5`, `isVlessCdnRow` читает xray-outbound | рабочее, уже ждёт такие строки |
| `client/ui/qml/Components/ServersListView.qml` | `71-79`, `102-111`, `717-790` | `protocolLabel` (`hysteria2`, `vless`), `protocolRank`, pills | **готово под v2** |
| `client/ui/controllers/api/apiConfigsController.cpp` | `652`, `670` | порядок сортировки v1-конфигов | v1, не мешает |
| `client/ui/qml/Pages2/PageSettingsApiServerInfo.qml` | `102`, `257-272`, `743` | показ h2-строки: домен для TLS, порт из outbound | v1-отображение, переиспользуемо |
| `client/ui/qml/Pages2/PageServerTest.qml` | `76` | `if (lower.indexOf("hysteria") === 0)` | v1 |
| `client/core/controllers/connectionReportController.cpp` | `179`, `1014` | классификация протокола в отчётах | нейтрально |

**Чего в клиенте нет вообще:** ни одного упоминания VLESS в *транспортном* слое отдельно от
xray — он всегда «xray-контейнер с outbound `vless`». Это нормально: VLESS не нужен свой
транспорт, нужен корректный outbound + UUID.

Вывод §2: **транспорт есть, карточки нет.** Всё, что нужно клиенту — научиться строить
xray-outbound (h2) / xray-outbound (vless) из понодного payload и положить его в
`container.amnezia-xray.{config,last_config}`; дальше работает существующий путь
`createVpnConfiguration → XrayProtocol`.

---

## 3. Структурная разница: peer vs client_config

| | **peer** (AWG/WG) | **client_config** (hysteria2/VLESS) |
|---|---|---|
| кто владеет секретом | клиент (X25519 privkey) | сервер (пароль h2 / UUID vless) |
| что выдаёт api | адрес в подсети env + серверный pubkey | ничего адресного; авторизацию и параметры |
| единица учёта | `peers` (public_key, address) | `peer_credentials` (secret на подписку×env) |
| лимит устройств | по pubkey (нельзя подделать) | только по `device_uid` (подделываемо) |
| конфиг клиента | INI, собирается из `PoolNode` | xray-outbound JSON |
| метрика трафика | счётчики интерфейса по peer_id | h2 — **нет** (metrics.rs:66), vless — по user-статистике xray |
| что значит «создать туннель» | `POST /v1/peers` | ничего (h2) / `AddUser` на ноде (vless) |

### 3.1 Контракт карточки (аддитивный)

```json
{
  "card_id": "standard-eu:eu:hysteria2",
  "pool": "eu",
  "proto": "hysteria2",
  "kind": "client_config",
  "country": "NL",
  "country_name": "Netherlands",
  "nodes": [
    { "endpoint": "203.0.113.10:8443",
      "country": "NL",
      "payload": {
        "sni": "eu-0001.beacon.frkn.org", "insecure": false,
        "alpn": ["h3"], "obfs": null,
        "up_mbps": 0, "down_mbps": 0,
        "auth": "b1c0…"                     // per-subscription токен, см. §5
      } }
  ]
}
```

- `kind` отсутствует ⇒ `"peer"` (старые ответы байт-в-байт валидны, старый клиент не замечает
  нового поля).
- `kind: "peer"` сохраняет `server_public_key/mtu/dns/keepalive/obfuscation` как есть.
- `kind: "client_config"` добавляет **только** `payload` в `PoolNode`; `server_public_key`
  остаётся пустым, `dns/mtu/keepalive` можно не заполнять.
- `card_id` продолжает быть `<shard>:<pool>:<proto>`, поэтому `groupCardsByPool`
  (`peersImportController.cpp:251-278`) группирует hysteria2 в ту же строку-пул, что AWG/WG —
  **pills появляются сами**.

### 3.2 Почему payload понодный, а не на карточке

- VLESS-Reality: `pbk`/`sid`/`sni` — из `inbound.stream_settings.reality_settings`
  (`config/inbound.rs:47-57`), ключ генерируется **на ноде** ⇒ у двух нод одного пула он
  разный. Общий конфиг на пул физически невозможен.
- hysteria2: `sni`/`insecure`/`alpn`/`obfs`/`host` — общие на пул (`pool_inbounds_conflict`
  уже сравнивает `h2.sni` и `stream_settings` между нодами пула,
  `http/request.rs:385-393`, `:405-470`), но `auth` — секрет на подписку, а порт бывает свой
  на каждой машине (порт намеренно **не** сравнивается, `request.rs:335-338`).
- Прецедент уже есть: `PoolNode.server_public_key` понодный, и клиент это переживает
  (`nodeToJson/nodeFromJson`, `peersImportController.cpp:88-118`).

Итого: `PoolCard` получает `kind: "peer" | "client_config"`, `PoolNode` — `payload:
Option<serde_json::Value>` (opaque для api, типизированный для клиента).

---

## 4. Gap list

### 4.1 api — обязательное

| # | место | что сделать |
|---|---|---|
| A1 | `src/bin/api/peers/mod.rs:78-113` (`PeerProto`) | заменить на `CardProto { Amnezia, Wireguard, Hysteria2, Vless(Tag) }` **или** оставить `PeerProto` для peer-части и ввести `CardKind`+`Tag`. Минимальный вариант: `PeerProto` → `CardProto` с `kind()` и `tag()`, `peer_tag() -> Option<Tag>` |
| A2 | `:303-314` (`Grant::allows`), `:460-475` (`check_grant`) | принимать `CardProto`; `allows` = `protos.contains(&proto.tag())` (уже так, меняется только тип) |
| A3 | `handlers/peers.rs:597` | перебирать **все** `grant.protos`, а не два литерала: `for tag in &grant.protos { if let Some(p) = CardProto::from_tag(*tag) { … } }`. Именно здесь сегодня «пропадают» h2/vless |
| A4 | `handlers/peers.rs:465-592` (`pool_cards`) | ветка `match proto`: `CardProto::Hysteria2 => (inbound.h2)` → собрать `payload` (host/port/sni/insecure/alpn/obfs/up/down); `CardProto::Vless(tag)` → `payload` из `inbound.stream_settings` (reality pbk/sid/sni, xhttp path/mode/extra, flow). `server_public_key` для этих веток — пустой |
| A5 | `handlers/peers.rs:236-251` (`PoolCard`), `:222-233` (`PoolNode`) | добавить `kind` (с `#[serde(default)]` → `"peer"`) и `payload: Option<serde_json::Value>` |
| A6 | `handlers/peers.rs:329-395` (`resolve_card`) | `PeerProto::parse` → `CardProto::parse` (`hysteria2`, `vless_xhttp_cdn`, …); для `client_config` возвращать признак, чтобы `post_peers_handler` не пытался выделять адрес |
| A7 | `handlers/peers.rs:832-1165` (`post_peers_handler`) | **не вызывать** для `client_config` (или отвечать `400 need_client_config`). Иначе он потребует `public_key` (`:897-899`, валидация `:858-862`) и попробует `insert_peer` с `address` |
| A8 | новый `authorize`-путь, см. §5 | `POST /v1/authorize` (AGW): `{subscription_id, device_uid, card_id}` → `{credential: {kind, secret, expires_at}}`. Отдаёт/продлевает per-subscription секрет |
| A9 | `src/bin/api/access.rs:30-52` | **коллизия**: грант с `Hysteria2` уже сегодня рождает v1-коннект через `grants_to_conns_map`. Решить явно: либо `client_config`-теги перестают быть v1-правами (тогда v1-подписки ломаются), либо вводится `sources`-маркер `v2_card` и `grants_to_conns_map` его пропускает. **Это единственный пункт, где можно тихо сломать v1** |
| A10 | `src/bin/api/config.rs:167-…` (`PeersConfig`) + `api-example.toml:72-97` | флаг состава карточек: `card_protos = ["amnezia_peer","wireguard_peer"]` → потом `+["hysteria2"]`. Гейт по версии клиента **отсутствует**: `X-Version`/min-version api не читает (в `gatewayController.cpp:54` строка «client version update is required» только распознаётся клиентом) ⇒ единственный безопасный рубильник — серверный список протоколов |

### 4.2 api — auth (детали в §5)

| # | место | что |
|---|---|---|
| A11 | `src/bin/auth/handlers.rs:9-31`, `src/memory/storage/connection.rs:51-60` | второй источник токенов: `validate_token` сейчас смотрит только `Proto::Hysteria2{token}` среди v1-коннектов |
| A12 | `src/bin/auth/http.rs:24-100` | подтягивать v2-креды: либо новый `POST /v1/auth/sync` (Bearer), либо `GET /v1/auth/tokens` |
| A13 | `src/bin/auth/*` (или api) | revoke-путь: `DELETE /v1/access` / истечение гранта должны гасить токен в кэше auth-сервиса (`expire_grants` — `src/bin/api/tasks.rs`, причина `expired`/`quota`) |

### 4.3 fnode

| # | место | что |
|---|---|---|
| N1 | `src/bin/node/tasks.rs:687-742` (`apply_peer`) | `other => Err(...)` (`:735-739`). Для `client_config` нужен отдельный путь: h2 — **ничего** (auth внешний), vless — `handler_client.create(subject_uuid, tag, None)` (`:474-489`) |
| N2 | `src/zmq/message.rs:73-81` (`PeerMessage`) | либо новое сообщение `ClientConfigMessage { subject_id, action, tag }` (чище), либо новые поля в `PeerMessage` — **только в конец** (rkyv позиционный, как `tag.rs:48-58`) |
| N3 | `src/bin/api/http/handlers/peers.rs:768-830` (`publish_peers_init`), `:726-761` (`publish_peer`) | публиковать init-набор кред для `peers-init-<uuid>` (`zmq/topic.rs:19`), иначе после рестарта ноды vless-юзеры пропадут до первого connect |
| N4 | `src/bin/node/metrics.rs:64-67` | h2 выпадает из метрик. Нужен сборщик: (а) `/traffic` API hysteria2 по пользователю, или (б) признать, что квота по h2 не считается, и **не продавать** h2 в лимитированных тарифах до N4 |
| N5 | `src/bin/node/stats.rs:28`, `:68-92` | vless-статистика уже per-user (`user>>>{uuid}@pony`) — подключить к `peer_traffic` по `credential_id` |

### 4.4 client

| # | место | что |
|---|---|---|
| C1 | `client/core/controllers/peersController.h:22-44` | `PoolCard.kind`, `PoolNode.payload` (`QJsonObject`), парсинг в `peersController.cpp:38-67` |
| C2 | `peersImportController.cpp:88-118` (`nodeToJson/nodeFromJson`) | round-trip `payload` (иначе переживёт refresh, но потеряется при `retargetPeerConfig`/смене варианта) |
| C3 | `:187-194` (`containerForProto`) | `hysteria2`/`vless*` → `DockerContainer::Xray`; заодно `protocolForProto`/`protocolKeyForProto` (`:203-213`) должны давать **`serviceProtocol`** = `hysteria2`/`vless`, а ключ контейнера = `xray`. Иначе: и ключ конфига, и фильтр/тиринг поедут (сейчас всё «не wireguard» = AWG) |
| C4 | `:215-231` (`protocolRank`) | ранги для hysteria2/vless (QML-версия уже есть — `ServersListView.qml:102-111`) |
| C5 | `:245-267` (`variantJson`) | `kind` в вариант (для C6/C7) |
| C6 | `:450-508` (`buildTunnel`) | ветка `kind == client_config`: **не** звать `createPeer` (`:495`), не требовать `serverPublicKey` (`:472-476`), вместо этого `PeersController::authorize` (`peersController.cpp:219-240` → новый) и `PeerConfigBuilder::buildClientConfig` |
| C7 | `:123-163` (`protoForNode`) | ветка xray: `config` = JSON xray-конфига (outbounds), `last_config` = тот же JSON, `hostName`/`port` из endpoint, `dns1/dns2`; **не** писать `client_priv_key`/`client_ip`/`server_pub_key` |
| C8 | `client/core/peerConfigBuilder.cpp:100-136` | новый `buildClientConfig(kind, payload, host, port)`: h2 → `{outbounds:[{protocol:"hysteria2", settings:{servers:[{address,port,password}]}, streamSettings:{network:"udp", tlsSettings:{serverName, alpn:["h3"], allowInsecure}}}]}` (транслятор в `network:"hysteria"` применится сам — `vpnConfigurationController.cpp:231-243`); vless → `{protocol:"vless", settings:{vnext:[{address,port,users:[{id,encryption:"none",flow}]}]}, streamSettings:{network:"xhttp"|"tcp", security:"reality", realitySettings:{serverName,publicKey,shortId,spiderX}, xhttpSettings:{path,mode}}}` |
| C9 | `:840-895` (`retargetPeerConfig`) | снять требование `target->serverPublicKey` для `client_config` (сейчас `:859` возвращает `false`) — иначе failover по IP не пересоберёт конфиг |
| C10 | `peersImportController.cpp:774-788` (`ensureTunnelForRow`) | для `client_config` «туннель» = наличие кеша; строка `peerRowHasConfig` (`servers_model.cpp:1226-1251`) проверяет непустой `config` — подходит, если C7 кладёт JSON в `config` |
| C11 | `client/core/controllers/connectionController.cpp:1523`, `vpnprotocol.cpp:104-114` | ничего: `amnezia-xray` → `XrayProtocol` уже есть; `createVpnConfiguration` (`vpnConfigurationController.cpp:30-56`) соберёт `xray_config_data` из `container.amnezia-xray.config` |
| C12 | `healthCheckController.cpp:476-500` | для v2-строки h2 работает как есть (читает контейнер `xray` → outbound). Для vless пинг пойдёт тем же `LibXrayPing` — проверить, что `isVlessCdnRow` (`connectionController.cpp:2036-2057`) не путает v2-строку с v1 |

### 4.5 БД

| # | что | нужно? |
|---|---|---|
| D1 | enum `proto`: `hysteria2`, `vless_tcp_reality`, `vless_grpc_reality`, `vless_xhttp_reality`, `vless_xhttp_cdn` | **нет**. Значения есть с v1 (иначе `tag.rs` не смог бы писать их в `connections.proto`); `0020_peers.sql:12-13` добавлял только два `*_peer` |
| D2 | `subscription_access.protos` | **нет**. Тип `proto[]` (`0020:41`), `tag_from_wire` принимает любой тег (`peers/mod.rs:263-279`) |
| D3 | креды | **да**, новая таблица: `peer_credentials(id uuid pk, subscription_id, env, proto, subject uuid, device_id uuid null, label text, created_at, revoked_at, revoke_reason)`, уникальность `(subscription_id, env, proto) WHERE revoked_at IS NULL`. `peers` не переиспользовать: `public_key`/`address` — `NOT NULL` (`0020:129-132`), а `address` ещё и `UNIQUE (env,address) WHERE revoked_at IS NULL` (`:143-144`) |
| D4 | трафик | **нет**. `peer_traffic(peer_id, subscription_id, env, …)` **без FK** намеренно (`0020:160-172`) ⇒ можно писать `credential_id` в `peer_id`, и вьюха `subscription_traffic` (`:174-181`) сложит это с v1-коннектами |
| D5 | индекс под auth-синк | желателен: `sexp`-подобный не нужен, хватит `(env, proto) WHERE revoked_at IS NULL` + `(subject)` |

---

## 5. Авторизация

### 5.1 Что есть у hysteria2 сегодня

Живая нода (пример — `~/c/f/env/dev/servers/us.frkn.org/hysteria-config.yaml`, и
`kz.frkn.org/h2.conf`, и `deploy/ubuntu/scripts/install/h2.sh:12`):

```yaml
auth:
  type: http
  http:
    url: https://api.frkn.org/auth
```

`https://api.frkn.org/auth` → nginx `proxy_pass http://auth` (пример:
`env/dev/servers/darkmachine2.frkn.org/nginx/api.conf:232-233`) → **отдельный бинарь** `auth`
(`src/bin/auth/`, релизится как `auth-$ARCH` — `env/deploy/ubuntu/scripts/update/auth.sh:21`).

Обработчик (`src/bin/auth/handlers.rs:9-31`):

```rust
if let Some(id) = mem.validate_token(&req.auth) { {ok:true, id: Some(id)} } else { {ok:false} }
```

`validate_token` (`src/memory/storage/connection.rs:51-60`) ищет среди v1-коннектов запись
`Proto::Hysteria2 { token: t } if t == *token` (`src/memory/connection/proto.rs:17`).

Токены в память auth-сервиса приезжают синком из api: `POST /connections/sync {proto:Hysteria2,
env, topic:Init(node)}` (`src/bin/auth/http.rs:24-100`), плюс live-обновления по
`Topic::Auth` (`src/bin/auth/service.rs:99`, публикация — `handlers/connection.rs:466-478`).

То есть **per-user auth-контур существует и работает** — но его вселенная = «v1-коннект
с токеном». Клиент v1 получает `hysteria2://<token>@host:port` (`config/inbound.rs:690`), т.е.
пароль = UUID коннекта.

### 5.2 M1 «общий пароль» — что именно ломается

У hysteria2 **один** блок `auth`, и он взаимоисключающий: `password` **или** `userpass`
**или** `http` (`src/config/h2.rs:45-49` — `AuthConfig { type, password, url }`).

Следствия, если «просто поставить общий пароль»:

1. На ноде, где `type: http`, клиенты v1 перестают проходить auth: их токен больше не
   проверяется (нода вообще не ходит в api) ⇒ **все существующие v1-подписки h2 на этой ноде
   отваливаются**.
2. Пароль один на всех: утечка = все пользователи; отзыв = смена пароля у всех и рассылка
   нового конфига (в v2 это `payload.auth` в карточке — то есть отзыв = ре-релиз клиента).
3. Совместить нельзя: чтобы v1 и v2 жили на одной ноде, `auth` должен остаться `http`, а
   различать пользователей придётся **внутри** ответа `/auth`.

⇒ Общий пароль допустим **только** на выделенном h2-пуле, где v1-h2 выключен (или на отдельном
порту/инстансе hysteria, чего fnode сейчас не умеет — `[h2]` один на процесс,
`src/bin/node/config.rs:135-139`, `memory/node.rs:416-429`). Это и есть главный риск M1.

### 5.3 Рекомендуемая модель (переиспользует живой контур)

**Секрет = UUID на (subscription, env, proto)**, хранится в `peer_credentials.subject`,
отдаётся клиенту в зашифрованном AGW-ответе (`/v1/locations` или `/v1/authorize`), клиент
подставляет его как `password` hysteria2. Нода **не меняется**: она продолжает спрашивать
`https://api.frkn.org/auth`.

Меняется только вселенная токенов в auth-сервисе:

```rust
// src/bin/auth/service.rs — рядом с Connections
pub grants: Arc<RwLock<HashMap<Uuid, GrantToken>>>,   // subject → (subscription_id, env, expires_at)
// handlers.rs:9-31
if let Some(id) = mem.validate_token(&req.auth).or_else(|| grants.validate(&req.auth)) { … }
```

Наполнение — два варианта, оба дешёвые:

- **push**: api публикует `Topic::Auth` (уже есть, `zmq/topic.rs:17`) сообщением нового вида
  `{action:Create, subject, subscription_id, env, expires_at}`; auth-сервис слушает этот топик
  (`service.rs:99`) и держит `grants`;
- **pull**: `POST /v1/auth/tokens` (Bearer, как `/connections/sync`) — auth-сервис
  синкается на старте и раз в N минут (цикл уже есть: `src/bin/auth/service.rs:186-215`).

Отзыв/истечение: `expire_grants` (`src/bin/api/tasks.rs:627`, причины `expired`/`quota`,
`peers/mod.rs:342-345`) обязан публиковать `Delete`/`Revoke` по `subject`; `DELETE /v1/access`
и удаление подписки — тоже. Без этого токен живёт до рестарта auth-сервиса.

Почему не `subscription_id` как пароль: `subscription_id` — capability-хендл, он уже ходит в
теле каждого AGW-запроса и попадает в поддержку/скриншоты; отозвать его нельзя, не сломав
подписку целиком. Отдельный `subject` ревоцируется точечно и меняется при переиздании.

### 5.4 VLESS

- UUID выдаётся на `(subscription, env)` (M3 — на `(subscription, env, device)`), кладётся в
  `peer_credentials.subject`, уходит в `payload.uuid`.
- На ноде юзер добавляется тем же путём, что v1: `handler_client.create(conn_id, tag, None)`
  (`src/bin/node/tasks.rs:474-489`) — это xray proxyman `AddUser` по gRPC, без рестарта ядра.
  Для v2 нужен новый `apply_client_config`/`drop_client_config` (N1) + сообщение (N2) +
  init-пуш (N3).
- Reality-параметры (`pbk`, `sid`, `sni`, `path`, `flow`) api берёт из
  `inbound.stream_settings` и кладёт **в payload ноды** (§3.2).
- Отзыв = `RemoveUser` + `peer_credentials.revoked_at`.

### 5.5 Что ломается, если оставить «как v1»

Соблазн: «пусть карточка `client_config` просто переиспользует v1-коннект» (`grants_to_conns_map`
уже это делает, `access.rs:30-52`; `docs/decommission-v1.md:7-9` прямо это и планирует).
Тогда api на выбор карточки минтит v1-коннект и отдаёт готовый `hysteria2://` / `vless://` линк.
Минусы, которые надо принять осознанно:

- один коннект на `(subscription, env, proto)` — это ровно тот же per-subscription секрет, но
  он живёт в `connections` и попадает под `prune_legacy_conns` (`api-example.toml:92-97`) и под
  `connections_to_prune` (`access.rs:80-…`) ⇒ карточка может «моргнуть» после уборки;
- лимит устройств и per-device cap для него не работают;
- при уходе v1 (`decommission-v1.md`) коннекты остаются навсегда именно из-за h2/vless —
  то есть v2 навсегда тащит v1-таблицу.

Рекомендация: **не** строить v2 на `connections`. Использовать `proto/conn.rs` только как
рендерер линка для `/sub` и как источник формы outbound-а; креды — свои (§5.3).

---

## 6. Milestones, стоимость, риски

Оценки — человеко-дни одного разработчика, знакомого с обоими репозиториями, без ревью и без
выкатки. Клиентские дни включают проверку на macOS; iOS/Android-проверка — отдельно.

### M0 — контракт и совместимость (1 д)

- `kind` (+`#[serde(default)]`) и `payload` в `PoolCard`/`PoolNode`, всегда `"peer"` по
  умолчанию; клиент парсит и игнорирует (C1, C5).
- Тест: старый ответ против нового клиента и наоборот. `docs/api-peers.md` — новая секция.
- Риск: почти нулевой. Это единственный шаг, который можно катить до всего остального.

### M1 — hysteria2 в v2, общий пароль (2.5-3 д)

- A1-A6 (CardProto + карточки h2), A10 (флаг `card_protos`), C3-C4, C6-C8 (h2-ветка),
  C9-C10.
- Ноды: выделенный h2-пул с `auth: {type: password, password: …}` + тот же пароль в
  `payload.auth` (конфиг пула задаётся оператором, `H2Settings` его не несёт — `config/h2.rs:73-83`).
- **Что ломается:** v1-h2 на этих нодах (§5.2). Поэтому — только пул, где v1-h2 уже не нужен.
- Риски: (1) `access.rs:46` — грант с `Hysteria2` уже минтит v1-коннект; пока A9 не решён,
  пользователь получит и карточку, и коннект; (2) нет метрик h2 (`metrics.rs:66`) ⇒ квоты не
  считать; (3) `PeerConfigBuilder` — единственный генератор, ошибка в outbound-е = «Connected, но
  нет трафика» (ровно этот класс багов уже ловили на junk-параметрах,
  `peersImportController.cpp:143-150`).
- **Стоит ли:** как проверка сквозного пути (card → connect → трафик) — да; как продукт — нет.

### M2 — per-subscription auth для hysteria2 (4-5 д)

- A8 (`/v1/authorize` или секрет в `/v1/locations`), A11-A13, D3, D5, C6 (брать секрет из
  ответа). Нода не меняется (остаётся `type: http`).
- Плюс: пуш/синк токенов в auth-сервис + revoke по `expire_grants`/`DELETE /v1/access`.
- Риски: (1) auth-сервис — одна точка на все h2-ноды: падение/рестарт = отказ в auth новым
  сессиям (существующие QUIC-сессии живут); (2) рассинхрон кэша = «оплатил, не подключается» —
  нужен TTL и лог `auth: unknown subject`; (3) `validate_token` — горячий путь, второй `HashMap`
  дёшев, но нужен тест на 10^5 записей; (4) **лимит устройств для h2 невозможен** (нет pubkey):
  кап остаётся per-subscription.
- После M2 общий пароль можно выпилить, а v1-h2 на тех нодах — вернуть.

### M3 — VLESS (6-8 д)

- A1-A7 (vless-ветки), N1-N3 (AddUser/RemoveUser + init-пуш + новое сообщение), D3, C3-C8
  (vless-ветка outbound-а: `vnext`/`reality`/`xhttp`), C11-C12 (проверить `isVlessCdnRow` и пинг).
- Плюс N5 (метрика по `user>>>{uuid}`) иначе квоты нет и для vless.
- Риски: (1) **понодные reality-ключи** — карточка обязана нести payload на ноду, иначе
  «работает только на первой ноде» (§3.2); (2) `pool_inbounds_conflict` сравнивает
  `stream_settings` **целиком** (`request.rs:389-393`), а `StreamSettings` сериализует
  `realitySettings` вместе с `privateKey`/`publicKey` (`config/inbound.rs:26-38`, `47-57`).
  Две ноды одного пула с разными reality-ключами (а они всегда разные — генерируются на ноде)
  получат `409 pool_inbounds_conflict` (`request.rs:405-470`) и не войдут в ростер. Для M3 это
  обязательный фикс: сравнивать `serverNames`/`shortIds`/`xhttpSettings`, но **исключить**
  `realitySettings.privateKey/publicKey` из `client_inbound_fields` — иначе VLESS-пул из двух
  нод физически невозможен; (3) `AddUser` без `RemoveUser` при удалении устройства = юзеры-сироты
  в xray; (4) CDN-вариант (`VlessXhttpCdn`) требует внешнего CDN-домена — это отдельная
  инфра-задача, не код.

### Порядок выката

1. Клиент с поддержкой `kind`/`payload` и h2/vless-конфигов — **в стор**, пиры юзерам ещё не
   включены (`docs/db-cleanup.md:26`; `[service.peers] enabled = false`,
   `api-example.toml:73`).
2. M0-A10 на api: `card_protos = ["amnezia_peer","wireguard_peer"]` — поведение не меняется.
3. M2 (auth) **до** включения h2-карточек: иначе на проде останется общий пароль.
4. Включение h2-карточек на одном шарде (canary) → метрики/жалобы → расширение.
5. VLESS — после того как `pool_inbounds_conflict` перестанет ругаться на разные
   `privateKey` в одном пуле.

### Сводка

| веха | дни | что даёт | главный риск |
|---|---|---|---|
| M0 контракт | 1 | совместимость, `kind`/`payload` | нет |
| M1 h2 + общий пароль | 2.5-3 | сквозной путь доказан | ломает v1-h2 на ноде (§5.2) |
| M2 h2 per-subscription auth | 4-5 | продукт: индивидуальный секрет, отзыв | auth-сервис = SPOF; кап устройств невозможен |
| M3 VLESS | 6-8 | второй протокол в тех же пилюлях | понодные reality-ключи + `pool_inbounds_conflict` |
| **итого** | **13.5-17** | | + ревью/QA/выкат ≈ 3-4 д |

---

## 7. Приёмочные проверки (чтобы не «на глаз»)

1. `POST /v1/locations` для подписки с грантом `["amnezia_peer","wireguard_peer","hysteria2"]`
   и пулом из двух h2-нод → **одна** карточка `kind:"client_config"` с **двумя** `nodes`, у
   каждой свой `endpoint` и `payload`; `access[0].protos` содержит `hysteria2`.
2. Старый клиент (без `kind`) на этом же ответе: строка строится, коннект падает с понятной
   ошибкой **не** ломая остальные строки (сегодня — «card node without endpoint or server key»,
   `peersImportController.cpp:487-492`); поэтому в проде — только с A10-флагом.
3. Смена протокола на пилюле h2 → `setPeerPoolProtocol` → следующая попытка коннекта не зовёт
   `POST /v1/peers` (проверить по логу `[PEERS] call …`, `peersController.cpp:117-120`).
4. Failover: выключить первую ноду в карточке → коннект на вторую пересобирает конфиг
   (`retargetPeerConfig`, C9), трафик идёт.
5. Auth: токен отозван (`DELETE /v1/access`) → новая h2-сессия получает `{ok:false}` не позже
   чем через один цикл синка; существующая не рвётся (ожидаемо).
6. Метрика: после M3 в `peer_traffic` появляются строки по `credential_id`, и
   `subscription_traffic` их суммирует (`0020:174-181`).
