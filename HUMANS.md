# HUMANS.md

## Current
- **Repo:** ffcore
- **Branch:** `main`
- **Task:** страна пула на карточке (`pool_country_code`), выдача портов, новый формат отчётов
- **Status:** на проде **0.6.104** (healthcheck подтверждает), миграция 0009 применена,
  колонки `nodes.pool_country_code/pool_country_name` есть. Собрано **локально**
  (zigbuild x86_64): билд-сервер `us.frkn.org` был недоступен (нет ping),
  поэтому GitHub-релиза с ассетами нет — только тег.
  Поле появится в выдаче, когда нода с бинарём ≥ 0.6.104 и
  `[node] pool_country_code` в config.toml зарегистрируется.
- **Updated:** 2026-10-31

## Done
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
- [ ] **`/healthcheck` раскатать на остальные ноды**: в `/opt/fnode/config.toml` поставить `endpoint = "0.0.0.0:8099"` (сейчас `127.0.0.1:3000`, снаружи недоступно) + `healthcheck_token`, заменить бинарь `node` на 0.6.95. Сейчас ручка живёт только на `cz2`.
- [ ] **api собирает `/healthcheck` всех нод** (раз в минуту) и отдаёт клиенту пул с `load`/`cpu`/`score` — чтобы приложение выбирало ноду, не стучась в каждую напрямую. Выбор ноды в клиенте честно меряется только **UDP-handshake-пробой** на реальный порт, не HTTP-пингом.
- [ ] поиск в телеметрии не только по UUID подписки, но и по логину/почте (join с подписками) + группировка «все цели одного нажатия Send» (сейчас плоский список).
- [ ] вернуть `fnode 0.6.95` на `cz2` (бэкап `/opt/fnode/fnode-0.6.95.bak-20261029`) — откат был лечением не той причины.
- [ ] ветка `feature/share-cleanup` (не закоммичена, тесты не гонялись): миграция `0004_conn_invariants.sql` (исключить `issued_via='shared_sub'` из `connections_single_proto_idx` + `connections_token_uniq`), идемпотентный `POST /connection` (200 + существующий id вместо 500/409), `Env::Deserialize` через одну нормализацию префикса `custom`, env-гейт `validate_env_served` в `create_connection_inner`. `cargo check` для `--bin api` и `--bin auth` зелёный; `cargo test` и деплой — по отдельному согласию.
- [ ] решить по `share_feed_url`: сейчас `base_url = https://frkn.org` даёт `https://frkn.org/sub/<token>`, а nginx отдаёт там HTML кабинета; сырой фид живёт на `api.frkn.org/sub/<token>`.
- [ ] решить, персистить ли метрики (сейчас рестарт api обнуляет историю графиков).
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
