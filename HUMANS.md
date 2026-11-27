# HUMANS.md

## Current
- **27.11.2026 — раскладка новых подписок по шардам: `growth` вместо «всё в новый».**
  Вопрос владельца: «сейчас ввожу 2й шард — новые юзеры будут на него все литься пока они не
  сравняются?» Да, лились бы: `pick_shard` брал шард с минимальной долей `used/limit`, поэтому
  пустой `0002` забирал 100% регистраций, пока не догонял `0001` (324/500), а 176 свободных
  слотов `0001` простаивали.
  **Сделано:**
  1. `[service.peers] balancing` — `growth` (дефолт) / `equalize` (старое поведение). Разбор в
     `peers::Balance` (`src/bin/api/peers/mod.rs`), поле в `PeersConfig`, проброс через
     `PgContext::init(.., balance)` → `PgPeers.balance`.
  2. `choose_shard()` — правило вынесено из `pick_shard` в чистую функцию (тестируется без БД):
     в `growth` шард допускается к юзеру, только пока его доля потока за час меньше его доли
     капа (`max_subscriptions` / сумма капов плана); `+1` — на юзера, которого сейчас сажаем.
     Если все шарды выбрали свою долю — WARN и посадка на наименее заполненный (софт-лимит цел).
  3. `pick_shard` грузит `used` одним запросом (`unnest` + LEFT JOIN) вместо запроса на шард;
     отдельным запросом — поток за час по `subscription_access.created_at`.
  4. Overflow-ветка (все шарды полны) сравнивала **абсолютное** число грантов и потому
     переливала самый маленький шард — теперь по доле.
  5. Логи: `shard picked` (env, fill, mode) и `shard is at its growth share` на каждый пропуск.
  **Тесты:** `cargo test --bin api` — **363 ok** (+7 в `postgres::peers::balance_tests`:
  growth делит поток при 324/500 vs 0, одиночный шард не троттлится, «все сверх доли» всё равно
  сажает, soft cap и overflow по доле, вес = кап, uncapped → равные доли, пустой список → None).
  Контракт — `docs/api-peers.md` §«Как план раскладывается по шардам».
  **Важно:** при вводе `0002` доли НЕ выравниваются — `0001` доходит до капа, когда `0002` на
  ~35%, дальше всё идёт в `0002`. Кому нужны равные проценты — `balancing = "equalize"`.
  **Не сделано (шаг 2):** кап всё ещё считается по подпискам и задаётся руками; шард из 3 нод и
  из 7 нод с одинаковым `max_subscriptions` выглядят равными. Логировать причины выпадения шарда
  из кандидатов (`live_shards`: нет нод / нет блока / disabled) тоже пока не стали.
- **27.11.2026 — личный шард `sfNNNN` для own-node аккаунтов, грант на шард, пул = нода.**
  Владелец: «мы можем давать гранты на шард — юзер поднимает ноду, ему даётся шард личный и он
  туда добавляет ноды; одна нода один пул для sf-нод; по пулам гранты давать не умеем; это чтоб
  нода юзера была только его и он давал кому хочет». Старые 22 env не трогаем — отдельной задачей.
  **Сделано (код готов, не собрано, не выкачено):**
  1. `Env::selfhosted(n)` → `Env::Named("sf0001")` (без `custom`), `is_personal()` принимает обе
     формы (`sf*` и legacy `custompersonal*`), `selfhosted_number()` даёт номер или `None`.
  2. Последовательность `selfhosted_shard_seq` (в `ensure_schema`, идемпотентно) +
     `next_selfhosted_shard()`; номер выдаётся в `POST /v1/private/account` и в
     `ensure_personal_scope` (первый install-токен). Не переиспользуется: имя живёт в
     `env_capacity`, `env_networks` и на машине юзера.
  3. Владелец — по `subscriptions.scope_env`, а не по парсингу uuid: `owns_env` (оба impl),
     `ScopeKind::from_env` (Named `sf*` = personal), `refresh_known_private_node`,
     `set_private_feed`/`delete_private_node`, `create_private_user`, алерты (`shard_owners`
     в `tasks.rs` вместо `personal_owner(env)`).
  4. Пул: `private_pool` не изобретает имя, но на нумерованном шарде нода без `pool` получает
     в пул свой `hostname` — одна нода = один пул, и каждая получает только свои пиры.
     В `pool_cards` фолбэк `"default"` заменён на hostname для `sf*` (legacy шарды остались
     на `default` — их не трогаем).
  5. Усыновление legacy: аккаунт без `scope_env`, у которого уже есть ноды/грант в
     `custompersonal<uuid>`, получает этот env, а не новый `sfNNNN` (иначе машины осиротели бы).
  6. Грант по-прежнему на шард (`upsert_grant(sub, node.env, tags)` в `seed_private_shard`) —
     это и было нужно, менять не пришлось.
  **Тесты:** `--lib` 132 ok (+1 `test_selfhosted_shard_names`), `--bin api` 356 ok
  (+1 `numbered_shard_gives_every_node_its_own_card`: две ноды без пула → две карточки, ни одной
  `default`). Контракт — `docs/api-peers.md` §«Личный шард own-node аккаунта».
  **Выкачено 27.11 04:19 MSK:** `v0.7.53`, собран на build01 (`--bin api --no-default-features`,
  24м54с), 21 073 424 Б, sha256 `7e4a809f5047167c…`; GitHub release `v0.7.53` — тег на коммите
  `272ec80` (первый заход `gh release create` поставил тег на `main`/0.6.142 — переделано через
  `--verify-tag`, теперь `ls-remote` даёт `272ec80`), ассет `api-x86_64` совпал по sha с
  выкаченным бинарём; бэкап `/opt/api/api.bak-0.7.52-20261127-041853`, `systemctl restart api` →
  `active`, `NRestarts=0`, `/healthcheck` → `v0.7.53`, 0 `ERROR`/`panic` в логе за старт.
  **Приёмка живая (прод, `POST /private/account`):** аккаунт `08d492f9…` создан →
  `scope_env = sf0001`, `plane = v2`, `v2_since` заполнен, `env_capacity` = `sf0001|selfhosted|
  selfhosted|t`, последовательность `selfhosted_shard_seq` = 1. Install-токен →
  `scope_env: sf0001`, `env_block: 10.134.0.0/16`. Регистрация ноды `test-sf-node` →
  `nodes.pool = test-sf-node` (нода без пула стала своим пулом), грант
  `subscription_access = sf0001 | {amnezia_peer} | {install}` без срока — **грант на шард,
  не на пул**. `/private/nodes` и `/private/state` отвечают 200 для sf-аккаунта.
  **Тестовый аккаунт удалён** (нода через `DELETE /private/nodes`, подписка через
  `DELETE /admin/api/subscriptions`, затем `env_capacity`/`env_networks`/`install_tokens`/
  `subscription_access` по `sf0001`, `setval('selfhosted_shard_seq',1,false)`) — в БД ноль
  `sf%`-строк, первый реальный клиент получит `sf0001`.
  **Остаток:** ЛК (`frkn.org/subscription/index.html:6227`) сортирует карточки по префиксу
  `custompersonal` — sf-карточки попадут в общую группу вместо «своих»; косметика, правка в
  репо сайта, не делал (там чужая незакоммиченная работа).
- **27.11.2026 — шард объявляет свои протоколы сам (грант перестал врать).**
  Владелец: «в шард завожу новый пул FL hysteria2 и не должен прописывать, какие теги есть —
  минтить должно туда, куда у юзера есть доступ».
  **Что было не так:** `key.rs` склеивал протоколы по всем env сразу
  (`catalog_map.values().flatten()`), и в пир-шард `0001` уезжал полный список
  `dev`+`wl`+`ru` — `vless_*`, `hysteria2`, `wireguard`, `amnezia_wg`. Грант обещал карточки,
  которые не из чего собрать, а v1-плоскость минтила конфиги, на которые не отвечает ни одна
  нода. UPSERT объединяет протоколы, поэтому мусор не рассасывался никогда.
  **Сделано:**
  1. `announced_inbounds()` (`handlers/peers.rs`) — карта `env → {tags}` по инбаундам
     **serving**-нод. Это и есть список протоколов шарда, настраивать его негде.
  2. `filter_protos_for_env()` (`postgres/peers.rs`) — сужает грант до анонсированного;
     вызывается из `assign_grant` и `fulfil_plan_grant` (оба получили параметр `announced`).
     **Fail-open** дважды: пустая карта (снапшот не загружен) и env без serving-нод —
     временные состояния, срезать грант в них значит отнять права у платящего.
  3. `ensure_granted_creds` больше не перебирает захардкоженную пару
     `[Hysteria2, VlessXhttpCdn]`, а идёт по `grant.protos`, отфильтрованным через
     `CardProto::from_tag(..).kind() == ClientConfig`. Новый прокси-протокол начинает
     минтиться без правки кода — достаточно, чтобы нода объявила инбаунд.
  **Проверка:** `cargo check` — 0 ошибок; `cargo test --bin api --no-default-features` —
  **355 passed** (350 + 5 новых в `postgres::peers::grant_filter_tests`: пир-шард режет
  v1-протоколы, новый анонс выживает, пустой меш / env без нод / полное несовпадение —
  fail-open). Контракт описан в `docs/api-peers.md` §«Протоколы шарда объявляет он сам».
  **Выкачено 27.11 02:45 MSK:** `v0.7.52`, собран на build01 (`--bin api --no-default-features`,
  24м 45с, 0 ошибок), артефакт 21 051 760 Б, sha256 `1fa18043132e532e1403b1cef8cf16eb…`,
  GitHub release `v0.7.52` (digest совпал), бэкап прода `/opt/api/api.bak-0.7.51-20261127-023928`,
  `systemctl restart api` → `active`, `NRestarts=0`, в логе 0 `ERROR`/`panic`.
  **Приёмка живая:** новый ключ → `/v1/activate` → грант `{amnezia_peer,wireguard_peer}`
  (было 9 протоколов), 0 v1-коннектов (шард их и не обслуживает), `/v1/locations` — 4 peer-карточки;
  AWG-клиент на `sh-0001-nl03` — handshake 6 с, `/healthcheck` через туннель `200 (v0.7.52)`,
  exit IP `132.243.203.45`. h2-карточки не сломались: `premium0001:prem:hysteria2` и
  `custompremium-a65e52c6af0e:default:hysteria2` отдаются с payload как раньше.
  **Остаток:** 12 из 324 грантов `0001` всё ещё с мусором (созданы до фикса, UPSERT только
  объединяет) — фильтр их не чистит, нужна перезапись `protos`. Вреда нет: карточки строятся
  по инбаундам нод, поэтому лишние теги просто игнорируются.
- **26.11.2026 — 0.7.48 (всё дерево, коммит из этой сессии):** четыре независимых куска,
  собранные в один релиз по решению владельца.
  **Чистка БД:** таск `purge_expired_connections` (`[tasks] connection_purge_after_days`,
  по умолчанию 90, `0` выключает) жёстко удаляет коннекты подписок, истёкших раньше порога,
  вместе со строками `connection_traffic`, батчами по `connection_purge_max_per_tick` (2000),
  `pg.conn().purge_expired_before()` — один атомарный CTE-statement; id возвращаются, чтобы
  память успела их выкинуть раньше, чем персистер трафика вернёт строки. На проде под нож
  попадает 20 089 коннектов у 3 725 подписок (все уже soft-deleted) + их трафик (48 МБ).
  **Персональные шарды:** `PRIVATE_POOL = "own"` больше не штампуется — api хранит пул, который
  нода объявила (`[node] pool`, тримит, `400` на >64 символов и control-символы), нода без пула
  держит свою карточку, рефреш без пула не стирает сохранённый; `0034` одноразово чистит
  `pool='own'` у `custompersonal%` (0.7.47 схлопывала весь шард в одну карточку).
  **v2-карточки прокси:** `ensure_granted_creds` минтит коннект для `hysteria2`/`vless_xhttp_cdn`
  карточек (v2-клиент сам v1-коннекты не создаёт, поэтому карточки молча выпадали из
  `pool_cards`), публикует в auth-топик как `Vec<Message>` (с одиночным слайсом auth падает на
  `CheckBytes` и dial'ы получают 403). Плюс `traffic_gib` в покупке плана и миграция `0033`
  (`transaction_type = 'PLAN_PURCHASE'`, база платёжного шлюза).
  **Чистка пиров:** `reap_idle_peers`/`reap_stale_devices` больше не держат `PER_TICK = 200`
  в константе — размеры в конфиге, полная пачка означает бэклог и следующий проход идёт через
  `peer_reap_backlog_pause_sec` (5 с); `peer_idle_days` в доке 30 → 90.
  **Починено в этом же релизе:** тест `hysteria2_card_matches_the_contract` ждал `server` = TLS-домен,
  а код с 0.7.48 отдаёт в `server` входной IP ноды и домен только в `sni` (иначе QUIC-хендшейк
  уходит в туннель, который сам же и строится) — тест приведён к коду.
  **Проверка:** `cargo test --lib` 131 ok, `cargo test --bin api --no-default-features` 350 ok.
  **Выкачено на прод 26.11 20:15 MSK:** бинарь `sha256 42fe8483…48a22` (21 030 608 байт,
  `ELF x86-64`, собран на build01 за 31 мин), бэкап `/opt/api/api.prev-20261126-201504`,
  `/healthcheck` = `Ok (v0.7.48)`, `/status` 20 нод. Миграция `0034` применена — 4 ноды
  `custompersonal` с `pool='own'` обнулены (у `dbe10c74…` теперь 3 карточки вместо 1),
  бэкап `/root/bk_own_nodes_20261126-201504.csv`. **Чистка отработала на первом же тике:**
  `purge_expired_connections: hard-deleted 20109 connection(s) of subscriptions expired before
  2026-08-28 (90 days)` — `connections` 46 250 → 26 153, `connection_traffic` 185 734 → 178 067,
  `VACUUM (ANALYZE)` сделан. Активные не задеты: 885 подписок / 10 328 коннектов на месте,
  подписок с `expires_at > now()`, потерявших коннекты — 0.
  **Грабля миграций:** `deploy/migrate.sh` гоняет **все** файлы на каждом деплое с
  `ON_ERROR_STOP=1`, а `0033` бьёт `ALTER TYPE transaction_type`, который живёт только в базе
  `payment`. На базе `api` это уронило бы весь прогон до `0034`, поэтому `0033` обёрнута в
  `DO $$ … IF EXISTS (pg_type) $$` — вне платёжной базы это no-op. Сам enum в `payment` уже
  содержал `PLAN_PURCHASE`, платёжную базу не трогали.
- **24.11.2026 — промежуточный коммит `c9a3a1d` (НЕ запушен):** всё дерево 0.7.47 — v2 peers plane
  (`POST /v1/peers/prepare`, идентичность `(env, pool, public_key)`), `/20` на пул (0029–0032),
  dual plans (`plans.rs`, `POST /subscription/{id}/plan`), вложенный `report{}`, union трафика,
  plane v2, `own_nodes_limit`, `[[node.endpoints]]`, `recursion_limit 512`, карточки premium/selfhosted
  при `Offline`. Прод api — бинарь от 23.11.
- **Сборка/деплой:** только на **build01 45.9.13.213** (`/root/build/ffcore`,
  `cargo build --release --bin api --no-default-features`), проверка `file` → `ELF … x86-64`,
  затем scp в `/opt/api/api.new`, бэкап `api.prev-<ts>`, restart, healthcheck. Готовый бинарь
  0.7.47 (sha256 `2ce03da9…`) собран и проверен, **ждёт «ок» пользователя на выкатку**.
  Правило записано в `AGENTS.md`: не собирать на маке и не собирать на api-хосте.
- **Платежи:** `POST /subscription/{id}/plan` пока никем не вызывается — в платёжном шлюзе нет поля
  `plan`, нет `[[app.plans]] kind="lite"` в живом конфиге, вебхук зовёт `PUT /subscription` (дни).
  ЛК держит `PLAN_GATEWAY_READY=false`.
- **Repo:** ffcore
- **Branch:** `feature/peers` (запушено, теги `v0.7.7`–`v0.7.10`; HEAD `be5efbd`)
- **Task:** PEERS — новый слой пиров: ключ на устройстве, адрес от api, права по шардам
- **Статус 2026-11-22 (ночь+):** на проде **0.7.32** (`Ok (v0.7.32)`, `sha256 659681bc…318b0d7`,
  бэкап `/opt/api/api.bak-0.7.31-20261122-020816`) — **exit-health алерты почтой**:
  `POST /v1/admin/alerts/exit` + дедупликация по (группа, нода), watchdog'и `ru`/`ai`/`gov`
  и hourly-хук группы `youtube` на `sh-nl01`/`sh-nl02` отчитываются сервисным токеном,
  телеграм по-прежнему off. Проверено живьём убийством `wg-ru-ufo`/`wg-ru-aeza` на nl02 и
  `wg-kz0` на nl01: письма `exit_dead`/`exit_degraded`/`exit_recovered` дошли, 11–19 с на
  переход, ни одного повтора. Детали — ниже в `Done` и в `../env/…/routing.md` §8.8.
- **Task (в работе):** GEO-LISTS — публичные `/router/geo/*` для OpenWrt и нод: `ru.zone` (суточный ipdeny),
  `ai4`/`aicn4`/`exch4`/`adult4`/`gov4` — ежечасный резолвер доменов с TLS-проверкой каждого IP (группы `ai`, `ai-cn`,
  `exch`, `adult`, `gov` на нодах пула `eu`)
- **Статус 2026-11-22 (ночь):** `/router/geo/*` отдаёт **nginx**, не api — новая зона больше не требует релиза api.
  Обработчик `deploy/nginx/frkn-geo.js` (njs, пакет `libnginx-mod-http-js`) + сниппет `deploy/nginx/frkn-geo.conf`
  с whitelist; тело/`ETag "sha256:…"`/`Last-Modified`/`Cache-Control`/`304`/`HEAD`/`405` сверены с прежними ответами
  api байт-в-байт на всех семи зонах, `youtube4`/`gov4`/`adult4` впервые отдаются (api отдавал `405` — их не было
  в `ZONES` релиза). Отдача зон из api **снята в 0.7.31** (выкачен 22.11.2026): `handlers/geo.rs` удалён
  целиком, монтирование и `[service] geo_dir` сняты, тесты api 296 → 272. Детали —
  `docs/router-geo.md` § «Зоны отдаёт nginx» и § «Отдача зон снята из api (`0.7.31`)».
- **Статус 2026-11-21 (ночь):** api на проде **0.7.30** (`Ok (v0.7.30)`, `/var/lib/frkn/geo/` отдаёт все зоны),
  включая **`adult4.zone` 200** (`sha256 d0fa9863…4ec`, 82 `/32`, `.sha256`/`.count`/`ETag`/`304` сходятся с файлом).
  Ноды `sh-nl01`/`sh-nl02`: сет `adult4` (30 токенов) и правило `ip daddr @adult4 return` вторым в `mark_pre`/`mark_out`;
  adult-трафик пира выходит с IP ноды, RU/US/KZ-туннели не двигаются (`routing.md` §8.6).
- **Статус 2026-11-21 (вечер):** api выкачен **0.7.25** — обобщённый обработчик `ZONES` (`ru.zone` + `ai4.zone`), резолвер `deploy/geo/update-ai4-zone.py` + `frkn-geo-ai-update.timer` на хосте api, применение зоны на `sh-nl01`/`sh-nl02` (`frkn-geo-ai4.timer`, один `nft -f`), марка `0x3` → таблица 300 (пустая, TODO us-exit)
- **Status:** на проде **0.7.20** (`sha256 9a04d881…baa6`, бэкап `/opt/api/api.bak-0.7.19-20261120-222730`), ноды шарда — **0.7.20** с `[node] pool_country_code = "NL"`. Карточка отдаёт `country: NL` / `country_name: Netherlands` из объявления пула; `shard_overview` для `0001` — `pools: ["eu"]`, `peers_live`, `nodes: 2`. Миграции `0022`, `0024`, `0025`, `0026` накатаны, `0023` — нет.
- **Статус 0.7.17 (историч.):** на проде **0.7.17** (бинарь `sha256 56784a7e…04ed`, бэкап `/opt/api/api.bak-0.7.16-20261120-203359`), миграции `0022`, `0024`, `0025` накатаны, `0023` — нет. Шард `0001` проверен живьём: пир по `card_id` создан, применён в ядре на обеих нодах, хендшейк и ping гейтвея прошли с `ee.frkn.org` (nl01 29 мс, nl02 38 мс), удаление пира/устройства снимает его с нод. Шард оставлен пустым. Тесты: lib 109, api **251**.
- **Осталось:** выкатить `0.7.18` (коммит `75928e8`, только фикс записи реестра пулов — `pools` пока пуст, `shard_overview.pools = []`); клиент Dopamine — инкременты 4–6 (адаптер `PoolCard → ServersModel`, кэш конфигов, экран устройств, ключ в Keychain). Продление подписки больше не оставляет отозванный грант мёртвым (`renew_grants`), но **миграция `0028` и код ещё не на проде**.
- **Task (23.11):** NODE-ENDPOINTS — одна форма точки входа `[[node.endpoints]]`
  (ip/port/weight/label) в конфиге ноды, в анонсе в api и в пейлоаде клиенту; строго
  аддитивно (легаси `node_ips`/`node_weights` работают бит-в-бит), плюс идемпотентный
  скрипт миграции `../env/dev/scripts/migrate-node-endpoints.py` и счётчик
  `endpoints_legacy` в `/status`. Не задеплоено, не закоммичено.
- **Updated:** 2026-11-23

## Done (peers)
- 2026-11-23 — **`POST /v1/peers/prepare`: все peer-пулы гранта одним вызовом (0.7.40, не задеплоено).**
  Клиент сможет сохранить конфиги всех пулов при логине/импорте подписки и гонять спид-тест
  без первого подключения. Ручка в AGW-конверте, тело `{subscription_id, device_uid, public_key,
  device_name?, platform?, pools?: ["<shard>:<pool>"]}`; ответ `{prepared:[{card_id,pool,proto,
  address,peer_id,config}], skipped:[{card_id,reason}]}`, где `config` — та же карточка, что в
  `/v1/locations`. Правила: только peer-карточки (hysteria2 → `card_kind_not_peer`), идемпотентность
  по `(env, pool, public_key)`, per-(device,pool) лимит 3, подписного нет, `check_grant` на протокол,
  каждый пул — своя транзакция (`db`/`peers_limit`/… в `skipped`, остальные пишутся). Новый чистый
  планировщик `peers/prepare.rs` (+6 тестов: 2 пула/2 адреса, повтор без новых пиров, полный пул
  пропущен при живом втором, hysteria2, 15 пулов без саб-лимита, ungranted proto) + тест формы
  ответа и тест store на один ключ в разных пулах. Идемпотентность `(env, key)` → `(env, pool, key)`
  в памяти и БД (`0029_peers_pool_key.sql`, `ensure_schema`): иначе один ключ не мог жить в двух
  пулах. `cargo test --bin api` — **307 passed**. Без деплоя и коммита.
- 2026-11-22 — **Отдача зон снята из api: 0.7.31 на проде, `/router/geo/*` живёт только в nginx.** Сначала
  гейт: перед правкой кода nginx реально отдаёт зоны (`200`, `Server: nginx`, ETag `"sha256:…"` и
  `Content-Length` те же, `If-None-Match` → `304`, неизвестное имя → `404`, не-GET → `405`, все семь зон `200` —
  `youtube4`/`gov4`/`adult4` впервые). Удалено: `src/bin/api/http/handlers/geo.rs` целиком (1323 строки:
  `ZONES`, `ZoneSpec`, `MIN_*_LINES`, `validate_zone`, `if_none_match_hits`, `GeoAsset`, 24 теста),
  `pub(crate) mod geo;` в `handlers/mod.rs`, `geo_handlers::routes(...)` + `.or(geo_routes)` в `routes.rs`,
  `[service] geo_dir` + `default_geo_dir()` в `config.rs`, `geo_dir` из `api-example.toml`. `/healthcheck` и
  остальные маршруты не тронуты; резолверы, их таймеры и `/var/lib/frkn/geo/` не тронуты. Тесты на хосте
  сборки: **272 passed** (было 296; 24 теста ушли с файлом), `cargo +nightly build --release --bin api` —
  9m37s. Прод: `/opt/api/api` = **0.7.31** (`sha256 820fccf6…b9db`), бэкап `api.bak-0.7.28-20261122-005255`,
  `/healthcheck` → `Ok (v0.7.31)`, в журнале 0 ошибок после рестарта. Приёмка через nginx: все семь зон `200`,
  тело каждой зоны совпало с файлом на диске по sha256, `.sha256` и `.count` сошлись с `wc -l`, `304` по ETag,
  `HEAD` те же заголовки, неизвестное имя/`.zone.bak` → `404`, `POST/PUT/DELETE/PATCH` → `405`, `/status`,
  `/admin`, `/sub/xxxx` (404 по неизвестному токену), `/key/validate` (400 на пустые параметры) — живы.
  Прямой запрос в api на `/router/geo/ru.zone` → `405`, как на любой неизвестный GET-путь (`/foobar`), зону
  не отдаёт. Сборка велась из `/opt/build/src/fcore` (дерево синхронизировано точечно — только 4 изменённых
  файла и удаление `geo.rs`, чтобы не выкатить незакоммиченную работу соседней сессии). Доки:
  `docs/router-geo.md` (§ «Отдача зон снята из api (`0.7.31`)», § «Где лежат файлы», ошибки),
  `docs/API.md` (nginx, `geo_dir` → `GEO_DIR`). **Не сделано:** git-коммит/тег (дерево незакоммичено).
- 2026-11-22 — **`/router/geo/*` отдаёт nginx: новая зона больше не требует релиза api.** Владелец: отдача зон — не
  путь в коде (каждая новая зона = правка `ZONES` → сборка → релиз api, а билд-хост уходит в OOM). Сделано:
  `deploy/nginx/frkn-geo.js` — njs-обработчик (`libnginx-mod-http-js` 0.8.2 на Ubuntu 24.04), читает
  `/var/lib/frkn/geo/<zone>.zone` и отдаёт те же три ресурса, что `handlers/geo.rs`: тело байт-в-байт,
  `ETag "sha256:<хэш файла>"` (не native nginx — `frkn-geo-ai4-refresh.sh` на нодах сверяет ETag с sha256 тела и
  иначе **не применяет зону**), `Last-Modified` = mtime зоны (и для `.sha256`/`.count`), `.count` считается из зоны
  (у `ru` файла `.count` нет), `304` по `If-None-Match` (одиночный/список/`W/`/`*`) с тем же набором заголовков,
  `HEAD` с `Content-Length`, `405` на не-GET/HEAD, `503` на пустой/обрезанный/ниже порога файл.
  `deploy/nginx/frkn-geo.conf` — `location ~ ^/router/geo/(ru|ai4|aicn4|exch4|youtube4|gov4|adult4)\.zone(\.sha256|\.count)?$`
  (whitelist) + `location /router/geo/ { return 404; }`, включён строкой в server `api.frkn.org` выше `location /`.
  Проверка на проде 22.11.2026: все 7 зон — тело/длина/`Content-Type`/`ETag`/`Last-Modified`/`Cache-Control`/`304`/`HEAD`/`405`
  совпали с прежними ответами api (12 ресурсов, что api отдавал, — байт-в-байт; `youtube4`/`gov4`/`adult4` были `405`,
  теперь `200` из своих файлов), `.sha256` = sha256 тела, `.count` = `wc -l`, `gzip` не включается, API не
  перезапускался (`api.service` не тронут), его обработчик доступен на `127.0.0.1:3000` как фолбэк. Отличие:
  неизвестное имя — `404` вместо `405` (артефакт api). Бэкап конфигов — `/root/nginx-backup-20261122-002519/`.
- 2026-11-21 — **0.7.28 на проде: `<zone>.count` отдаётся api; группа `exch` проверена живьём на обоих входах.** Обработчик гео-листов получил третий ресурс на ту же зону: `GET/HEAD /router/geo/<zone>.count` (`GeoAsset::Count`, поле `count` в `Snapshot`, пишется из `validate_zone`) — один и тот же strong `ETag`/`Last-Modified`, `304` по `If-None-Match`, `503` на файл, не прошедший форму/порог (счётчик невалидного списка не отдаётся). Работает для всех зон `ZONES`: `exch4` **180**, `aicn4` 40, `ai4` 95, `ru.zone` 8664 — совпадает с `wc -l` самих зон. Тесты на дереве (побайтно равно рабочему): `cargo test --release --bin api --no-default-features` — **294 passed**, geo 22, включая новые ассерты `.count` (`routes_serve_the_exch4_zone_with_the_same_semantics` проверяет тело, тот же ETag и `503` на обрезанном файле; `resource_names_parse_into_zone_and_asset` — `exch4.zone.count`, `ai4.count`, `.count`). Прод: `/opt/api/api` = **0.7.28** (`sha256 41d314a4…`, `--version` → `>>> API Service 0.7.28`), `/healthcheck` → `Ok (v0.7.28)`, бэкапы `api.bak-0.7.26`, `api.bak-0.7.27`. Приёмка 0.7.27/0.7.28 (`api.frkn.org/router/geo/exch4.zone`): 200, `ETag "sha256:…"`, `If-None-Match` → 304, `.sha256` совпадает с пересчитанным хэшем, неизвестное имя и `POST` → 405, `ru/ai4/aicn4/exch4` — 200. Ноды (обе): `frkn-geo-exch4-refresh.sh` → `unchanged (304), set keeps 180 elements`, `/etc/frkn/geo/exch4.zone` = опубликованная (sha `758d203c…`), в ядре 174 токена (`auto-merge` склеил соседние `/32`). **Живое доказательство маршрута (netns-пир `10.88.9x.2` + veth, тот же `mark_pre`, что у клиента):** `www.okx.com` 200 (0.52 с) и `www.binance.com` 202 (0.28 с) на nl01 и nl02, `api.binance.com/api/v3/ping` 200; адрес-отражатель `104.16.184.241`, временно добавленный в `@exch4`, выходит с **171.22.134.156**, тот же адрес без элемента — с NL (`176.124.198.154` nl02 / `132.243.203.40` nl01); элемент снят, сет переприменён из файла, netns удалены. Вердикты не поехали: `aicn4` → `net0` без марки, `ai4` → `wg-us0`/300, `ru4`/`exch4` → `wg-ru-ufo`/200, `youtube4` (nl01) → `wg-kz0`/600. **Сборка:** `build.sh`-путь недоступен (дерево не в теге), собирали из scratch-копии на `us.frkn.org`; полный LTO дважды падал — ICE rustc и два OOM-kill'а (`rustc` 1.7–2.1 GB anon при 5.9 GB RAM и параллельных чужих сборках), рабочий вариант — `CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 -j1` (17m54s, бинарь 25.8 MB против 20.5 MB у полного LTO). **Не сделано:** тег/`gh release` (дерево незакоммичено, `HEAD` 0.7.24; `Cargo.toml` 0.7.28).
- 2026-11-21 — **Группа `adult`: direct whitelist, adult-сайты всегда через NL (api 0.7.30).** Владелец: adult-трафик обязан
  выходить с публичного IPv4 NL-входа ноды и никогда через RU/US/KZ (в NL рабочий правовой режим без возрастной верификации в стиле
  UK/US; RU-выход подставил бы пользователей под российское законодательство, а `ru4` — большой агрегированный список, который
  ловит hosting-адреса этих сайтов случайно). Это **второй direct whitelist** (после `aicn4`): ни марки, ни `ip rule`, ни таблицы,
  ни выхода, ни watchdog'а — только nft-сет `adult4` и `ip daddr @adult4 return` **вторым**, сразу после `@aicn4` и выше
  `@exch4`/`@ai4`/`@ru4`/`@youtube4`, в обеих цепочках. Источник — `env/.../domains/adult.txt` (24 домена: 19 обязательных платформ
  + image/media CDN `ei/ev.phncdn.com`, `cdn77.xvideos.com`, `xhcdn.com`, `static.xhcdn.com`; проверено, что каждый проходит TLS/SNI).
  Резолвер: `update-ai4-zone.py --group adult4 --domains …/adult.txt --zone-name adult4.zone` + юнит/таймер
  `frkn-geo-adult4-update.{service,timer}` (ежечасно + до 5 мин); первый прогон — 24 домена, 36 имён, 162 кандидата, **98 `/32`**
  (`sha256 2e0a6295…5d3790`), через час таймер перегенерировал до **82 `/32`** (CDN ротирует) и api отдаёт именно её
  (`d0fa9863…4ec`), `ETag "sha256:…"`/`304`/`.count` сходятся с файлом. В `ZONES` — `adult4.zone` + `MIN_ADULT4_LINES = 8`,
  parity-тест `routes_serve_the_adult4_zone_with_the_same_semantics` (заголовки, ETag, 304, HEAD, `.sha256`, `.count`, порог);
  `cargo test --bin api` — **296 passed**. Ноды: `frkn-geo-adult4-refresh.sh` (обёртка над общим движком) + `frkn-geo-adult4.{service,timer}`,
  сет и оба `return` в `frkn-msk-route.sh`, засев из `/etc/frkn/geo/adult4.zone` при пересоздании таблицы; оба узла подтянули зону
  с api (0 → 30 токенов после auto-merge). **Живая приёмка (netns-пир = forwarded путь пира, обе ноды):** `www.pornhub.com`
  (`66.254.114.41`, в зоне) — **200** за 0.25 с, exit IP = IP ноды (`132.243.203.40` / `176.124.198.154`), дельты `wg-ru-ufo`,
  `wg-ru-aeza`, `wg-us0`, `wg-us1`, `wg-kz0` — 0 байт; `ip route get <ip> mark 0x2` = `dev wg-ru-ufo table 200` (марка реально
  доступна правилу); при временном добавлении адреса в `@ru4` ответ всё равно 200 и туннели 0, а **контроль без членства в `adult4`**
  уходит в RU-туннель (+2016 байт на nl01, +3360 на nl02) и отвечает **301** (гео-редирект) — то есть возврат срабатывает именно
  благодаря `@adult4 return`. После теста `ru4` (3596 токенов) и `adult4` переприменены из зон, временных элементов нет. Не сломано:
  `ya.ru` из netns 302 через `wg-ru-ufo` (+1920 Б), `claude.ai` mark 0x3 → `wg-us0` table 300, `exch4` → table 200, правило
  `youtube4` на nl01, гарды таблиц 200/300/600 на месте, bench `200 1048576`, `awg0-peer` 8 пиров с живыми handshake,
  торрент-счётчики растут (`info_hash` 3→4, `BitTorrent protocol` 26→27). Доки: `docs/router-geo.md`, `docs/API.md`,
  `env/.../routing.md` §8.6, `overlay.md`, `domains/README.md`, `env/.../README.md`, скрипт приёмки `adult4-e2e.sh`.
  **Прод 0.7.30 собран и выкачен параллельной сессией из того же дерева** (в бинаре есть `adult4.zone`/`gov4.zone`, sha
  `/opt/api/api` = `8e5d992a…6e`); независимая сборка того же дерева на хосте api — `0.7.30`, sha `2f59576e…9bd`. Бэкап
  `/opt/api/api.bak-0.7.27-20261122-0001`. **Не сделано:** git-коммит/тег (дерево незакоммичено, `HEAD` 0.7.24).
- 2026-11-21 — **Группа `gov`: гос-сайты отдельным одноцелевым выходом (`gov4.zone`, api 0.7.30).** Пятая зона того же конвейера: домены `domains/gov.txt` в `env` (14 доменов: Госуслуги/ЕСИА, Почта, `mos.ru`, ФНС, СФР, суды, Минтруд/Роструд, Росгвардия) → резолвер с TLS/SNI-проверкой → `gov4.zone` (`.sha256`, `.count`) → `/router/geo/gov4.zone`. В `ZONES` добавлена строка `gov4.zone` + `MIN_GOV4_LINES = 8`, тесты — полный набор тех же гарантий, что у остальных зон (`routes_serve_the_gov4_zone_with_the_same_semantics`: заголовки/ETag/`304`/`HEAD`/`.sha256`/`.count`, свой порог, отказ `503` на обрезанном файле; плюс `gov4` в списках 404/unknown/truncated/POST и в `only_registered_zones_are_served`). Зачем отдельная зона: общие RU-выходы (UFO/Aeza) WAF-зафлагены для гос-сайтов — замер 21.11.2026 `pochta.ru` 417 за 0.5 с, `mos.ru` чёрная дыра 20 с — а старый `msk-tw` открывает их за десятые доли секунды, поэтому ноды получили группу `gov` (марка `0x7`, таблица 700, один выход, fail-closed). Приёмка: сет `gov4` на обеих нодах, `ip daddr @gov4 meta mark set 0x7 return` **выше всех групп**, exit IP `217.149.29.118` из netns-пира на всех четырёх сайтах, дельты RU/US/KZ-туннелей 0; остановка трубы — блок за 0.2 мс и восстановление по handshake. Доки: `docs/router-geo.md`, `docs/API.md`, `env/.../routing.md` §8.7.
- 2026-11-21 — **Группа `exch`: биржи → RU-выход; 0.7.27 на проде, сети на обеих нодах, порядок правил исправлен.** `exch4.zone` (26 доменов, 186 `/32` в текущей генерации) отдаётся api **200** (`api.frkn.org/router/geo/exch4.zone`, `.sha256`, `ETag`/`304`; неизвестный путь и `POST` — 405 — не сломаны, `ru/ai4/aicn4` тоже 200), бинарь на проде `/opt/api/api` = **0.7.27**, `/healthcheck` → `Ok (v0.7.27)`, бэкапы `api.bak-0.7.26`, `api.bak-0.7.25-…`. Сборка: `cargo +nightly build --release --bin api --no-default-features` из дерева, побайтно совпадающего с рабочим (`sha src/`), тесты на том же дереве — **294 passed**. Ноды `sh-nl01`/`sh-nl02`: `frkn-geo-exch4.service` применяет зону, сет `exch4` = **180 токенов** (186 `/32` после auto-merge) на обеих, `/etc/frkn/geo/exch4.zone` + `.etag` + `.count` совпадают с api по sha256. **Два дефекта найдены и закрыты:** (1) правило `ip daddr @exch4 meta mark set 0x2` без **`return`** — `meta mark set` не прекращает разбор цепочки, поэтому общий CDN-адрес, попавший и в `exch4`, и в `ai4` (сегодня `99.84.91.11`), получал марку `0x3` и уходил в US-группу: замер на netns-пире до правки — AI +2.5 КБ / RU 0, после — `wg-ru-ufo` +2.3 КБ / AI 0; `return` добавлен в обе цепочки; (2) `frkn-geo-ai4-refresh.sh` не мог засеять **пустой** сет: `live_count()` = `nft | grep | wc -l` под `set -euo pipefail` падал на `grep` (exit 1) → сервис выходил 1, и первая установка зоны на ноду без `/etc/frkn/geo/exch4.zone` была невозможна; добавлен `|| true` (и в `kernel_count`), после чего `0 -> 180`. Приёмка (netns-пир, forwarded путь как у реального пира): реальный запрос `https://api.binance.com/api/v3/ping` из netns — **200** за 0.5 с, tx дельта только по RU-туннелю, AI 0; адрес внутри `@exch4`, временно добавленный, выходит с **171.22.134.156** (RU, UFO Москва) — на `sh-nl02` то же; временные элементы сняты, зоны переприменены из файла, netns удалён. Fail-closed проверен на живом: пока новые RU-выходы не поднялись, таблица 200 = только гард, и ответ для marked-трафика — ICMP unreachable (никогда не NL). Watchdog `ai`: изолированный прогон в таблице 399 на обеих нодах — healthy → failover за 2 тика → **только гард** (blocked) → возврат за 2 тика, живая таблица 300 не тронута (Gemini 200 через `wg-us0`, гард на месте). Скрипты приёмки в репозитории `env`: `production/shard/0001/eu/exch4-e2e.sh`, `ai-watchdog-test.sh`. **Не сделано:** тег/`gh release` для 0.7.27 (дерево незакоммичено: `Cargo.toml` 0.7.27, `HEAD` 0.7.24).
- 2026-11-21 — **Группа `ai-cn`: direct whitelist для китайских AI (api 0.7.26).** Владелец: китайские AI (DeepSeek, Qwen, Moonshot/Kimi, Zhipu/GLM, MiniMax, Volcengine/Doubao) **не** должны идти в AI/US-группу — они не заблокированы в EU, а `deepseek.com` (им ходит наш тулинг) был жёстко срезан fail-closed гардом таблицы 300. Это **не exit group**: у неё нет марки, `ip rule`, таблицы и выхода — только nft-сет `aicn4` и `ip daddr @aicn4 return` **выше всех марок**, поэтому трафик выходит с IP самой ноды. Резолвер обобщён (`--group/--domains/--zone-name`, `LOG_NAME`, пишет ещё `<zone>.count`), вторая зона `aicn4.zone` добавлена в `ZONES` (`MIN_AICN4_LINES = 8`) и отдаётся `/router/geo/aicn4.zone` (+ `.sha256`) с тем же контрактом; юнит/таймер `frkn-geo-aicn4-update.{service,timer}` (ежечасно + до 5 мин). Ноды: новая обёртка `deploy/geo/nodes/frkn-geo-aicn4-refresh.sh` (общий движок `frkn-geo-ai4-refresh.sh` теперь берёт тег из имени зоны и на `304` переприменяет зону, если сет в ядре пуст — заодно закрыт известный пробел §8.2), `frkn-geo-aicn4.{service,timer}`, в `frkn-msk-route.sh` — сет `aicn4`, правило `return` в обеих цепочках и засев из `/etc/frkn/geo/aicn4.zone` при пересоздании таблицы. `deepseek.com`/`chat.deepseek.com` убраны из `domains/ai.txt` (переехали в `domains/ai-cn.txt`). Живой прогон api: **17 доменов, 25 имён, 57 кандидатов, 50 прошли TLS/SNI, 50 `/32`** (`volces.com` apex без A-записи → `0 IPs none`). Приёмка (nl01, nl02): сет `aicn4` 50 `/32` (32 токена после auto-merge), `chat.deepseek.com` с ноды **403** вместо `000`, `api.deepseek.com` **401**, exit IP = IP ноды (`132.243.203.40`), netns-путь пира — 403 за 0.05 с при неизменных счётчиках `wg-ru-msk`/`wg-ru-nvsb`/`wg-kz0`; доказательство порядка — адрес из `ai4`, временно добавленный в `aicn4`, перестаёт блокироваться (000 → 403), затем сет восстановлен из зоны; `ai4` по-прежнему блокируется (000), RU (`0x2`) и youtube (`0x6`) не изменились. Тесты: geo 22, `cargo test --bin api` — 294. Доки: `ffcore/docs/router-geo.md`, `docs/API.md`, `env/.../routing.md` §8.3, `overlay.md`, `domains/README.md`, `env/.../README.md`.
- 2026-11-21 — **Группа `ai`: домены → проверенные IP → nft set `ai4` (api 0.7.25).** Обработчик гео-листов обобщён: `ZONES` = `ru.zone` (порог 1000) + `ai4.zone` (порог 8), один фильтр на `<name>.zone` / `<name>.zone.sha256`, имя зоны сверяется со списком (неизвестное — `404`, путь не склеивается с каталогом), заголовки/ETag/`304`/`HEAD`/`405` — те же; тесты geo 20 (`routes_serve_the_ai4_zone_with_the_same_semantics`, `ai4_zone_is_served_below_the_ru_floor`, `unknown_zones_are_404` и др.). Резолвер `deploy/geo/update-ai4-zone.py` (+ `frkn-geo-ai-update.{service,timer}`, ежечасно + до 5 мин): источник — `env/production/shard/0001/eu/domains/ai.txt`, A-записи у **1.1.1.1 и 8.8.8.8** (объединяются), каждый IP проверяется **TLS с SNI и проверкой сертификата на имя** (wildcard учтён) — непрошедшие выпадают; домен без проверенных IP берёт прошлое поколение из `ai4.state.json` (`reusing previous generation`); публикация отменяется при <50 % доменов со свежим ответом (сломан чекер), <60 % от предыдущего размера (сжатие) или нуле — старая зона остаётся. Живой прогон на api: **29 доменов, 48 имён, 133 кандидата, 90 прошли TLS, 90 `/32`, 4.1 с**; второй прогон — 92 (CDN ротирует). Ноды: `deploy/geo/nodes/frkn-geo-ai4-refresh.sh` — условный GET, sha256 с `.sha256` и сверка с ETag, форма/порог 60 % от текущего сета, замена сета **одной транзакцией `nft -f` (flush + add)**, сохранение зоны/ETag только после успеха; ошибки сети → exit 0 (прошлый сет жив), ошибки валидации → exit 1. Маршрут: `ip daddr @ai4 meta mark set 0x3` до `ru4` в `mark_pre`/`mark_out`, `ip rule fwmark 0x3 lookup 300 priority 3090`, таблица 300 пустая (US-выхода нет → трафик идёт штатным IPv4 ноды, поведение не изменилось; TODO в `env/.../routing.md` §8). Живая приёмка: сет `ai4` 92 элемента на обеих нодах (первый apply 0 -> 92), после часового прогона резолвера — `92 -> 84`, повторный прогон «unchanged (304), set keeps 84 elements»; счётчик в postrouting: 9 пакетов на 9.9.9.9 — 0 с меткой `0x3`, трафик на IP из `ai4` — с меткой; `ip route get` для не-AI адреса — старый путь (`via … dev ens3`), RU-марка `0x2` → `wg-msk*` таблица 200, `ru4` побайтно тот же список (7410 токенов до и после), хендшейки пиров живые. Доки: `docs/router-geo.md`, `docs/API.md`, `env/.../routing.md` §8, `env/.../domains/README.md`.
- 2026-11-21 — **GEO-LISTS: api отдаёт свежий RU-список для роутера, ipdeny обновляется по таймеру.** Публичные plain-text ручки (без auth и без AGW-конверта, как `/sub/<token>`): `GET/HEAD /router/geo/ru.zone` и `/router/geo/ru.zone.sha256`; файл из `[service] geo_dir` (дефолт `/var/lib/frkn/geo/ru.zone`), `ETag` — сильный `"sha256:<hex>"` от содержимого, `Last-Modified` из mtime, `Cache-Control: public, max-age=3600`; `If-None-Match` (список / `W/` / `*`) → `304` без тела, `HEAD` отдаёт те же заголовки с полным `Content-Length`. api валидирует список перед выдачей (`validate_zone`: IPv4-форма, октеты ≤ 255, префикс ≤ 32, ≥ 1000 строк) и на битом файле отвечает `503`, чтобы роутер остался на вшитой копии. Обновлятор `deploy/geo/update-ru-zone.sh` + `frkn-geo-update.{service,timer}` (ежедневно 04:30 UTC + до 30 мин, `Persistent=true`): curl с таймаутами, CRLF-нормализация, awk-валидация (`MIN_LINES=1000`), `chmod 644`, sha256, `mv` в том же каталоге (атомарно); при ошибке старый файл остаётся, совпавший sha256 не перепубликует (ETag стабилен → роутеры получают 304). Прогон на реальном ipdeny: **8664 CIDR, sha256 `a115697b…0a645`** — байт-в-байт совпал с вшитым списком `../router/fw/files/etc/frkn/geo/ru.zone`. Доки `docs/router-geo.md` (+ `docs/API.md`), `cargo check --bin api --all-targets` зелёный, `cargo test --bin api` — **281 passed** (geo: 15 новых). **Не выкачено**: на проде `/router/geo/*` пока отвечает 405 (неизвестный путь).
- 2026-11-21 — **Продление подписки возвращает отозванный грант (v2-шард), а не только двигает дату.** Дыра: `expire_grants` закрывает грант в 00:00 с причиной `expired`/`quota`, а `extend_grants` обновлял только строки `WHERE revoked_at IS NULL` — человек платил ещё месяц, дата подписки уезжала вперёд, а `POST /v1/peers` продолжал отвечать `no_grant`/`grant_expired` 402 навсегда. Теперь миграция `0028_grant_revoke_reason.sql` (+ `ensure_schema`) даёт `subscription_access.revoked_reason`, `revoke_grant` его пишет (`expire_grants` → `expired`/`quota`, `DELETE /v1/access` → `subscription_ended`, удаление пира → `user`), а `PgPeers::renew_grants` под `FOR UPDATE` продлевает живые гранты и **возвращает** отозванные часовым (`expired`) или счётчиком (`quota`, только если под лимитом снова есть место) — `env`/`pool`/`plan`/`protos` те же, то есть тот же шард. Явные отзывы (`user`, `subscription_ended`) и строки без причины с ещё идущим сроком не трогаются; строки без причины с истёкшим сроком (все, что прод успел отозвать до миграции) судятся как `expired`. Подписка без грантов — no-op (v1 не задет). Пиры не разотзываются: ноды уже получили `Delete`, новый туннель выдаётся обычным `POST /v1/peers`. Точки входа — общий `renew_grant_access`: `PUT /subscription`, `POST /admin/api/subscriptions/<id>/extend`, Apple IAP (`set_subscription_expiration`), продление ключом. Лог: `renewal: grant revived for subscription … env … until …`. Тесты api **266** (+7: `renewal_revives_an_expired_grant`, `renewal_leaves_an_explicit_revocation_alone`, `renewal_without_grants_is_a_no_op`, `a_revived_grant_passes_the_peers_access_check`, `a_spent_quota_stays_revoked_until_there_is_room`, `a_live_grant_only_moves_forward`, `a_pre_column_revocation_is_judged_by_its_term`). Доки: `docs/api-peers.md` («Продление: грант не только продлевается, но и возвращается»). **На прод не выкачено**, миграция `0028` не накатана.
- 2026-11-21 — **Лимит пиров стал на устройство, а не на подписку.** Кап `peers_per_device` (дефолт 3, конфиг `[service.peers].peers_per_device`, старое имя `max_per_device` читается, `0`=3) теперь считает живые туннели по X25519-ключу устройства (`devices.public_key`) **суммарно по всем env и подпискам** (`PeersState::peers_of_device_key`, индекс `DeviceStore::by_key`), поэтому вторая подписка на том же телефоне не даёт ещё три туннеля, а роутер с тремя каналами упирается в 3 ровно на третьем. Новый код отказа — `peers_limit` (409, было `device_limit`); `devices_limit` (15 устройств на подписку) остался отдельным и проверяется только при регистрации нового устройства. Идемпотентный `POST /v1/peers` (тот же `(env, public_key)`) теперь ищется **до** проверки лимитов, так что повтор на заполненном капе — 200 с тем же пиром. Попутно закрыт баг: новое устройство не писалось в память (`DeviceStore`), из-за чего `devices_per_subscription` и счётчик устройства видели только строки, загруженные на старте. Тесты: api **256** (новые `a_device_cap_counts_across_subscriptions_and_envs`, `extra_channels_of_one_device_count_towards_its_cap`, `recreating_the_same_peer_does_not_add_to_the_device_count`, `revoked_peers_are_not_counted_against_the_device`). Доки: `docs/api-peers.md` (раздел «Лимиты»), `docs/app-peers-tz.md`, `docs/peers-schema.md`, `docs/peers-envelope.md`. Прод не трогали.
- 2026-11-21 — **«Пир не доезжает до ядра» не подтвердилось; карточка теряла второй entry-IP ноды.** (1) Жалоба «`POST /v1/peers` → 200, а `wg show awg0-peer` пуст на sh-nl01/sh-nl02» — ложная: в `peers` одна живая строка `10.88.0.2/32` (`0001`, `amnezia_peer`, создана 01:59:49), в ядре **обеих** нод тот же публичный ключ, на nl01 живой хендшейк (endpoint клиента, 3.8 KiB rx/3.3 KiB tx); `api` не рестартовал с 22:32 (`NRestarts=0`). На нодах нет бинаря `wg` — только `awg`, поэтому `wg show awg0-peer` = `command not found`, а не «пусто»; смотреть `awg show awg0-peer`. (2) Проверено живьём: новый пир `10.88.0.3` появился в ядре обеих нод через ~2–4 с после ответа api, `POST /v1/peers/delete` снял его так же быстро; пир/устройство после проверки удалены, в проде осталась только строка `10.88.0.2`. (3) **Карточка теряла доп. адрес ноды**: `pool_cards` строил один `endpoint` из `node.address`, игнорируя `[node] node_ips`, — `/v1/locations` и `POST /v1/peers` отдавали 2 endpoint вместо 3 у `0001:eu:amnezia_peer` (nl01 объявляет `.40` и `.41`, nl02 — `.154`). Теперь `nodes[]` — один элемент на entry-IP (общий ключ/MTU/DNS/обфускация, разные `endpoint`), тест `a_card_carries_every_entry_ip_of_a_node`, контракт обновлён в `docs/api-peers.md`. **Не выкачено** — ждёт релиза (миграций нет, только код).
- 2026-11-20 — **0.7.19/0.7.20 на проде: нода с расхождением в пуле не регистрируется, страна пула доезжает до карточки, реестр пулов жив.** (1) **Порт вне сравнения** (`client_inbound_fields`): одна карточка может отдаваться на разных портах — в карточке у каждой ноды свой `endpoint`, клиент дёргает выбранную ноду; адрес интерфейса, DNS, keepalive, MTU, ключ и профиль обфускации по-прежнему обязаны совпадать. (2) **Расхождение = отказ**: при `pool_strict_inbounds` (дефолт) `POST /node` отвечает `409 pool_inbounds_conflict` **до** регистрации — нода не попадает ни в `/status`, ни в `/connections/sync`, ни в `peers-init`, а fnode видит 409 и завершается (`systemd` → failed) вместо ретрая конфиг-ошибки каждые 10 с; `false` оставляет прежний «зарегистрировать как degraded». (3) **Страна пула в карточке**: `[node] pool_country_code`/`pool_country_name` → `nodes.pool_country_*` (миграция `0026` + ensure на старте) → `country`/`country_name` карточки, фолбэк — страна первой ноды. Первая попытка не работала вовсе: у ноды **свой** `NodeRequest` в `bin/node/http.rs`, поля туда не попали, и в БД было пусто (карточка выглядела правильно только за счёт фолбэка) — 0.7.20 это исправляет. (4) **Реестр пулов**: `note_pool` падал на типах (`serde_json::Value` в jsonb, i64 в integer) — теперь `$3::text::jsonb` и i32; `pools` заполняется на регистрации (`0001/eu`, `node_count 2`, переводы). (5) Живая приёмка 0.7.19: пир по `card_id` → ядро на обеих нодах → хендшейк и ping `10.88.0.1` с `ee.frkn.org` (29 мс); 0.7.20: `country: NL / Netherlands`, `pools: ["eu"]` в `shard_overview`. Тесты: api 251.
- 2026-11-20 — **0.7.17 на проде: `card_id` в `PeerView`, реепинг мёртвых устройств, шард `0001` проверен живым туннелем.** (1) `peers.pool` (миграция `0025`) + `card_id = <шард>:<пул>:<протокол>` в ответах `/v1/peers*`: приложение собирает конфиг из одного списка пиров (адрес из блока шарда, endpoint/ключ/MTU/DNS/обфускация — из ноды пула). (2) `reap_stale_devices` (`stale_device_days` 90, интервал 3600, 200 за тик): устройства без живых пиров и без активности удаляются вместе с отозванными пирами. (3) `PgPeers::ensure_schema` добавляет `peers.pool` на старте — бинарь и миграция могут приезжать в любом порядке. (4) `GET /admin/api/shards` больше не 500: `list_shards` кастовал `pools` (text[]) в int8; теперь отдаёт список пулов, `kind` и `enabled`. (5) `note_pool` не писал реестр никогда (jsonb-параметр и i64 в integer-колонку, ошибка глоталась `let _`) — касты `$3::jsonb`, `$4::int4` и warning в лог; фикс не выкачен, ждёт `0.7.18`. Живая приёмка: пир `10.88.0.2–4` создан по `card_id`, появился в `awg show awg0-peer` на обеих нодах, туннель с `ee.frkn.org` пинует `10.88.0.1` (nl01 28.9 мс, nl02 38.5 мс), `POST /v1/peers/delete` + `/v1/devices/delete` снимают пиров с нод. Обе ноды шарда переведены под systemd (`awg-quick@awg0-peer` был inactive при живом интерфейсе, поднятом руками; `systemctl restart` бесполезен — юнит без ExecStop, нужен `awg-quick down` + `systemctl start`). **Провал по дороге:** выкатил 0.7.17 до миграции `0024` — бинарь паниковал на `node_type` при загрузке нод, прод лежал ~2.5 минуты (20:34–20:36), откат на 0.7.16 с бэкапа, затем `0024` и повторная выкатка. Миграции накатывать ДО бинаря, включая `ADD VALUE` из `0024`.
- 2026-11-20 — **0.7.17 (написано, не выкачено): карточка в `PeerView` и реепинг мёртвых устройств.** (1) **`peers.pool`** (миграция `0025_peers_pool.sql`): `resolve_card` больше не выбрасывает середину `card_id` (`src/bin/api/http/handlers/peers.rs:263`), `createPeer` пишет пул в строку пира, а `peer_view` отдаёт `card_id = <шард>:<пул>:<протокол>` — из него приложение собирает конфиг по одному `GET /v1/peers` (адрес из блока шарда, endpoint/ключ/обфускация — из ноды пула). Старые строки бэкфиллятся только там, где у шарда ровно один пул; иначе `card_id = null` (ручная форма `env`+`proto` тоже без карточки). (2) **`reap_stale_devices`** (`tasks.rs`, `postgres/peers.rs`, `[service.peers] stale_device_days` = 90, `[tasks] stale_device_interval_sec` = 3600): удаляет устройства без живых пиров, у которых последняя активность (создание/ротация пира, его отзыв) старше порога — иначе лимит 15 устройств забивается стёртыми телефонами, а `device_uid` в Keychain переживает переустановку. Потолок 200 устройств за тик, `0` выключает таску. (3) `PgPeers::ensure_schema` добавляет колонку на старте — бинарь не падает на всех peer-запросах, если миграция ещё не накатана (и наоборот). Контракт — `docs/api-peers.md`, конфиг — `api-example.toml`. Тесты: api **251** (+3: `peer_view_names_the_card_it_was_created_for`, `resolve_card_keeps_the_pool` и распознавание пустой середины хэндла).
- 2026-11-20 — **0.7.7–0.7.10: шарды объявляются конфигом, гранты истекают сами, мусор не копится.** (1) **`expire_grants`** — новая таска: грант с истёкшим сроком или выбранной квотой снимается вместе с пирами и v1-коннектами. До неё в api **никто никогда не отзывал грант**: истёкший плательщик держал рабочий туннель в ядре ноды бесконечно (проверка квоты стояла только на создании пира). (2) **Шарды в конфиге** — `[service.peers.shards."0001"] {plan,title,kind,max_subscriptions,enabled}` применяются на старте (`upsert_shard`), та же строка, что пишет `PUT /admin/api/shards/{env}`; неизвестный план/вид — громкая ошибка, а не молчаливо мёртвый шард. Ручного SQL больше не нужно. (3) **Prune** — reconcile удаляет v1-коннекты, у которых нет пары `(env, proto)` в грантах (`grant_changed`, не restorable), `scope_env` подписки не трогается, dry-run по умолчанию, потолок `prune_max_per_tick`. (4) **`DELETE /v1/access`** снимает и коннекты env сразу (`revoke_env_access` — общий путь с таской). (5) Продление из ЛК (`PUT /subscription`) двигает гранты. (6) `ensure_shard_capacity` (мёртвый код) удалён. Миграция `0021` на прод ещё не накатана. Тесты: lib 109, api 248.
- 2026-11-19 — **Активация: грант получает peer-протоколы, ответ — env+capacity, продление двигает гранты.** Три дырки в `/v1/activate` (peers flow): (1) в `subscription_access.protos` писались только v1-теги (`filter(!is_peer)`), поэтому `POST /v1/peers` сразу после успешной активации отвечал `proto_not_granted` — теперь добавляются `AmneziaPeer`+`WireguardPeer` (из каталога, если есть, иначе оба); (2) ответ содержал только `Instance::Key`, теперь для peers-flow отдаётся `{subscription_id, key, env, capacity{plan,used,limit,overflow}}`; (3) продление не двигало `subscription_access.expires_at` — добавлен `extend_grants()` (`UPDATE ... RETURNING` + write-through в память), вызывается после `add_days`. Тесты: lib 109, api 244. **Не сделано:** IAP-путь (`POST /v1/subscriptions`) грант не создаёт — покупатели через App Store останутся без пиров.
- 2026-11-19 — **Лимиты: 3 туннеля на устройство, 15 устройств на подписку (45 туннелей).** `Limits` получил `devices_per_subscription` (+`check_devices`), дефолты в `[service.peers]`: `max_per_device = 3`, `max_devices_per_subscription = 15`, `max_per_subscription = 45` (чтобы лимит пиров не срабатывал раньше лимита устройств). Проверка устройства стоит **до** создания новой установки в `POST /v1/peers` — существующее устройство с лишним туннелем не считается дважды. Новый отказ `Refusal::DevicesLimit` → 409 `devices_limit` (отдельный код, чтобы приложение сказало «привязано 15 устройств», а не «лимит туннелей»). Доки и `api-example.toml` обновлены. Тесты: lib 109, api 244.
- 2026-11-19 — **Вид шарда (`kind`) и алерт на заполнение адресов.** Миграция `0021` дополнена: `env_capacity.kind` (`core|premium|personal`, CHECK + backfill по префиксам), вид в `shard_overview`; `ShardKind` в `peers/mod.rs`; кэш `PeersState.env_capacity` стал структурой `ShardCapacity{plan,max_subscriptions,enabled,title,kind}` + `shard_kind()`; вид **не выводится из имени**. `validate_env_served` решает по виду: пустой `core` — законно, `premium`/`personal` — только со своей нодой, энв без строки в `env_capacity` — по старому вайтлисту (легаси-регионы). Private-ручки проверяют `kind == Personal`, для легаси `custompersonal…` — по имени. Алерт: `AlertKind::ShardFillHigh` (`shard_fill_high`) и правило `observe_shard_fill` в существующем движке, гистерезис (`[alerts] shard_fill_pct` 85 / `shard_fill_reset_pct` 75), прогон раз в тик в `tasks.rs`, запись в `alert_events` с `node_id = NULL`. Тесты: lib 109, api 243.
- 2026-11-19 — **Шард как непрозрачный id + человеческий слой.** В конфиге ноды `shard = "0001"` принимается наравне с `env` (`NodeConfigRaw`, serde alias); `GET /status` отдаёт у каждой ноды `env` и `env_title` (join с `env_capacity.title`, читается через `to_jsonb`, чтобы api переживал старт до миграции); миграция `0021_shard_title.sql` — колонка `title` + view `shard_overview` (shard, title, plan, pools, nodes, peers_live, address_hosts); при регистрации ноды в именованном шарде без адресного блока — громкий `warn` («peers cannot be served until the subnet is seeded»). Доки `api-peers.md` переписаны: имя env несётся дословно, `customstandard-01` больше не появляется. Тесты: lib 109, api 241.
- 2026-11-19 — **ENV → Shard, шаг 1: имя шарда строкой, без фолбэков.** `Env::Named(String)` (`memory/env.rs`) — имя не встроенного региона несётся **verbatim**: `standard-ru` теперь проходит конфиг api (`[service.peers] subnets`), конфиг ноды (`[node] env`), БД и топик `peers-update-standard-ru` без префикса `custom`. Раньше `Deserialize` заворачивал неизвестное имя в `Custom`, и в БД уезжало `customstandard-ru` — шард, которого не обслуживает ни одна нода. Удалены молчаливые пути: `From<&str>`/`From<String>` (→ `Experimental`), `Default`; десериализация неизвестного имени теперь ошибка; `traffic::parse_env` → `Option` (+warn), а не запись трафика в `experimental`; строки БД с непарсящимся env пропускаются с warn (`postgres/node.rs`, `install_token.rs`) или дают ошибку (`connection.rs`); `scope_env` подписки — `None` + warn. Формат имени — `is_shard_name()` (lowercase, цифры, дефис, ≤32, без ведущего дефиса), потому что имя уходит в топик. Тесты: lib 109, api 241, добавлены `test_named_shard_round_trip` и `test_no_silent_fallback`. **Не сделано:** `kind` (Core/Premium/Personal) вместо `is_personal`/`is_premium`/`is_frkn_shared`; их строковые предикаты пока на месте, `Named` = «не frkn shared» (шард требует хотя бы одной ноды с протоколом).
- 2026-11-19 — **PEERS: ёмкость шарда считается в адресах, а не в подписках.** `live_shards` берёт шарды только объявленные для ЭТОГО плана (`env_capacity` = карта plan→шард, раньше `super-duper`-энв мог принять `standard`-юзера) и считает лимит из адресного бюджета: сумма блоков `env_networks` минус резерв 10% (`peers/alloc.rs:block_hosts/total_hosts/allocatable`, /24 = 253 хоста, раздаются 227). `POST /v1/peers` отдаёт `no_free_address` на 90% бюджета, а не на последнем адресе — резерв под перевыпуск пиров. `GET /v1/capacity` показывает `used`/`limit` в адресах и больше не падает на NULL `max_subscriptions` (было `row.get::<i32>` на nullable колонке). `max_per_subscription` 50 → 15; knob `service.peers.subscriptions_per_node` удалён — лимит больше не зависит от числа нод, конфиг `[service.peers]` задокументирован в `api-example.toml`. Тесты: api 241, lib 108, зелёные.
- 2026-11-19 — **mesh (E2E-мессенджер) выпилен из репозитория**: удалены модуль `src/mesh/` (UIN-реестр + sealed-slot инбокс, 542 строки), хендлеры `bin/api/http/handlers/mesh.rs` и четыре публичные ручки `/v1/mesh/{register,lookup/<uin>,send,pull}`, снапшот-таска (`mesh_registry_snapshot`) и сохранение на shutdown, поле `Service.mesh`, `service.mesh_snapshot_path` из конфига и `api-example.toml`; отдельный бинарь `meshd` (`src/bin/mesh/`, `[[bin]] meshd`) тоже удалён — он зависел от `fcore::mesh` и без модуля не собирался. Причина: фича не используется, клиентов нет ни в одном репо, `mesh.frkn.app` отдавал 502. `cargo check --all-targets` зелёный (только старые warning'и). Восстановить при нужде: `98307e5` (api-модуль) и `5cabf8d` (meshd). На проде после выката убрать `/var/lib/fcore/api/mesh-registry.bin` и vhost `mesh.frkn.app`.
- 2026-11-19 — **PEERS: каркас схемы (api + миграция + нода), без выката**. Миграция аддитивная: `env_networks` с `EXCLUDE USING gist` (пересечение подсетей невозможно), `subscription_access` (строка на env, `protos[]`, свой срок, свой лимит), `devices` (`device_uid` + `device_name` + `support_tag`), `peers` (пир может нести свой ключ — каналы роутера), `peer_traffic`, `subscription_limits`, view `subscription_traffic`. api: валидация в памяти, аллокатор адресов по блокам env, публикация в шину на create/delete/revoke, `peers-init` при регистрации ноды. fnode: отдельные интерфейсы, apply/prune идемпотентны, трафик по `peer_id`, локальный `peers.json`. v1 не тронут (peer-теги отбиваются в старых путях). Спека — `../PEERS-SPEC.md`, контракт — `docs/api-peers.md`. **Не сделано:** Linux-сборка с фичами (гоняю на `us.frkn.org`), приложение, снапшот-восстановление на живом стенде, деплой.

## Done
- 2026-11-23 — **NODE-ENDPOINTS: одна форма точки входа на всех уровнях, аддитивно.**
  `[[node.endpoints]]` (`ip` + опц. `port`/`weight`/`label`) парсится в `NodeConfig`
  (`config/settings.rs`): дубль ip = ошибка, порт `1..=65535`, `weight<=0` = 100,
  `endpoints[0].ip` == `address`; при наличии секции легаси-ключи игнорируются с
  `WARN config_legacy_endpoints_ignored`, без секции прежний путь работает бит-в-бит
  (golden-тест на реальном FR/RU конфиге). `Node.endpoints` — источник правды,
  `node_ips`/`node_weights` остаются зеркалом для старых сборок api и клиентов
  (`TODO(remove-legacy-endpoints)` + `#[deprecated]` на легаси-полях). api:
  `endpoints[]` рядом с `node_ips`/`node_ports`/`node_weights` в `/v1/services`,
  `/v1/config` и в карточках `/v1/locations` (`PoolNode.endpoints`); порт из
  `[[node.endpoints]]` доезжает до клиента. Миграционный счётчик: нода шлёт
  `endpoints_legacy`, api хранит (`nodes.endpoints`/`endpoints_legacy`, self-healing DDL)
  и отдаёт в `/status` и в `NodeResponse` (админ-вид). Клиент: `node_endpoints.h`
  (endpoints → легаси-ключи, «вес = доля, пинг = вето» без изменений), применяется в
  `ServersModel::addServer*`/`editServer`, `portForNodeIp`/`weightForNodeIp`/`NodeIpsRole`
  и в сборке пула; отдельный тест `client/tests/node_endpoints_test.cpp` (QtCore,
  собирается вручную). Скрипт миграции: идемпотентный, бэкап, `--dry-run`, `--report`,
  сверка результата перед записью; на флот **не запускался**. Тесты: lib 131, api 342,
  node 20 — зелено. Без деплоя и коммита.
- 2026-11-23 — **SELFHOSTED-INSTALL: своя нода ставится одной командой и появляется в приложении.** Владелец: клик «своя нода» в ЛК — это продукт, а не набор ручных правок. Причина, по которой нода не доезжала: установщик врал про окружение (литерал `personal` вместо scope env — топик шины не совпадал), не поднимал peer-плоскость и NAT, ставил fnode v0.6.41; api не выдавал блок адресов на mint, не создавал грант на карточку и **не отправлял `peers-init` по приватному пути вообще** (публиковался только из `POST /node`). Сделано: (1) `POST /private/install-token` аллоцирует блок шарда в `env_networks` (первый mint, `<next free /16>` из `auto_subnet_pool`) и отдаёт `env_block` + `node_version`; новый `GET /private/install-token/<token>` отдаёт то же до сжигания токена; (2) `POST /private/nodes` и node-token refresh сеют грант `{amnezia_peer,hysteria2}` (карточка в `/v1/locations`), проверяют, что peer-интерфейс внутри блока (иначе 400, блок принимается от ноды, если у шарда его нет), и **всегда** публикуют `peers-init` — 200 на повторную регистрацию больше не оставляет интерфейс пустым; (3) изоляция: для персонального env `publish_peer` не пишет в широкую тему `peers-update-<env>-<pool>`, а шлёт authoritative `peers-init-<node_uuid>` каждому узлу шарда, `peers_for_node` дополнительно фильтрует по env; тест `an_own_shard_sees_only_its_own_subscription`; (4) нода: `systemctl reload fnode` (SIGHUP) перерегистрируется и пересобирает set, `--peers-resync` игнорирует снапшот на старте, один отложенный re-register через 20 с закрывает PUB/SUB slow joiner. Установщик `frkn.org/install` (1.3.0): env = `scope_env` из токена, `[awg_peers]` на `awg0-peer` (`<block>.1/16`, UDP **51821**, реальный файл в `/etc/amnezia/amneziawg/`), NAT ровно на блок с детектом аплинка и идемпотентным helper'ом (`PostUp/PostDown` включённого `awg-quick@awg0-peer` → переживает ребут), `pool = "own"` + `pool_translation` + `label` = «<Country> | Selfhosted» (полное имя страны), версия ноды спрашивается у api (fallback v0.7.47), повтор безопасен (ключи/uuid/NAT не дублируются). Тесты api: **332 passed** (в т.ч. новый изоляционный). Сборка/деплой на прод и приёмка на `cz2` — ждут ok владельца (см. отчёт сессии). Не закоммичено.

- 2026-11-22 — **exit-health алерты почтой: 0.7.32 на проде (переход — письмо, тик — тишина).**
  Новая ручка `POST /v1/admin/alerts/exit` под `with_service_or_admin_auth` (сервисный токен — тот же,
  что у регистрации нод; SMTP-креденшелов на нодах нет): тело `{group,node,table,event,duration_sec,
  detail,active_iface,probes[]}`, события `exit_dead`/`exit_degraded`/`exit_recovered`.
  Рендер — `alerts::format_exit_message` (`src/bin/api/alerts.rs:1257`): группа, нода, таблица,
  «длится/лежала», по каждой пробе «ожидали <exit IP>, получили <наблюдаемый> — ok|DEAD, handshake Ns»,
  тема `[FRKN] ⛔|🟡|🟢 <нода> — группа <g>: выход мёртв|деградировал|восстановлен`.
  Дедупликация `alerts::ExitDedupe` (`alerts.rs:1137`, `decide` — 1169) по паре (группа, нода):
  переход — сразу, повтор не чаще `[alerts] exit_reminder_sec` (30 мин), `exit_recovered` без эпизода —
  тишина; после рестарта api сидится из `alert_events` (`PgAlerts::latest_exit`, `postgres/alerts.rs:199`),
  так что перезапуск посреди аварии второго письма не даёт; неудачная доставка → следующий тик пробует сразу.
  Хендлер — `http/handlers/alerts.rs:221`, маршрут — `http/routes.rs:1334`; `delivered=false` → 502
  (нода повторит), `action=suppressed` → 200 с причиной. Ручка ходит прямо в `notify_report`, минуя
  тик `monitor_alerts`, поэтому `quiet_env_prefixes` (personal/custompersonal) её не глушит:
  `alert_events.details.route = "exit"`. Написано в `docs/API.md` § «Алерты», `api-example.toml`.
  Тесты: `cargo test --bin api` — **278 passed** (пять новых: переход/напоминание через 30 мин, второй
  эпизод сразу после отбоя, независимость пар, ретрай после сбоя SMTP, текст письма и тема).
  Релиз собран на `frkn-api-jump` из дерева (`nice -n 10 CARGO_BUILD_JOBS=6`, 6m34s), `sha256 659681bc…318b0d7`,
  бэкап `/opt/api/api.bak-0.7.31-20261122-020816`, `/healthcheck` → `Ok (v0.7.32)`. Живая проверка
  (письма читали из ящика `pigeon@frkn.org` по IMAP, не только по журналу): nl02 оба RU-туннеля down →
  `exit_dead` через **11 с** (Очередь Яндекса `1795302529-m8nxreadqmI0-…`), up → `exit_recovered`
  «лежала: 1 мин»; nl01 только primary down → `exit_degraded` через **19 с** (active_iface `wg-ru-aeza`),
  up → `exit_recovered` «45 с»; youtube на nl01 `wg-kz0` down → `exit_dead` за **1 с**, повторные прогоны
  юнита — тишина, up → `exit_recovered` «16 с». Повторы того же статуса (3 подряд) → `suppressed/repeat`,
  после отбоя 100 с наблюдения — ни одной новой строки и ни одного вызова с нод.
  Ноды: `/usr/local/bin/frkn-ru-exit-watchdog.sh` (хук `report_exit`, бэкап `.bak-20261122-020*`),
  `/etc/frkn/alert-api.env` (`600`, `ALERT_NODE` — у nl02 ядро с хостерским именем),
  `frkn-youtube-route.sh` + `RuntimeDirectory=frkn-youtube-route`/`Preserve=yes` в двух юнитах.
  Не покрыто живьём: `ai` и `gov` (тот же скрипт и код-путь, но их туннели не роняли); у `youtube`
  нет своего таймера — тревога приходит на часовом прогоне, если юнит не перезапустили.
- 2026-11-21 — **`[node] bench_url`: нода сама объявляет адрес своего бенча.** Раньше `nodes.bench_url` (публично `speedtest_url` в `/status`) жил только в БД и ставился руками SQL — пересобранная/перерегистрированная нода его теряла. Теперь необязательный `[node] bench_url` в `/opt/fnode/config.toml`: fnode шлёт `bench_url` в payload регистрации (`src/bin/node/http.rs`, ключ **не сериализуется**, если пусто, — иначе старый api/другая нода затирали бы значение), api принимает его в `NodeRequest` и нормализует общим `fcore::normalize_bench_url` (обязателен `http(s)://` + хост, завершающие слэши срезаются, пусто = «бенча нет»; невалидное непустое → 400). Upsert в `src/bin/api/postgres/node.rs` пишет `bench_url = COALESCE(EXCLUDED.bench_url, nodes.bench_url)`: отсутствие поля не стирает значение, выставленное иначе (старая нода, `deploy/seed-nodes-bench-url.sql`), явное — перезаписывает; память ноды обновляется на регистрации, поэтому `/status` видит новый URL без рестарта api. Тесты: lib `bench_url_keeps_only_a_dialable_base`; api `node_request_without_bench_url_is_absent`, `node_request_bench_url_is_normalized_and_carried`, `node_request_rejects_unusable_bench_url`, `node_upsert_keeps_stored_bench_url_when_absent`, `node_upsert_binds_bench_url_parameter`, `registration_updates_speedtest_url_in_status`; node `registration_carries_bench_url`, `registration_omits_absent_bench_url`. `cargo test --bin api` — 287, `--bin node` — 9, `--lib` — 112. Оператору на существующей ноде: добавить строку в конфиг и перезапустить fnode. **Не выкачено** (миграций нет).
- 2026-11-15 — **сводка переехала: api отдаёт данные, письмо собирает mrkting (api 0.6.139)**. `GET /v1/admin/digest?kind=&from=&to=` под service-or-admin токеном возвращает JSON сводки (флот, алерты, клиенты, люди, тренд), окно считает отправитель. Из api удалены `DigestConfig`/`[digest]`, `digest_scheduler`, рендер письма и SMTP-отправка сводки; **алерты по нодам остались** со своим `enabled`. Тесты `cargo test --bin api` — 205 passed. Релизы: `v0.6.138` (данные) и `v0.6.139` (ручка принимает сервисный токен — mrkting ходит именно им, а не админским; без этого было бы 401). Прод: бинарь `sha256 2360a2b3…`, бэкап `/opt/api/api.bak-0.6.137-20261115-195615`, `/healthcheck` → `Ok (v0.6.139)`, секция `[digest]` из `/opt/api/config.toml` убрана (бэкап в `/root/frkn-prod-backups/`). Живая проверка: сервисным токеном `https://api.frkn.org/v1/admin/digest?kind=daily` → 200 с реальными числами (70 нод, 3055 подключений, 1071 активных), без токена → 400.
- 2026-11-14 — **api 0.6.135: `speedtest_url` в `/v1/services` и `/v1/config`**. Коннекшены теперь несут base URL бенча ноды рядом с `node_ips`/`node_ports` (`node.bench_url`), чтобы клиент не выводил адрес из hostname. Поле **отсутствует**, если бенча у ноды нет (премиум-слоты) — та же семантика, что в `/status`: «нет тест-файла», а не 0 Мбит/с. Тест `gateway_connection_carries_speedtest_url` (плюс проверка, что без бенча ключа нет), `cargo test --bin api` — 204 passed.
- 2026-11-14 — **api 0.6.134: `speedtest_url` в `/status`**. Причина: зона `beacon.frkn.org` переехала на шаблон `<hoster>-<cc>-<city>-bench-<nn>.bench…`, и потребители (status-page, Dopamine) больше не могут вывести адрес бенча из hostname/PTR — сборка адреса на клиенте давала NXDOMAIN. Теперь источник истины — `nodes.bench_url` (миграция `0017_node_bench_url.sql`, сид `deploy/seed-nodes-bench-url.sql` на 10 нод), `/status` отдаёт его дословно как `speedtest_url` (`null` = бенча нет, напр. премиум-слоты). Регистрация ноды поле не перезаписывает (`Node::bench_url` не приходит от fnode, upsert его не трогает). 2 новых теста (`status_row_carries_speedtest_url_verbatim`, `status_row_without_bench_is_null_not_missing`), `cargo test --bin api` — 203 passed. Релиз `v0.6.134`, бинарь `d9d032d9…`, бэкап `/opt/api/api.bak-0.6.133-20261114-154050`. Приёмка: 10/10 пуловых нод отдают адрес, 9 из 10 отвечают `200 1048576` (`eu02` offline). Контракт для клиента — `dopamine/frkn-docs/api-speedtest-url-consumer.md`.
- **Открыто:** `/v1/services` и `/v1/config` поле пока не отдают (только `/status`); status-page переключён на поле, локальная карта `BENCH_HOSTS` удалена.
- 2026-11-13 — **api 0.6.133: `pool` в публичном `/status`**. Status-страница группирует ноды по пулам и меряет скорость по пулу/ноде, но публичный `/status` выбрасывал пул, хотя нода его несёт (`Node.pool`, `Node.pool_translation`) и `/v1/services` уже отдаёт оба. Добавил в `NodeStatusPublic` поля `pool` и `pool_translation` как есть из конфига (`[node] pool`), оба **всегда** в JSON и `null` у нод без пула — потребитель отличает «пула нет» от «старый api». Маппинг вынесен в `node_status_public()` + `status_listed()` (personal-ноды по-прежнему не публикуются), 3 новых теста. Отдельного `pool_id` нет намеренно: строка пула — идентификатор, `pool_translation` — отображаемое имя, второй id был бы вторым источником правды (`docs/node-pool.md`). Проверка: `cargo test --bin api` — 201 passed; релиз `v0.6.133` (мердж `d9f6aa8`), бинарь `sha256 4e3b41b5…` = собранному и совпадает с диджестом ассета, бэкап `/opt/api/api.bak-0.6.132-20261113-221639`, `/healthcheck` → `Ok (v0.6.133)`. Живой `/status`: 42 ноды, ключи `pool`/`pool_translation` присутствуют, пулы `EU Рилзопровод` 6, `RU Рилзопровод` 3, `Gaming Рилзопровод` 1, у остальных 32 — `null`. Расхождение: у `pl.frkn.org` в БД пул есть, в `/status` `null` — на хосте два инстанса fnode, и `/opt/fnode/config.toml` пула не содержит (в `/opt/fnode-eu/config.toml` он есть), поэтому одна и та же нода регистрируется дважды и без пула перетирает память; нужна правка конфига/уборка дубля.
- 2026-11-13 — **api 0.6.132: `/connections/sync` отдаёт `node_`-ноде только её env**. С 0.6.116 предикат `sync_conn_matches` был записан как `strict_env || proto == Hysteria2 || conn.env == env`, а `strict_env = bound.is_some()`; при `node_`-токене (`strict_env = true`) вся дизъюнкция истинна и сравнение `conn.env == env` **не вычислялось** — selfhosted-нода тянула все непинованные коннекты протокола из всех env. Живьём на 0.6.131: нода `1cc40d25…` (env `custompersonaleb371bd5a…`) на `/connections/count` получала AmneziaWg **1478** + AmneziaWgMobile **1579** вместо своих 2; сервисным токеном тот же env — `1/1`, а `dev` — `1388/1478` (значит env-фильтр работал только у наших нод). Отсюда и «3к пиров» на клиентских нодах и 889 мёртвых пиров на prem1 (awg8/awg9, env `personal1a9faff…`, 2 живых коннекта, 0 handshake). Теперь env обязателен всегда, поблажка Hysteria2 осталась только сервисному токену; `/count` и `/sync` делят один предикат, поэтому prune видит ту же (правильную) разницу. Тест `test_sync_conn_matches_node_token_is_env_scoped` (4 случая), `cargo test --bin api` — 198 passed. Релиз `v0.6.132` (мердж `0109c07`, коммит `6486e00`), бинарь `sha256 e6a0efcb…` = собранному на `us.frkn.org`, бэкап `/opt/api/api.bak-0.6.131-20261113-170251`, `api active`, `/healthcheck` → `Ok (v0.6.132)`. Проверка после выката тем же токеном: `1/1`, сервисный `dev` — `1354/1445` (без изменений). Осталось: у клиентских нод старые пиры снимаются только рестартом fnode (полный sync + prune по памяти); пиры, которых нет в снапшоте, prune не видит — нужен отдельный prune по ядру.
- 2026-11-12 — **сводки по системе в почту: daily / weekly / monthly (api 0.6.131)**. Новый модуль `digest.rs` + таска `digest_scheduler` (тик раз в 10 мин, отправка по локальному часу, отметки в `digest_runs`, поэтому рестарт не даёт дубля). Блоки: флот (онлайн/офлайн, `премиум`/`личные`/shared env), алерты с разбивкой по правилам и простоем, клиенты (подключения, протоколы, страны), **люди с дельтами** (активных на конец периода, ±к началу, новые, истёкшие, истекают за 7 дней, по продуктам) и тренд (7 дней / 8 недель / 12 месяцев). HTML + текстовый фолбэк через `[alerts.email]`, ручка `POST /v1/admin/digest/send?kind=…&dry_run=1`. Проверка живьём: daily — `57/65 нод, 56 алертов, 3397 подключений, 1071 активных (9 новых, 9 истекло)`, weekly — `1071 (+34), 78 новых, 42 истекло, тренд 797→1071`. Письмо доставлено. По ходу поймал и починил: `sum(bigint)` → numeric ронял api паникой (0.6.130), а api при старте печатал в журнал весь Settings с токенами и паролями — теперь одна строка.
- 2026-11-11 — **api 0.6.126: в теме письма видно состояние ноды**. Было `[FRKN] ee.frkn.org` на все алерты (один тред на ноду, но в списке писем ничего не понятно); стало `[FRKN] ee.frkn.org — офлайн` / `— снова онлайн` / `— лежит > 1 ч` / `— мигает` / `— CPU` / `— диск`, у watchcat свои метки (переключение, нет живых выходов, перехват, карантин, снапшот). Компромисс осознанный: тредов теперь по числу состояний на ноду, `References` по-прежнему указывает на общий корень хоста. Релиз `v0.6.126` (`210a73b`), сборка (6 артефактов, 0 ошибок), диджест `094a77ae…` = загруженному, бэкап `/opt/api/api.bak-0.6.125-*`, healthcheck `Ok (v0.6.126)`, тест-алерт доставлен.
- 2026-11-09 — **api 0.6.125 на проде: ручка алертов от наших бекендов**. `POST /v1/admin/alerts/send {subject,text}` под `with_service_or_admin_auth` (сервисный токен payment-gw совпадает с `[service].token` api — проверено), текст до 8000 символов, тема до 200; доставка через тот же канал, что и алерты нод. Релиз `v0.6.125` (`a220f21`), сборка на билд-хосте (6 артефактов, 0 ошибок), диджест `sha256 5782e3ab…` = загруженному, бэкап `/opt/api/api.bak-0.6.124-*`, healthcheck `Ok (v0.6.125)`. Живая проверка: плохой токен → `401`, сервисный → `{"delivered":true,"email":true,"telegram":null}`. В репозиторий добавлен `docs/port-probe.md` — контракт бекенда для порт-теста из поддержки.
- 2026-11-09 — **api 0.6.124 на проде: тема письма = хост, треды собираются**. Мердж `feature/alert-mail-subject` (`d99e88e`), тег `v0.6.124`, сборка на билд-хосте (6 артефактов, 0 ошибок), релиз, api-диджест `sha256:094d8802…` совпал с загруженным бинарём. Бэкап `/opt/api/api.bak-0.6.123-20261109-155846`, `active`, healthcheck `Ok (v0.6.124)`. Проверка: `POST /v1/admin/alerts/test` → `{"delivered":true,"email":true,"telegram":null}`; реальные события после рестарта — `pl.frkn.org` → `route=global`, `delivered=t` (письмо с темой `[FRKN] pl.frkn.org`), личные ноды → `quiet`.
- 2026-11-08 — **тема письма = хост, алерты собираются в треды — **выкачено, api 0.6.124****. Раньше темой была первая строка текста, а там IP — в почтовом клиенте список выглядел как `89.125.79.221`. Теперь `email_subject(host)` = `[FRKN] <host>` (постоянная для хоста, поэтому клиент группирует все его алерты в один тред), правило остаётся в теле. Плюс заголовок `References: <frkn-alerts-<host>@frkn.org>` — одинаковый корень на все алерты хоста, чтобы тредили и клиенты, которые игнорируют тему. Хост чистится от всего, кроме `[A-Za-z0-9._-]`, чтобы не собрать битый заголовок. Тесты: стабильность темы по хосту, санитайз корня треда; всего 192 passed.
- 2026-11-08 — **телеграм выключен, алерты идут только почтой**. `[alerts] telegram_enabled = false` в `/opt/api/config.toml` (токен и chat_id на месте, чтобы включить обратно одной строкой), бэкап конфига `/root/frkn-prod-backups/20261108-200427/api-config.toml`, рестарт, healthcheck `Ok (v0.6.123)`. Живая проверка: `POST /v1/admin/alerts/test` → `{"delivered":true,"email":true,"telegram":null}`; после рестарта `alert_events` по `ee.frkn.org` (core) → `route=global`, `delivered=t` (уже письмом), по `custompersonal…` → `route=quiet`, `delivered=f`. Ограничение: алерты владельцу selfhosted-ноды (`route=owner`) тоже телеграмные, поэтому с выключенным телеграмом они не уходят — при возврате к этой фиче нужен либо `telegram_enabled` для владельцев отдельно, либо почта владельца в `subscription_alert_chats`.
- 2026-11-08 — **почта включена как второй канал алертов на проде**. В `/opt/api/config.toml` добавлена секция `[alerts.email]` (SMTP Яндекса скопирован из mrkting: `smtp.yandex.ru:587`, STARTTLS, `pigeon@frkn.org`, пароль приложения 16 символов), получатель `pigeon@frkn.org`; TOML провалидирован до установки, бэкап конфига `/root/frkn-prod-backups/20261108-195226/api-config.toml`, рестарт, healthcheck `Ok (v0.6.123)`. Живая проверка `POST /v1/admin/alerts/test` → `{"delivered":true,"telegram":true,"email":true}`: алерт уходит в оба канала сразу.
- 2026-11-08 — **api 0.6.123 на проде: тихие personal-ноды, почта, алерты владельцу**. Мердж `feature/alert-channels` (`69ae5d2`), тег `v0.6.123`, сборка на билд-хосте (6 артефактов, 0 ошибок), релиз с ассетами, api-диджест `sha256 d4e69faf…` совпал с загруженным бинарём. Миграция `0016` применена (`subscription_alert_chats`). Деплой: бэкап `/opt/api/api.bak-0.6.122-<stamp>`, `active`, healthcheck `Ok (v0.6.123)`, `alerts: started (offline>240s, …)`, `alerts: seeded 63 known heartbeats`. Живая проверка маршрутизации: `alert_events` новых записей несут `details.route` — `ee.frkn.org` → `global`/`delivered=t`, все `custompersonal…` → `quiet`/`delivered=f` (в телегу не уходят). 226 ERROR в журнале — старый шум `ensure connection … Subscription is not active`, к алертам не относится.
- 2026-11-07 — **алерты по selfhosted-нодам уходят владельцу + тумблеры каналов**. Новая таблица `subscription_alert_chats` (миграция `0016`, плюс `ensure_table` в api): `PUT/DELETE/GET /admin/api/subscriptions/{id}/alerts/telegram` с `chat_id`. Маршрутизация в `monitor_alerts`: env `custompersonal<uuid>`/`personal<uuid>` → `personal_owner()` достаёт подписку; если у неё привязан чат, алерт идёт туда (`route=owner`), иначе только в лог (`route=quiet`, поле в `details.route`). Наш общий чат такие ноды больше не будят. Каналы получили независимые тумблеры `[alerts] telegram_enabled` и `[alerts.email] enabled`: выключить канал можно, не удаляя токен/SMTP. Тесты: `personal_owner` (оба префикса и мусор), нормализация chat_id (числа, `-100…`, `@name`, строка с пробелами), независимость тумблеров; всего 190 passed.
- 2026-11-07 — **api: личные ноды пишем в лог, почта как канал алертов (0.6.123, не выкачено)**. `[alerts] quiet_env_prefixes` (по умолчанию `["personal", "custompersonal"]`): по таким env событие по-прежнему попадает в журнал и в `alert_events` (с `details.quiet = true`), но push не уходит — «уведомляем только наши core и premium машины». Второй канал — секция `[alerts.email]` (SMTP через lettre, rustls, тема = первая строка сообщения). Каналы **независимы**: алерт уходит и в телеграм, и на почту, если настроены оба; `can_deliver` = включено и есть хотя бы один канал. Тесты: фильтр env, настраиваемые префиксы, независимость каналов, всего 187 passed. Документировано в `src/bin/api/api-example.toml`.
- 2026-11-07 — **алерты: почта как второй канал, оба одновременно**. `POST /v1/admin/alerts/test` переписан: принимает целиком `AlertsConfig`, шлёт во все настроенные каналы сразу и отвечает по каждому (`{"telegram":true,"email":true,"delivered":true}`), 502 если не дошло никуда. Watchcat-события (`/v1/node/watchcat/event`) и drift снапшота (`/v1/node/snapshot/drift`) больше не ходят напрямую в `send_telegram` — тоже через `notify()`, то есть тоже уходят в оба канала. `channel`-переключателя нет: каналы независимы, работают все, что настроены. Добавлен `NotifyReport` + тест на раздельный учёт каналов.
- 2026-11-07 — **api 0.6.122 на проде: IP в первой строке алертов**. `main` запушен (`2f92b18..efca0cd`, 4 коммита), тег `v0.6.122`, сборка на build-хосте (0 ошибок, 6 артефактов), GitHub-релиз с ассетами, api-диджест `sha256 2980c4cc…` совпал с загруженным бинарём. Деплой: бэкап `/opt/api/api.bak-0.6.121-20261107-150317`, рестарт, healthcheck `Ok (v0.6.122)`, journal `alerts: started (offline>240s, recovery 120s, …)`, 0 ERROR. Живое доказательство формата: первые же `alert_events` после рестарта (15:03:27–15:03:58, `delivered=t`) несут `details.ips` — `eu01.frkn.org` с двумя адресами, `vps-5087` с одним; текст сообщения начинается с этого списка (`alerts.rs:705-745`, `tasks.rs:1082`). Побочный эффект рестарта: 10 алертов по 5 реально мёртвым нодам (777, NL777, olympic-magenta, vps-5087, eu01), узлы восстановились 52 online / 5 offline. Миграций между 0.6.121 и 0.6.122 нет.

- 2026-11-06 — **SNAPSHOT-SYNC: full sync завершается только по подтверждённому count (fnode 0.6.122, локально)**. `sync_connections` возвращает `Applied/EmptyFull/NotModified`; `FullSyncTracker` хранит `seen + expected`, prune запускается только когда числа совпали. Пустой full sync (`304`) теперь чистит тег, ошибка старта снимает pending, частичный батч не может удалить живые коннекты. Восстановление снапшота ждёт завершения всех `restore_*`, повторные `Create` для уже восстановленных пиров пропускаются, чтобы full sync не падал на «already exists». Тесты: lib 110, api 181, node 7 (включая 4 tracker-теста), mesh 10. Полный `cargo test --offline` зелёный. API-код не менялся; версия `0.6.122` поднята для будущей сборки ноды. Коммит `0e3c1a4`.
- 2026-11-06 — **задача A: снапшот fnode сверяется с api (0.6.121, собран)**. Снапшот v2 несёт
  host/env/pool/счётчики по тегам; чужой env/pool — фатальный выход. На старте считается
  `/connections/count` по env+тегу, расхождение >5 % или >20 записей → файл в `.stale-<ts>` и
  полный синк. Первый полный список по тегу делает prune (удалённые пока нода лежала коннекты
  больше не возвращаются). Write-through снапшот на каждое применённое сообщение (коалесинг),
  часовой drift check → `/v1/node/snapshot/drift` → Telegram, `--reset-snapshot`, дамп настроек
  больше не печатает api-токен. Тесты: api 181, lib 110 (+4 identity), node 3 (snapshot cfg).
  **api 0.6.121 выкачен на прод** (healthcheck Ok, sha `2b415e20…`, 0 ERROR; `/connections/count` отдаёт dev: Wireguard 1060, AmneziaWg 1320, AmneziaWgMobile 1383). **fnode 0.6.121 ещё не выкачен** — нужен рестарт сервисов на нодах пула.

- 2026-11-06 — **0.6.120: `409 pool_key_mismatch` устранён**. Инбаунды сравниваются как множество
  по `tag` и только по клиентским полям (порт, адрес, dns, keepalive, mtu, публичный ключ по
  отпечатку, весь `awg_obfuscation`, `sni`, `stream_settings`); имена интерфейсов и порядок
  больше не конфликтуют. Ответ — `pool_inbounds_conflict` с `differences[]`; нода регистрируется
  всегда и получает `degraded`, data-plane не отрубается; флаг `pool.strict_inbounds`.
  Миграция `0015` (enum `node_status` + `degraded`). Приёмка на проде: рестарт обеих нод РФ-пула —
  0×409, `awg1` 1410/1387, `wg0` 1082/1076 (≤2 %), новый коннект `10.100.18.2/32` появился в `wg0`
  на обеих нодах за 70 с.

- 2026-11-05 — **`POST /v1/nodes/load` на проде (0.6.118)**: доля занятости 0..1 по среднему
  за 60 с (`max(bandwidth, 0.6·conn + 0.4·cpu)`, порог 0.85 → 1.0), нет ключа = нет данных.
  Полоса — только дефолтный интерфейс (иначе awg/lo двоят трафик); коннекты — по
  `user.traffic.online` (свежий handshake), у старых fnode — медиана по остальным нодам.
  Живая проверка: msk-tw 0.01, msk-tw-nvsb 0.16, повторный запрос — 429.
  Контракт — `docs/nodes-load.md`.
- 2026-11-05 — **веса адресов живые**: api 0.6.117 на проде, fnode 0.6.117 на msk-tw и
  msk-tw-nvsb, `node_weights` 75/75 и 25, колонка `nodes.weight` удалена (0014).

- 2026-11-06 — **0.6.119 на проде**: `node_weights` теперь есть в `/v1/config` (раньше только в
  `/v1/services`, где нет адресов — клиент не мог связать вес с адресом), `POST /v1/nodes/load`
  принимает `node_ips`; живая проверка RU-пула: 3 адреса, веса 75/25/75, load 0.01/0.08/0.01.
- 2026-11-05 — **api 0.6.117 на проде: веса адресов**. Healthcheck `Ok (v0.6.117)`, бинарь
  sha256 `9061ae31…` = ассет релиза `v0.6.117`, бэкап `/opt/api/api.bak-0.6.116-…`, 0 ERROR в
  журнале. Колонка `nodes.node_weights` (jsonb) создана самим api при старте; у всех 9 нод в пулах
  значение `NULL` — их fnode старых сборок поле не шлют, поэтому пока все адреса весят 100.
  Миграция `0014_drop_node_weight.sql` (drop старой колонки `nodes.weight`) **не применена** —
  упёрлась в approval-гейт; на работу не влияет (новый api колонку не читает и не пишет).
  Дальше для живых весов: fnode ≥ 0.6.117 на ноды пула + `[node] node_weights` в конфиге.
- 2026-11-03 — **веса адресов пула (`node_weights`) вместо веса ноды**. Вес ноды (`nodes.weight`, `[node] weight`) убран совсем: теперь вес живёт только у адреса — `[node] node_weights = { "1.2.3.4" = 75 }` в конфиге ноды, карта `{"ip": weight}` в `NodeRequest`/`nodes.node_weights JSONB`, в ответе клиенту рядом с `node_ports` (`node_weights{ip: w}`). Дефолт — **100** (0/отрицательный = «не задан», старая нода шлёт 0), веса могут совпадать, ключи карты всегда равны ключам `node_ports`. Обратная совместимость: поле `weight` в ответе сохранено как легаси (= максимум по адресам) — старые клиенты без числового `weight` выбрасывают сервер (`client/mozilla/models/server.cpp`); новое поле аддитивное, старые сборки его не видят. Миграции: `0013_node_weights.sql` (аддитивная, api добавляет колонку сам в bootstrap) и `0014_drop_node_weight.sql` (**только после** деплоя 0.6.117). Контракт для клиента: `docs/pool-weights.md`. Тесты: bin api 167 (+2), lib 106. На прод не выкачено.
- 2026-11-01 — **`/connections/sync`: guard по токену ноды**. `node_`-токен по-прежнему не mgmt, но получает свой sync: новый фильтр `with_connections_sync_auth` (`src/bin/api/http/filters.rs`) сверяет токен с `node_access_tokens` и отдаёт uuid ноды, хендлер берёт env **из записи ноды**, а не из тела (`sync_env`, `handlers/connection.rs`) и публикует только в `Topic::Init(своей ноды)`. Для ноды с `node_`-токеном hysteria2 тоже фильтруется по env. Сервис/админ-токен — как раньше (env из тела). Причина: приватная нода с `env = "dev"` в конфиге тянула пиров dev. Тесты: bin api 165 (+4 на `sync_env`), lib 106. **Выкачено 2026-11-03**: api **0.6.116** на проде (`Ok (v0.6.116)`, sha256 `75123cc9…`, релиз `v0.6.116` с ассетами); после рестарта 0 ERROR, ноды переводятся в Online, `connections_sync_env_rewritten` — 0 (приватная нода ещё не синкалась).
- 2026-10-30 — **0.6.104: страна пула на карточке (`pool_country_code`)**.
  Клиент добавил `poolCountryName` для подписи под флагом: раньше она всегда
  бралась у ноды, и у пула получалось «флаг ЕС, подпись Poland». Отдаём ровно
  два поля рядом с `pool`/`pool_translation`:
  `pool_country_code` (ISO-код, обязателен для пула) и `pool_country_name`
  (опционально, пусто = клиент переводит код сам). Поля добавляются **только**
  на карточке пула и в `available_countries`; у одиночной ноды их нет — там
  страна уже в `country_code`, и прежнее поведение не меняется.
  Источник значения: нода-участник объявляет страну пула в своём конфиге
  (`[node] pool_country_code` / `pool_country_name`) — по аналогии с
  `[node] pool_translation`. Проброс: `NodeConfigRaw` → `NodeConfig::from_raw`
  (код нормализуется: trim, upper, только 2–3 латинские буквы, иначе отбросить) →
  `NodeRequest` → `fcore::Node` → `nodes.pool_country_code/pool_country_name`
  → карточка. Миграция `deploy/migrations/0009_pool_country.sql`.
  Тесты: 149 bin api (+3: поле в JSON, страна в `available_countries`, отсутствие
  полей при пустом значении) + 104 lib.
  **Чтобы поле реально появилось в проде:** нужен бинарь ноды ≥ 0.6.104 (старые
  не шлют новые поля при регистрации) и `[node] pool_country_code = "EU"` в
  config.toml хотя бы на одной ноде каждого пула; `COALESCE` сохранит значение,
  когда его пришлёт любая нода пула.
- 2026-10-30 — **0.6.103: новый формат отчётов телеметрии (клиент 4.8.14.79+)**.
  Клиент перестал слать `report_id` + один `target` и присылает отчёт целиком:
  туннель выключен → `targets[]` (состояние каждого адреса) + `attempts[]` (что
  реально произошло: исход, код, сколько ждали); туннель включён → `mode:"tunnel"`,
  `target`, `tunnel`, `session`, `mtu`. Проверено подстановкой реальных тел в
  структуры: **оба отбивались 400** (`missing field report_id` / `scan_id`) —
  то есть новые отчёты не доходили вообще. Сделано:
  (1) `/v1/report/block` принимает **оба** формата: тело разбирается как `Value`,
  старый (`report_id`+`report`) уходит в прежний хендлер без изменений, новый — в
  `parse_rich_report`; старые сборки клиента не затронуты;
  (2) **upsert, не `DO NOTHING`**: клиент обязан повторять один и тот же
  `report_id`, и последняя версия должна побеждать. Unique-индекса, который
  отдавал бы 409, нет — `ON CONFLICT (id) DO UPDATE` по отчёту (payload, config,
  mtu, tunnel, network, session), по попыткам (outcome, detail, waited_ms) и
  отдельный `upsert_observations` для наблюдений. Наблюдения не идут через
  `insert_batch`: тот пропускает весь батч, если `batch_id` уже встречался, а
  повтор может принести **больше** таргетов (первая отправка оборвалась).
  В ответе `created: true` = запись новая, `false` = обновлена повтором.
  (2а) **`report_id` обязателен в обоих формах** — он и есть идемпотентность.
  Выдумывать его на сервере нельзя: повтор после правки одного поля стал бы
  новым отчётом, а настоящий повтор с тем же id мы всё равно не отличили бы.
  Нет поля → `400 missing_report_id`, не-uuid → `400 invalid_report_id`
  (внятные коды, не общий «bad request»). Строки журнала попыток получают id
  от `(report_id, индекс, at)` — это ключ строки, не идентичность отчёта;
  (3) две новые таблицы — `client_reports` (отчёт целиком: `mode`, `app`, `config`,
  `mtu`, `tunnel`, `network`, `session`, `payload`, гео клиента) и
  `connection_attempts` (журнал попыток: адрес, порт, протокол, `outcome`,
  `detail`, `waited_ms`); миграция `deploy/migrations/0008_client_reports.sql`;
  (4) наблюдения из `targets[]` и цели режима tunnel идут в `block_observations`
  (`source='targets'` / `'tunnel'`), чтобы прежние агрегаты продолжали считать;
  (5) **словарь состояний**: клиент присылает `ok`/`unknown`/`error`, а счётчики
  знали только `open`/`answered`/`timeout`/... — новые отчёты ложились бы в базу,
  но ни в один счётчик, и дашборд выглядел бы пустым. `OK_STATES += ok`,
  `FAIL_STATES += error`, `INCONCLUSIVE_STATES = no-answer|unknown`, агрегат
  получил `$7`; тест `client_vocabulary_lands_in_counters` проверяет, что словари
  не пересекаются;
  (6) `GET /v1/admin/report/reports?support_tag=…&subscription=…` — для поддержки:
  отчёт + его попытки, то есть ответ на «кто прислал и что не так».
  Битая запись в `targets[]` (плохой адрес/порт) отбрасывается со счётчиком
  `targets_rejected`, ответ остаётся 202 — не 500.
  Тесты: 146 bin api (было 132) + 104 lib; сэмпл содержимого —
  `cargo test --bin api report_ingest_sample -- --nocapture`.
  **Контракт (сверено с клиентом 4.8.14.79):**
  - `report_id` — uuid, один на отчёт, генерируется при сборке отчёта, при
    переотправке **не меняется**, приходит на верхнем уровне тела в **обоих**
    режимах (`/report/block` и `/report/scan`). Внутри `report`/`scan` дублировать
    не нужно — api читает корень;
  - `app` (`version`/`platform`/`os`) — на верхнем уровне в обоих режимах;
    внутри `report`/`scan` остаётся как было (старые сборки читаются оттуда);
  - новый формат принимается **на обоих** маршрутах: диспетчер разводит по
    вложенному объекту (`report` → старый блок, `scan` → старый срез, иначе
    новый формат), поэтому `report_id` в корне работает и там, и там;
  - лимит тела `/report/block` поднят 16 → 64 КБ: отчёт стал пачкой
    (`targets[]` + `attempts[]`), десять таргетов в 16 КБ уже не влезали.
  Трафик (`session.last_no_data.rx_bytes/tx_bytes`) сохраняется как есть внутри
  `session` jsonb — это клиентская диагностика, а объёмы мы и так считаем.
- 2026-10-30 — **0.6.102: `node_ports` — порт конкретного адреса пула**.
  Внутри одной ноды несколько IP — это один интерфейс, порт у них общий; между
  нодами одного пула порты могут различаться (443 / 8443 / 51820). Плоского
  `node_ips` + одного порта не хватало: клиент не знал, куда дозваниваться.
  Добавлено поле `node_ports` — объект `{адрес: порт}`, ровно в том виде, в каком
  его читает клиент (`servers_model.cpp::portForNodeIp`; нет ключа → порт
  протокола). `node_ips` остаётся единственным списком адресов, `node_ports` — его
  дополнение; оба считаются из одной карты (`pooled_node_ports`), поэтому
  разъехаться не могут. У одиночной ноды поля нет (порт один и он уже в конфиге).
  Нода без нужного инбаунда не попадает ни в `node_ips`, ни в `node_ports` —
  адрес, где никто не слушает, клиенту бесполезен (прежнее поведение отдавало его).
  Версия карточки пула (`config_rev_pool`) хеширует адреса + порты + серверный
  материал каждого члена: иначе смена порта или ключа не заставила бы клиента
  перечитать конфиг. Версия каталога (`catalog.rs`) тоже учитывает `ip:port` по
  каждому протоколу — раньше смена порта не поднимала версию фида.
  Живые данные: EU-пул — 6 нод, 10 адресов, у всех 443 (фича про запас);
  `prem-nl-01` — один hostname, 5 разных портов hysteria2 = 5 отдельных нод.
  Тесты: 132 bin api (+9).
- 2026-10-30 — **0.6.97/0.6.98: входы (с какого адреса клиент подключён)**. Агент читает `endpoint` пира — `peer.endpoint` в `defguard_wireguard_rs` для WG и `WGPEER_A_ENDPOINT` для AWG (`PeerStats.endpoint`) — и шлёт разовые события в новом топике `Topic::Entries` (`EntryEnvelope`), а не метрикой: ключ серии в `MetricStorage` — хеш `(имя+tags)`, и каждый новый IP плодил бы новую серию. Дедуп на ноде: тот же адрес по тому же коннекту уходит при смене либо раз в 30 мин (`entry_seen`). api в `MetricWorker` обогащает GeoLite2 (ASN+страна), режет `/24` (`/48` для IPv6), хеширует `sha256(salt+ip)[..8]` и пишет в `entry_usage`. Живая проверка: **612 входов с msk-tw, 27 с pl, 3 с cz2**; по странам RU 593 / UA 10 / NL 6 / US 4, по ASN AS12389 77 / AS8359 49, полный IP не хранится (0 строк без `/24` или `/48`).
  Две ошибки по ходу, обе пойманы на проде: (1) node собран **без фич** (`default = []`) — такой бинарь не управляет туннелями вообще, откатил cz2/pl/msk-tw на бэкапы и пересобрал с `--features wireguard,amnezia-wg`; (2) батч входов падал на `ON CONFLICT DO UPDATE command cannot affect row a second time`, когда в одном батче два адреса из одной `/24` — свёртка `GROUP BY (node_id, subscription_id, entry_prefix)` в CTE.
- 2026-10-29 — **0.6.96: heartbeat залипал в offline при нескольких сериях `sys.heartbeat`**. Ключ серии в `MetricStorage` — `hash(имя + теги)`, а `note_heartbeat` (`http/handlers/private.rs:324`) писал `sys.heartbeat` с **пустыми** тегами при каждой регистрации приватной ноды → вторая, навсегда замороженная серия рядом с живой. `get_last_heartbeat` (`metrics/storage.rs:390`) делал `return` на первой серии с этим именем, порядок обхода `DashMap` произвольный → `now - ts` рос без предела → `is_offline = true` навсегда, `node_heartbeats.last_ms` не двигался, нода выпадала из конфигов подписок через 300 с. Фикс: максимум по всем сериям + `note_heartbeat` пишет с `node.get_base_tags()`. Регрессионный тест `test_get_last_heartbeat_ignores_stale_series` (падал бы на старом коде). Тесты lib 104, bin api 117. Инцидент и разбор: `env/dev/servers/cz2.frkn.org/incident-2026-10-29-heartbeat.md`.
- 2026-10-29 — **0.6.94/0.6.95: `GET /healthcheck` на ноде** (под токеном, для суппорта и будущего выбора ноды в пуле). Отдаёт `node_id, hostname, env, version, uptime_sec, cpu_pct, mem_used/total, load 1/5/15, disks[], ts`; ничего про клиентов. Токен — `[metrics] healthcheck_token` (заголовок `X-Health-Token` или `Bearer`), сравнение постоянного времени, пустой токен = ручка закрыта (401). Живёт на том же порту, что `/metrics`; на cz2 поднят на `0.0.0.0:8099`. Публичный `/healthz` оставлен как был. Проверено на cz2: 401/401/200/200 (последний — снаружи). В 0.6.95 починен `uptime_sec` (был 0 — точку старта инициализировали лениво; теперь `mark_process_start()` в `main`).
- 2026-10-29 — **0.6.93: `GET /v1/admin/report/observations`** — сырые наблюдения телеметрии блокировок по подписке (для поддержки: «клиент говорит, что отправил отчёт — что у него не работает»). Отдаёт время, приложение/платформу, цель, протокол/транспорт, состояние, RTT, ICMP, ноду, `support_tag`, страну по ASN. В дашборде (`0.2.51`) — вкладка «Телеметрия» с поиском по ID подписки и кнопкой «Последние 50».
- 2026-10-28 — **0.6.78: телеметрия блокировок в проде**. `block_observations` (одна таблица на отчёты и сканы), идемпотентность по `report_id`/`scan_id` и окну 10 мин, лимиты 16/128 КБ + 10/мин на IP + 30/час на подписку, geoip ASN/страна оффлайн из GeoLite2 (`/opt/api/GeoLite2-*.mmdb`), суточная ретенция 30 дней. Найдено на проде и починено: сериализация `cidr`/`char(2)` (касты в INSERT), `ip_hash` отсутствовал в CTE агрегата. Админ-ручка закрыта admin-токеном.
- 2026-10-27 — **block-telemetry Э0+Э1** (ветка `feat/block-telemetry`): таблица `block_reports` (+ индексы addr/time, asn/time, sub/time), `insert_once` с `ON CONFLICT (id)`, `count_recent` для дедупа по окну, `aggregate()` с вердиктом на лету; хендлер с валидацией контракта, ответами 202/400/401/413/429, `SubRateLimiter` 30/час на подписку; `src/geoip.rs` — GeoLite2 ASN+Country в `OnceLock`, конфиг `geoip_asn_path` / `geoip_country_path`. Базы лежат на проде в `/opt/api/GeoLite2-{ASN,Country}.mmdb` (8.6M/3.2M, sha сверены), в гит не коммитятся. **В `config.toml` пути пока НЕ добавлены** — старый бинарь 0.6.74 их не читает, добавляем при выкатке.
- 2026-10-25 — **0.6.74: первые 50 адресов каждой подсети больше не выдаются**. Аллокатор (`connection.rs`, `premium.rs`) стартует с `network + RESERVED_PREFIX + 1` (`IpAddrMask::RESERVED_PREFIX = 50`, `src/memory/connection/wireguard.rs`), а если счётчик откатился ниже резерва (восстановление из бэкапа) — поднимается до резерва. Причина: на хостах собственные awg-интерфейсы стоят на `100.64.0.1-.4` и `10.77.0.1-.4`, а пул выдавал юзерам ровно эти адреса. Тесты: 101 lib (2 новых на резерв). Прод `Ok (v0.6.74)`. **Не сделано:** 7 живых коннектов всё ещё сидят в зарезервированном диапазоне (3 AWG `100.64.0.2-.4`, 4 мобильных `10.77.0.1-.4`) — их замена требует фикса `Action::Update` в ноде (`node/tasks.rs:127` возвращает «AWG user already exists» вместо замены allowed-ips).
- 2026-10-25 — **0.6.73: трафик слота в инвентаре**. Для каждого слота с владельцем отдаём `traffic_up` / `traffic_down` / `traffic_total` — это суммарный трафик подписки-владельца (`traffic().total_for_subscription`), отдельного счётчика на слот нет. На проде: 21 из 26 слотов с трафиком, крупнейший ~203 ГБ вниз. Прод `Ok (v0.6.73)`.
- 2026-10-25 — **свои юзеры доступны любому аккаунту (0.6.72)**. Снят гейт `plan_kind == Selfhosted` в создании child-подписок (`private.rs`, бывшая ошибка «not a selfhosted account»): теперь границу задаёт только `user_limit` (по умолчанию 20, админ поднимает через `PUT /admin/api/subscriptions/{id}/selfhosted-limits`), а child-аккаунт по-прежнему не может приглашать своих. Ноды к обычному аккаунту подключались и раньше — `POST /private/nodes` не смотрит на план, `ensure_personal_scope` создаёт personal-энв; в UI кнопка «Своя нода» есть у всех планов (проверено скриншотом на standard-подписке: команда установки выдаётся). Прод `Ok (v0.6.72)`. Живьём создание юзера на standard-подписке не прогонял — POST заблокировался на подтверждении.
- 2026-10-25 — **prem1…prem5 попали в премиум-инвентарь** (0.6.71). Причина, почему их не было в дашборде: на prem1 пять слотов живут personal-энвами (`personal<uuid>`), а инвентарь фильтровал только `custompremium-*`. Попытка через тип ноды не удалась: в конфигах поставил `type = "premium"` и перезапустил пять fnode, но **fnode не передаёт type при регистрации** — в БД `node_type` остался `common` (строка обновилась в 23:07, тип нет). Итог: фильтр по имени хоста `prem*` (плюс прежний `env.is_premium()`), а владельцем слота считается любой `scope_env`, не только premium (0.6.70). Проверено: инвентарь отдаёт 26 слотов, 21 assigned, 5 free; prem1…prem5 видны с `subscription_id` и статусом ноды (prem4 — offline). Конфиги на prem1 оставлены с `type = "premium"` (инертно, но семантически верно).
- 2026-10-24 — **0.6.68: инвентарь премиума из нод + резка слота по сроку**. `GET /admin/api/premium/inventory` (admin token) строит список слотов из нод: премиум-энв объявляет сама нода при старте (`custompremium-…`), реестр не нужен. На выходе `slots[]` (env, `subscription_id`, `premium_until`, `expired`, живые/всего коннектов, узлы с hostname/address/label/country/online) и `totals` (slots/assigned/free/expired); `free` = у энва есть нода, но ни одна подписка на него не ссылается. Блокировка: `cleanup_expired_subscriptions` теперь режет коннекты премиум-энва, когда истёк `premium_until`, а подписка ещё жива — тем же `SyncOp::delete_connection(DELETE_REASON_EXPIRED)`, что и полная просрочка; `scope_env` и `subscription_id` остаются, чтобы поддержка видела, кто держал слот. Прод: `Ok (v0.6.68)`, инвентарь отдаёт 20 энвов (15 assigned, 5 free, 0 expired).
- 2026-10-24 — **0.6.67: отдельный срок премиума** (код готов, **не выкачен**). Миграция `0006_premium_until.sql`: колонка `subscriptions.premium_until`, бэкфилл `= expires_at` для живых `custompremium-*`. В коде: поле в `Subscription` + `premium_until()/set_premium_until()`, чтение в `from_row`, и `ScopeInfo.premium_until` в `GET /subscription/{id}` — фронт рисует плашку премиума по нему, а не по сроку подписки. mrkting править не нужно: этот путь идёт напрямую в fcore (nginx `location /` → api). Тесты `--lib` 99, `--bin api` 97. Смерджено `--no-ff`, тег `v0.6.67`, запушено. **Блокер:** `us.frkn.org` (82.26.93.183) не отвечает по ICMP и SSH, сборка сорвалась на `ssh: connect ... timed out`; миграцию на прод не накатывал — её надо применять вместе с бинарём.
- 2026-10-23 — **0.6.66 на проде: карточка «Серверы» у премиум-подписок** (шаг 1 из плана). Причина пропажи: кабинетные read-ручки считали env как `Env::personal_for(subscription_id)`, а премиум владеет нодой на `custompremium-<слот>`, поэтому `/private/nodes/stats` отдавал пусто и карточка (`selfhosted-servers.js` прячется при пустом списке) исчезала. Добавлен `Env::is_premium()` и `owned_env(sub, id)` — берёт `scope_env`, если он personal или premium, иначе падает назад в personal; применён в `list_private_nodes`, `private_node_stats`, `owned_node` (через него series) и `private_state` (счётчик серверов). Провизioning-ручки намеренно оставлены на `personal_env`. Релиз: revert премиум-мёржа `36f6b19` (`78347da`) → фикс `76a7257` → merge `b22cb92` → тег `v0.6.66` → сборка на `us.frkn.org` → бэкап `/opt/api/api.bak-0.6.64-*` → рестарт → `Ok (v0.6.66)`. Проверено: `prem02-4` (`d2cd1be7`) `/private/nodes/stats` → нода `prem02-4.frkn.org / 195.133.80.165`; обычная подписка → `[]`; в браузере карточка «Серверы 1» с онлайном, CPU/RAM/диском/сетью и трафиком.
- 2026-10-22 — **заблокирована lite-подписка** `d6a469c4…` (1 ГиБ, 11 коннектов dev/ru) через `DELETE /admin/api/subscriptions/{id}`: `is_deleted=t` в БД, все 11 коннектов `is_deleted=t`/`deleted_reason=manual`, в памяти api `is_active=false`, на `cz2` пиров по её AWG-адресам нет. Отдельного флага «block» в ffcore нет: у lite (`expires_at NULL`) единственный рычаг — `is_deleted`; обнуление `limit_bytes` не блокирует (реконсайлер пересоздаёт коннекты, т.к. `is_active()` для lite = true). Прямой SQL не виден api до `db_sync_interval_sec` (1000 с), через admin API — сразу. mrkting при удалении не уведомляется — платёжная запись/ЛК остаются.
- 2026-10-22 — **проверены добавление/удаление коннектов на живой ноде** (`cz2.frkn.org`, personal env подписки `c6831dad…`, api 0.6.62): `POST /connection` (AmneziaWg) → новый пир на `awg0` за ~5с, `allowed-ips` = выданному адресу; `DELETE /connection` → пир снят (2 → 1 пир, держится 60с), строка `is_deleted=t`, `deleted_reason=manual`. Hysteria2: `/auth` с токеном созданного коннекта → `{"ok":true,"id":…}`, после DELETE → `{"ok":false}` — отзыв работает. Сверка ноды с БД сошлась точно: 4 активных коннекта env ↔ 4 пира на `awg0`+`awg1` по allowed-ips, сирот нет. Найдено два дефекта: (1) повторный `POST /connection` на ту же пару `(subscription, env, proto)` отдаёт **500** с сырым текстом ошибки БД (`connections_single_proto_idx`) вместо 409; (2) env с лишним префиксом (`custom` + `custompersonal…`) принимается молча — коннект создаётся в env, которого нет ни на одной ноде, и hysteria-токен при этом выдаётся.
- 2026-10-22 — **api 0.6.62 выкачен на прод** (`141.133.173.16`, `/opt/api/api`, systemd `api`): релиз опубликован из готового билда на билд-сервере (`gh release create` не доехал 21.10 после сборки в 23:15, пересборки не было), бэкап `api.bak-0.6.60`, `healthcheck` → `Ok (v0.6.62)`. Миграций между 0.6.60 и 0.6.62 нет. 15 нод ушли в Offline на рестарте и все вернулись Online; метрики нод обнулились (in-memory).
- 2026-10-21 — проверка вторым прогоном: шарить personal/selfhosted env теперь можно и без живого коннекшена — `owns_env = scope_env == env || есть коннекшен в env` (env принадлежит подписке по построению). Общие env по-прежнему требуют коннекшен. `cargo test --lib` 99, `cargo test --bin api` 96 — зелёные.
- 2026-10-21 — 0.6.62 (локально): shared subscriptions. `share_tokens.kind` (`conn`/`sub`), `source_connection_id` nullable; `POST /share/sub` (mgmt) `{subscription_id, env, proto}` → `frkn://sub/shared/<uuid>`, child-коннекшен `issued_via='shared_sub'` без метки, мят идемпотентен по `(subscription, "env · proto")`, шарить можно только env, который реально есть у подписки; `/v1/services` с `sub`-токеном отдаёт все серверы env+proto; `/v1/config` с `sub`-токеном уважает payload `node_id` в рамках env; в `GET /subscription/{id}` добавлен `shared_subs[]`.
- 2026-10-21 — AGENTS.md переписан: карта кода/доков, build-флаги (api/auth без default-features, node с xray/wireguard/amnezia-wg), миграции только на деплое, релиз через server-release.sh, грабли; не закоммичено.
- 2026-10-21 — v0.6.60: env-wide share-токены (`share_tokens.node_id` nullable, `/sub/<token>` отдаёт все онлайн-ноды env, `/v1/config` для env-токена = конфиг выбранной ноды + `node_ips` пула).
- 2026-10-21 — v0.6.59: `GET /private/nodes/stats?subscription_id=` (живые CPU/RAM/диск/сеть + inbound трафик/коннекты) и `GET /private/nodes/{uuid}/series` (даунсемпл ~240 бакетов, 24ч/7д), владение проверяется по personal env.
- 2026-10-21 — имена серверов в `/v1/services`: `Устройство (NL4)`, для пулов `Устройство (Finland)`; безымянные коннекшены — лейбл ноды, как раньше. ` · ` остаётся под суффикс протокола.
- 2026-10-21 — тег `v0.6.59`, GitHub Release, api на `141.133.173.16` (`Ok (v0.6.59)`), бэкап `api.bak-0.6.58-*`.
- 2026-10-21 — почищены ветки: 19 смердженных + 5 старых (они есть на origin).
- 2026-10-20 — v0.6.58: catalog-pool-version + healthcheck-version, api задеплоен.
- 2026-10-20 — v0.6.57: Clash + Hysteria2, собран на us.frkn.org.

## Next
- [ ] **Выкатить fnode 0.6.122 на `msk-tw` канарейкой**: сохранить старый снапшот, проверить `awg show awg1 allowed-ips`, удаление коннекта при остановленной ноде и отсутствие ложного prune. Только после этого — остальные ноды пула.
- [ ] **`/healthcheck` раскатать на остальные ноды**: в `/opt/fnode/config.toml` поставить `endpoint = "0.0.0.0:8099"` (сейчас `127.0.0.1:3000`, снаружи недоступно) + `healthcheck_token`, заменить бинарь `node` на 0.6.95. Сейчас ручка живёт только на `cz2`.
- [ ] **api собирает `/healthcheck` всех нод** (раз в минуту) и отдаёт клиенту пул с `load`/`cpu`/`score` — чтобы приложение выбирало ноду, не стучась в каждую напрямую. Выбор ноды в клиенте честно меряется только **UDP-handshake-пробой** на реальный порт, не HTTP-пингом.
- [ ] поиск в телеметрии не только по UUID подписки, но и по логину/почте (join с подписками) + группировка «все цели одного нажатия Send» (сейчас плоский список).
- [ ] вернуть `fnode 0.6.95` на `cz2` (бэкап `/opt/fnode/fnode-0.6.95.bak-20261029`) — откат был лечением не той причины.
- [ ] ветка `feature/share-cleanup` (не закоммичена, тесты не гонялись): миграция `0004_conn_invariants.sql` (исключить `issued_via='shared_sub'` из `connections_single_proto_idx` + `connections_token_uniq`), идемпотентный `POST /connection` (200 + существующий id вместо 500/409), `Env::Deserialize` через одну нормализацию префикса `custom`, env-гейт `validate_env_served` в `create_connection_inner`. `cargo check` для `--bin api` и `--bin auth` зелёный; `cargo test` и деплой — по отдельному согласию.
- [ ] решить по `share_feed_url`: сейчас `base_url = https://frkn.org` даёт `https://frkn.org/sub/<token>`, а nginx отдаёт там HTML кабинета; сырой фид живёт на `api.frkn.org/sub/<token>`.
- [ ] решить, персистить ли метрики: ответ — метрики нод живут в rkyv-снапшоте `metrics.snapshot_path` (`/opt/api/snapshots/metrics.*`, ~1.78 ГБ), в БД их нет; `node_metrics` пустая, читать её нечем.
- [ ] **чистка БД по плану `docs/db-cleanup.md`**: `0023_drop_unused.sql` (не накатана) сносит `node_metrics`, `node_metrics_1m`, `grafana_metrics`, `digest_runs`; отдельно решить по `pools` (пустая, `shard_overview.pools` от неё пуст) и по путям `client_reports`/`connection_attempts` (писатель есть, строк 0).
- [ ] **`card_id` в `PeerView` + реепинг устройств (0.7.17)**: чтобы клиент после переустановки собирал конфиг без пересоздания пира, в ответ `GET /v1/peers` нужно добавить `card_id`. Он строится как `<env>:<pool>:<proto>`, а `pool` в `peers` не хранится — значит миграция `0025` добавляет `peers.pool text` (nullable), `createPeer` пишет его из `card_id`, `PeerRow`/`PEER_COLUMNS` получают поле, `PeerView.card_id` = `format!("{env}:{pool}:{proto}")` при непустом pool. Второе: таска `reap_stale_devices` — устройство без активности N дней (конфиг `service.peers.device_inactive_days`, дефолт 90) снимается вместе со своими пирами через существующий `revoke_env_access`/delete-путь, иначе сироты после вайпа Keychain копят лимит 45 пиров и адреса шарда.
- [ ] клиент: экран устройств (список, удаление устройства → рвёт его пиры, число пиров на устройстве), и приватный ключ устройства — в Keychain, чтобы штатная переустановка переиспользовала пиры.
- [ ] снапшот метрик: выключить сейв или срезать retention — сначала разобрать содержимое партов скриптом.
- [ ] патч `src/bin/node/tasks.rs` (`SUB: Failed to handle messages`) — применять или нет.
- [ ] проверить живьём шару одной ноды и шару региона на `dev` (api 0.6.62 + mrkting 0.2.42 уже на проде).
- [ ] dopamine: сборка с `frkn://sub/shared/` (локально собирается, не закоммичено).

## Blockers / Notes
- **Билд-хост `us.frkn.org` (82.26.93.183) отваливается по прямому SSH** (22 filtered), `ProxyJump` с ключом перестал пускать — рабочий способ: `ssh -o ProxyCommand="ssh -i ~/.ssh/ed25519_frkn -W %h:%p root@darkmachine2.frkn.org" -i ~/.ssh/ed25519_frkn root@82.26.93.183`.
- Перед сборкой чистить грязный `Cargo.lock` в `/opt/build/src/<repo>`, иначе `build.sh` падает на `git checkout` с `Aborting`.
- Лог api при старте печатает `Settings: ServiceSettings {...}` вместе с токенами и паролем БД — не грепать эту строку целиком.
- **block-telemetry ждёт клиент** — сборка/деплой `v0.6.75` после того, как клиентская часть начнёт присылать отчёты.
- GeoLite2 требует атрибуции («This product includes GeoLite2 data created by MaxMind…») — добавить на сайт/в доки, когда ручка поедет в прод.
- **Обход недоступного билд-хоста: ходить через jump `ProxyJump=root@darkmachine2.frkn.org`** (`darkmachine2.frkn.org` = 91.199.137.92). Прямой SSH до `us.frkn.org` (82.26.93.183) периодически не поднимается, через jump работает. Сборку запускать отвязанно: `ssh -J root@darkmachine2.frkn.org root@us.frkn.org 'cd /opt/build && setsid nohup ./build.sh fcore <tag> > /tmp/build-<tag>.log 2>&1 </dev/null &'` — обрыв SSH её больше не убивает. `server-release.sh` jump не умеет, поэтому `gh release create` делать руками.
- Попутно: в `env/dev/scripts/infra-health/server-check.sh` для `us.frkn.org` записан другой IP (`82.21.92.48`); он отвечает по SSH, но с **другим host key** (не тот хост) — не путать.
- Один конфиг = один сервер: у каждой ноды свои ключи (AWG pubkey / Reality), поэтому env-wide «одна карточка с фейловером» невозможна вне пула.
- `/subscription/{id}` (mrkting) отдаёт `connections[].id`, но `node_id` там нет — для тестов брать id оттуда.
- SSH на прод: ключ `~/.ssh/ed25519_frkn`; GitHub — `~/.ssh/id_rsa_gh_2pizza` (после рестарта агента добавлять `ssh-add`.
- Мерджи только `--no-ff`, версия бампается на ветке.
- Секреты не коммитить.

## Related
- `frkn.org/HUMANS.md`, `frkn-services/HUMANS.md` (Task DEVICES-ENV)
