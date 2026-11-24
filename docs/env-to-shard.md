# env → shard: валидированное имя + purpose вместо enum

Статус: **план, не сделано**. Цель: убрать `enum Env` как гейт, завести шарды данными
(`env_networks`), не сломав провод (JSON/TOML/топики) и не потеряв проверки прав.

## 1. Что есть сейчас

| что | где |
|---|---|
| `enum Env` c вариантами + `Custom(String)` | `src/memory/env.rs:9-22` |
| `FromStr` с белым списком → `standard-ru` отбивается | `env.rs:104-118` |
| `Custom` подклеивает `custom` на выводе, `FromStr` — срезает | `env.rs:35,49,94` / `:114,121-133` |
| `From<&str>` на неизвестной строке → `Env::Experimental` | `env.rs:156-160` (тест `:262`) |
| `Default` = `Experimental` | `env.rs:11` |
| `traffic::parse_env` — то же молчаливое падение в `experimental` | `bin/api/traffic.rs:192-195` |
| `ScopeKind` выводится из варианта `Env::Custom(name)` + префикса | `src/http/response.rs:84-86` |
| `env: Env` торчит в ответах ЛК (`EnvInfo`, `EnvTrafficInfo`) | `src/http/response.rs:98,114,144,151` |
| права на префиксах: `is_personal`, `is_premium`, `is_frkn_shared` | `env.rs:57-80` |
| `owns_env` = личный энв подписки или её премиум-слот | `memory/subscription.rs:291-300` |
| private/premium-ручки режут по `is_personal`/`is_frkn_shared` | `handlers/private.rs:714,733` |
| продукты → энвы | `memory/product.rs:85-89`, `bin/api/config.rs:95` |
| топики шины содержат имя env и парсятся обратно | `zmq/topic.rs:28-30,55,60` |
| в базе env уже `text`, без CHECK | `deploy/migrations/0020_peers.sql:18-21,37,66` |
| в ноде `env` уже `String`, `Env` — только in-memory | `memory/node.rs:126` vs `:169` |

Гейт — **10 вызовов `Env::from_str`** вне `env.rs`: `api/postgres/peers.rs:165,175,1161,1190`,
`api/http/request.rs:550`, `api/http/handlers/peers.rs:601,1169,1320`, `zmq/topic.rs:55,60`.

## 2. Целевые типы

```rust
/// Имя шарда: валидировано в точке входа, на проводе — голая строка.
pub struct EnvName(String);

impl EnvName {
    /// trim + lowercase + `^[a-z0-9][a-z0-9-]{0,31}$`
    pub fn parse(raw: &str) -> Result<Self, EnvNameError>;
    pub fn as_str(&self) -> &str;
}

/// Проверка без конструирования (админка, до INSERT).
pub fn validate_env_name(raw: &str) -> Result<(), EnvNameError>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvPurpose { Core, Premium, Personal }

/// Имя + назначение. Eq/Hash — ТОЛЬКО по name.
pub struct Env { name: EnvName, purpose: EnvPurpose }
```

Правила, без которых это развалится:

1. **На проводе — голая строка.** `Serialize`/`Deserialize` у `Env`/`EnvName` пишут и читают
   `"standard-ru"`, а не `{"name":…,"purpose":…}`. Иначе ломаются клиент, ЛК, конфиги нод и
   парсинг топиков `updates-<env>` / `peers-update-<env>`. Derive-сериализацию тут использовать
   нельзя.
2. **Eq/Hash по name.** `HashMap<Env, Vec<Tag>>` (`enabled_conns`, `env_proto_templates`,
   `bin/api/config.rs:92-105`) не должен зависеть от purpose: иначе два одинаковых шарда с
   разным назначением станут разными ключами и в конфиге появятся дубли.
3. **purpose — из данных, не из имени.** Источник истины — колонка `env_networks.purpose`.
   `Env`, собранный из строки (TOML, старый JSON), получает purpose по таблице
   соответствий из §3 и **не используется в проверках прав**; права читают purpose из строки БД.
   Иначе api и нода с разными конфигами молча разойдутся в трактовке шарда.
4. **Personal без владельца бессмысленен.** `owns_env` (`subscription.rs:291`) сравнивает env с
   `Env::personal_for(sub.id)`, а `private.rs:714` требует «личный и не frkn-shared». Значит
   проверка = `purpose == Personal && name == personal_for(sub.id)`; `personal_for` остаётся
   и продолжает давать ту же строку (`custompersonal<uuid_hex>`), чтобы не мигрировать данные.
5. **`From<&str> → Experimental` и `Default = Experimental` удалить.** Молчаливое превращение
   опечатки в exp-энв — худший класс бага: юзеры уезжают в чужой шард без ошибки в логах.
6. **`validate()` отдельным методом — не надо.** Валидный `EnvName` должен быть единственным
   возможным состоянием: приватное поле + `parse()`. Свободная функция `validate_env_name`
   нужна только там, где строку проверяют, не конструируя (админка).

### Где какой тип

* `EnvName` — всё, что есть просто идентичность: ключи `enabled_conns`/`env_proto_templates`,
  `node.env`, `connection.env`, логи, топики.
* `Env { name, purpose }` — только там, где решается семантика: `owns_env`, private/premium
  ручки, выдача грантов, `pool_cards`.
* Клиент purpose не видит и не получает: для него env — по-прежнему непрозрачная строка,
  наружу торчат `plan_title` и `pool_translation`.

## 3. Данные

Новая миграция `0021_env_purpose.sql`, аддитивная:

```sql
ALTER TABLE env_networks ADD COLUMN IF NOT EXISTS purpose text NOT NULL DEFAULT 'core';

-- назначение существующих энвов, один раз
UPDATE env_networks SET purpose = 'premium'
  WHERE purpose = 'core' AND (env LIKE 'premium%' OR env LIKE 'custompremium%');
UPDATE env_networks SET purpose = 'personal'
  WHERE purpose = 'core' AND (env LIKE 'personal%' OR env LIKE 'custompersonal%');

ALTER TABLE env_networks DROP CONSTRAINT IF EXISTS env_networks_purpose_chk;
ALTER TABLE env_networks ADD CONSTRAINT env_networks_purpose_chk
  CHECK (purpose IN ('core', 'premium', 'personal'));
```

Проверки до миграции:

```sql
-- 1. нет ли энвов, которые сейчас совпадают только после срезания custom
SELECT DISTINCT env FROM subscription_access WHERE env LIKE 'customcustom%'
UNION SELECT DISTINCT env FROM peers WHERE env LIKE 'customcustom%'
UNION SELECT DISTINCT env FROM env_capacity WHERE env LIKE 'customcustom%';
-- 2. весь список энвов, которые надо получить в purpose
SELECT env FROM env_networks ORDER BY env;
```

Правила:

* `env_networks` = валидный список шардов. «Энв существует» проверяется здесь, а не в `FromStr`.
* env **иммутабелен**: переименование ломает гранты, пиры, подсеть, конфиги нод и кэш клиента.
  Человеческое имя шарда — отдельное поле (`title`/`translation`), не env.
* purpose=Personal/Premium нельзя создавать из общей админки: только своим флоу
  (`personal_for(id)`, premium-slot), иначе шард «Personal» обойдёт оплату.

## 4. Шаги в коде

1. `EnvName` + `validate_env_name` + тесты формата (границы, uppercase → lowercase, `.`/`:` отбить).
2. `Env { name, purpose }`: `Serialize`/`Deserialize` (голая строка), `Display`, `Eq`/`Hash` по name.
3. Удалить `From<&str>`, `Default`; проверить, что компилятор показал все точки, где раньше был
   молчаливый `Experimental`.
4. 10 точек `Env::from_str` → `EnvName::parse` (вход api) либо `EnvName::parse`+purpose из БД
   (внутри), в топиках — `EnvName::parse`.
5. `product.rs:85-89` и `config.rs:95`: константы `GAMING`/`BS_PREM` вместо вариантов enum.
6. `owns_env`, `private.rs`, `amnezia.rs` (`:746,1869,1887`), `subscription.rs:1037` — сравнение
   purpose + `personal_for`, не префиксы.
7. `handlers/peers.rs:601,1169,1320` — валидация env через `EnvName::parse` + наличие строки в
   `env_networks` (сейчас даёт 400 `bad_env`).
8. Тесты: `standard-ru` round-trip (parse → serialize → topic → parse), `dev`/`gaming`/`bs-prem`/
   `custompersonal<hex>` не меняются, `custom` не всплывает, неизвестная строка — ошибка,
   `owns_env` не пускает в чужой личный энв, `premium-eu` не получает Personal по имени.

## 5. Проверка на живом

1. `cargo test --bin api` и `cargo test --lib` зелёные.
2. На проде после выката: вставить `env_networks ('standard-ru', '10.88.0.0/24', 'core')` +
   `env_capacity` + ноду с `[awg_peers]` → `POST /v1/locations` отдаёт карточку, `POST /v1/peers`
   создаёт пир (зашифрованным конвертом, см. `peers-envelope.md`).
3. Нода подписалась на `peers-update-standard-ru` — в логе fnode есть топик, пир применился.
4. `custom`-энвы не сломались: личная нода (personal) продолжает регистрироваться и обслуживаться
   без оплаты, премиум-слот виден только своему аккаунту.

## 6. Риски

* **Провод.** Любая derive-сериализация `Env { name, purpose }` ломает клиент и ноды — проверять
  `serde_json::to_string` в тесте, а не глазами.
* **Права останутся на префиксах, если не доделать шаг 6.** Тогда shard «работает», а
  `premium-*` по имени всё ещё считается премиум-слотом.
* **Удаление строки `env_networks` при живых пирах** оставляет туннели без адреса: удалять только
  после отзыва грантов/пиров.
* **Топик-совместимость.** Имя с `.`, `:`, пробелом или uppercase не распарсится из
  `peers-update-<env>` — формат из §2 держать жёстко.

## 7. Решения от 19.11.2026

### 7.1 Fallback не нужен — убираем целиком

Три точки, которые нельзя переносить в новую схему: `From<&str>` → `Experimental`
(`env.rs:156-160`), `Default = Experimental` (`env.rs:11`), `traffic::parse_env`
(`bin/api/traffic.rs:192-195`). Что встаёт на их место:

* `traffic::parse_env` → `Option<EnvName>`: неизвестный env **не пишет серию** и логирует `warn`.
  Сейчас он тихо пишет трафик в `experimental` — то есть портит квоты чужого шарда.
* `postgres/install_token.rs:67` (`Env::from(scope_env.as_str())`) → `EnvName::parse`, ошибка =
  битая строка в БД, отдаём ошибку, а не энв по умолчанию.
* тест `env.rs:262` («unknown_garbage → Experimental») удаляется вместе с поведением.
* `#[derive(Default)]` на структурах с `Env` — компилятор покажет все места; дефолтный шард не нужен.

### 7.2 NodeType не заменяет вид шарда

`NodeType` = `fcore::Type` (`memory/node.rs:85-97`): `common` / `premium` / `service` / `agent` —
это **роль ноды**, а не вид шарда. Вид шарда из нод не выводится:

* проверки идут без нод: `owns_env` (`subscription.rs:291`) на `subscription.scope_env`,
  `private.rs:714` на `install_token.scope_env` — до того, как нода зарегистрировалась;
* шард без живых нод (только заведён, все офлайн) не имеет ни одного NodeType, а решать
  «Personal / Premium / Core» надо уже в `/v1/locations` и при активации;
* в `Type` есть `Service`/`Agent`: они тоже могут стоять в шарде, и если роль ноды начнёт
  означать вид шарда, эти значения вступят в конфликт;
* `node_type` — pg-enum (`common/premium/service/agent`); `personal` туда не добавить без
  миграции и без ответа «какие `common`-ноды были личными» — а ответ снова берётся из имени.

Правильная раскладка: **вид шарда — колонка в `env_networks`** (единственное место, истинное для
пустого шарда), **NodeType — инвариант при регистрации**: `premium`-нода только в premium-шарде,
`common` — в core, personal-шард закрыт install-token'ом (`private.rs:714`). Так классификация
единая, но вид шарда не зависит от того, какие ноды сейчас живы.

### 7.2.1 Зачем был `is_frkn_shared` и что его заменяет

Три вызова, из них один несущий:

* `connection.rs:579` (`validate_env_served`): гейт «в энве есть живая нода с этим протоколом»
  применяется **только к пользовательским** энвам. Наши регионы (`ru`, `bs-prem`, `gaming`…)
  могут быть временно без нод, а entitlements на них ещё ссылаются — и тарифный ensure обязан
  продолжать перевыпуск. Коммит `c70eb0b`: до него гейт стоял на всех энвах и за минуты после
  0.6.63 насыпал 16 ошибок `Env not served: ru has no nodes` (4 подписки × 4 протокола).
* `private.rs:714,733` — «нода в шаред-энве не может быть личной». Это уже покрыто
  `!is_personal()`, то есть вторая половина условий не срабатывает никогда (перестраховка
  вокруг `Custom`).

После перехода на шарды предикат не нужен: «типовая» роль отдаётся **существованию строки в
`env_networks`** (опечатка → строки нет → 400 `bad_env`, соединение не создаётся), а «пустой шард
— это нормально» — виду шарда (`kind = core/premium/personal`). Личные и премиум-энвы при этом
сами лежат в `env_networks` (оттуда же берётся подсеть), так что проверка `owns_env` остаётся
на `kind` + `personal_for(id)`.

### 7.3 Топики

Сейчас нода подписывается на четыре топика (`bin/node/node.rs:362-369`):

| топик | что несёт |
|---|---|
| `updates-<env>` | поток коннектов (v1) |
| `init-<uuid>` | полный набор коннектов ноды |
| `peers-update-<env>-<pool>` | поток пиров **пула** (нода без `pool` — `peers-update-<env>`) |
| `peers-init-<uuid>` | авторитетный набор пиров ноды: `(env, pool)`, а без `pool` — весь шард |

Сделано в 0.7.44: поток пиров сузился до пула, двойной публикации нет (v2-пирами пользуется
только владелец). Порядок раскатки: api → `systemctl restart fnode` на каждой ноде; нода при
старте подписывается на свой `peers-update-<env>-<pool>` и получает `peers-init` уже по пулу.

Предложенные `updates-shard-<name>` / `init-node-<id>` — это переименование пары для пиров.
Два условия:

1. Не смешивать с v1: `updates-<env>` (коннекты) остаётся, поэтому `updates-shard-<name>` читается
   как «главный поток». Либо переименовать обе пары одновременно с выводом v1, либо назвать явно
   `peers-updates-<shard>` / `peers-init-<node>`. Имя внутри — `EnvName`, формат из §2.
2. Это контракт api↔нода на живом парке, поэтому раскатка в три шага:
   api публикует **в оба** имени и `Topic::from_str` принимает оба → раскатываем fnode на всех
   нодах → старые имена выпиливаем (когда в `/status` нет нод на старом билде). Без первого шага
   между релизом api и обновлением нод пиры молча перестают применяться: ошибок на api нет,
   в логе ноды просто нет сообщений.

### 7.4 Миграция до деплоя

Инвентарь — все места, где env лежит строкой:

```sql
SELECT 'env_networks' t, env FROM env_networks
UNION ALL SELECT 'env_capacity', env FROM env_capacity
UNION ALL SELECT 'subscription_access', env FROM subscription_access
UNION ALL SELECT 'peers', env FROM peers
UNION ALL SELECT 'connections', env FROM connections
UNION ALL SELECT 'nodes', env FROM nodes
UNION ALL SELECT 'subscriptions.scope_env', scope_env FROM subscriptions WHERE scope_env IS NOT NULL
UNION ALL SELECT 'install_tokens.scope_env', scope_env FROM install_tokens
ORDER BY 1, 2;
```

Проверить до деплоя:

1. **Формат.** Любое имя, не проходящее `^[a-z0-9][a-z0-9-]{0,31}$` (uppercase, `.`, `:`, пробел,
   `_`), — либо мигрировать, либо явно решить, что оно остаётся легаси и в шарды не попадает.
2. **`customcustom%`** — строки, которые сейчас совпадают с именем только после срезания префикса
   (§3). Нормализовать один раз до включения verbatim-строки.
3. **Вид шарда** — заполнить `env_networks.purpose` по маппингу из §3, включая `custompremium…` /
   `personal…`.
4. **Сверка после миграции** — ни один пир/грант не должен ссылаться на env, которого нет в
   `env_networks` (иначе после деплоя он станет `bad_env` и туннель не поднимется):

```sql
SELECT DISTINCT p.env FROM peers p
  LEFT JOIN env_networks n USING (env) WHERE n.env IS NULL;
SELECT DISTINCT a.env FROM subscription_access a
  LEFT JOIN env_networks n USING (env) WHERE n.env IS NULL;
```

Сейчас на проде `env_networks` содержит только `dev` — это самый дешёвый момент для миграции:
чем позже, тем больше таблиц придётся сверять.
