# Гео-листы `/router/geo/*`: зоны `ru.zone`, `ai4.zone`, `aicn4.zone`, `adult4.zone`, `exch4.zone`, `gov4.zone`, `youtube4.zone`

Публичные plain-text ручки, из которых потребители (роутер, ноды пула `eu`)
берут списки IPv4. Отдаёт их **nginx с хоста api** прямо из файлов в
`/var/lib/frkn/geo` — api в этой отдаче больше не участвует (§ «Зоны отдаёт
nginx»); контракт ответа при переезде не менялся.

* **`ru.zone`** — российские подсети для прошивки OpenWrt-роутера (у него есть
  вшитая копия, которая стареет вместе с прошивкой) и для set `ru4` на нодах
  пула `eu`.
* **`ai4.zone`** — IP-адреса AI-сервисов (Anthropic/Claude, OpenAI/ChatGPT,
  Gemini, xAI/Grok, Mistral, Perplexity, Copilot), которые резолвер api
  **проверяет TLS-хендшейком** перед публикацией. Из них ноды пула `eu` держат
  nft set `ai4`, и маршрут для AI-трафика решается ядром, а не прокси в
  userspace.
* **`aicn4.zone`** — те же домены → проверенные IP, но для китайских AI
  (DeepSeek, Qwen, Moonshot/Kimi, Zhipu/GLM, MiniMax, Volcengine/Doubao).
  Ноды применяют её как **direct whitelist**: адрес из сета не маркируется и
  выходит с IP самой ноды. Это не ослабленная проверка, а другое решение о
  маршруте, поэтому зона отдельная, а не часть `ai4`.
* **`adult4.zone`** — проверенные IP взрослых платформ (Pornhub, XVideos, XNXX,
  xHamster, Redtube, Youporn, Spankbang, ...) и их image/media CDN. Второй
  **direct whitelist**: адрес из сета не маркируется и всегда выходит с IP
  NL-входа ноды, никогда через RU/US/KZ. Это правовое решение владельца (в NL
  рабочий режим без требований возрастной верификации; RU-выход подставил бы
  пользователей под российское законодательство, а RU-сет — большой
  агрегированный список — ловит hosting-адреса этих сайтов случайно), поэтому
  зона отдельная и её правило стоит выше всех марок.
* **`exch4.zone`** — проверенные IP китайских криптобирж (Binance, OKX, Bybit,
  HTX, Gate, KuCoin, MEXC, Bitget, BitMart, LBank, CoinEx, BingX, WEEX,
  Tokocrypto). Биржи отказывают или деградируют на US/EU адресах, поэтому ноды
  маркируют этот сет **в RU-выход** (`mark 0x2`, существующая таблица 200) и
  ставят правило **выше** `ai4`/`youtube4`. Третье решение о маршруте при той же
  проверке — третья зона.
* **`gov4.zone`** — проверенные IP российских государственных сайтов (Госуслуги,
  ЕСИА, Почта, `mos.ru`, ФНС, СФР, суды, министерства). Ноды дают этому сету
  **собственный одноцелевой выход**: `mark 0x7` → таблица 700 → `msk-tw`
  (`217.149.29.118`), правило стоит **выше всех** групп и fail-closed (упал
  единственный выход — сайты недоступны, а не утекают через NL или общую RU-группу).
  Причина: новые RU-хостинговые диапазоны WAF-зафлагены для гос-сайтов —
  `pochta.ru` отдавал 417 за 0.5 с, `mos.ru` не отвечал вовсе, а старый `msk-tw`
  открывает те же адреса за десятые доли секунды. Четвёртое решение о маршруте при
  той же проверке — четвёртая зона (подробности — `env`/`routing.md` §8.7).
* **`youtube4.zone`** — проверенные IP YouTube: ноды с KZ-выходом (`mark 0x6`,
  правило только на `nl01`) маркируют этот сет в таблицу 600. Тот же конвейер;
  сверх `/router/geo/` копия лежит в `/var/www/media/geo/` и отдаётся как
  `https://media.frkn.org/geo/youtube4.zone` (отдельная статика, к geo-ручке
  отношения не имеет).

Все ручки отдаются с условным GET: ежечасный/ежедневный опрос стоит один
`304`, когда список не менялся. Никакой БД за ручками нет: истина — файл на
диске, его sha256 — и сильный ETag, и то, что отдаёт `.sha256`.

## Ручки

| Метод | Путь | Ответ |
|---|---|---|
| `GET`, `HEAD` | `/router/geo/ru.zone` | список CIDR, `text/plain; charset=utf-8`, один CIDR на строку, `\n` |
| `GET`, `HEAD` | `/router/geo/ru.zone.sha256` | `<sha256>  ru.zone\n` (формат `sha256sum`) |
| `GET`, `HEAD` | `/router/geo/ai4.zone` | то же, `/32` на строку |
| `GET`, `HEAD` | `/router/geo/ai4.zone.sha256` | `<sha256>  ai4.zone\n` |
| `GET`, `HEAD` | `/router/geo/aicn4.zone` | то же, `/32` на строку (direct whitelist) |
| `GET`, `HEAD` | `/router/geo/aicn4.zone.sha256` | `<sha256>  aicn4.zone\n` |
| `GET`, `HEAD` | `/router/geo/adult4.zone` | то же, `/32` на строку (direct whitelist: adult → всегда NL) |
| `GET`, `HEAD` | `/router/geo/adult4.zone.sha256` | `<sha256>  adult4.zone\n` |
| `GET`, `HEAD` | `/router/geo/exch4.zone` | то же, `/32` на строку (биржа → RU-выход) |
| `GET`, `HEAD` | `/router/geo/exch4.zone.sha256` | `<sha256>  exch4.zone\n` |
| `GET`, `HEAD` | `/router/geo/gov4.zone` | то же, `/32` на строку (гос-сайты → отдельный выход gov) |
| `GET`, `HEAD` | `/router/geo/gov4.zone.sha256` | `<sha256>  gov4.zone\n` |
| `GET`, `HEAD` | `/router/geo/youtube4.zone` | то же, `/32` на строку (YouTube → KZ-выход, только `nl01`) |
| `GET`, `HEAD` | `/router/geo/youtube4.zone.sha256` | `<sha256>  youtube4.zone\n` |
| `GET`, `HEAD` | `/router/geo/<zone>.count` | число CIDR валидированной генерации (`180\n`) — то же, что `<zone>.count` на диске |

`.count` — третья производная от того же файла: один и тот же strong ETag
(`sha256:…`), тот же `Last-Modified`, отказ `503`, если файл не прошёл
проверку формы/порога. Ноде он не нужен (она считает элементы сама), но
позволяет однострочной проверке сверить «применённый сет == опубликованная
генерация» без скачивания списка: `nft … | wc -l` против
`curl -s …/exch4.zone.count`.

Ручки **без авторизации** и **без AGW-конверта**, как `/sub/<token>` и
`/key/validate`. Полный URL на проде: `https://api.frkn.org/router/geo/ai4.zone`.

Отдаются только зоны из **whitelist в nginx** (`/etc/nginx/snippets/frkn-geo.conf`,
см. § «Зоны отдаёт nginx»). Имя из пути никогда не склеивается с каталогом:
`secret.zone`, `../../etc/passwd`, `ru.zone.sha256.bak` ничего не открывают.

Неизвестное имя зоны отвечает `404` (nginx), а не `405`: `405` у api был
артефактом его универсального POST-маршрута — он отвечал так на **любой**
неизвестный GET-путь (`/foobar`, `/router/xyz`). `405` на `/router/geo/*`
остался только для не-GET/HEAD по **известному** имени (тот же ответ, что и у
api).

### Заголовки ответа `200 OK`

| Заголовок | Значение |
|---|---|
| `Content-Type` | `text/plain; charset=utf-8` |
| `ETag` | сильный: `"sha256:<hex>"` — хэш содержимого файла |
| `Last-Modified` | mtime файла в IMF-fixdate (`Sat, 21 Nov 2026 04:30:00 GMT`) |
| `Cache-Control` | `public, max-age=3600` |
| `Content-Length` | размер тела (у `HEAD` — размер, который вернул бы `GET`) |

`HEAD` отдаёт те же заголовки без тела — этим проверяют свежесть, не качая
файл.

### Условный запрос

`If-None-Match` с текущим ETag (можно списком, можно `W/"…"` — сравнение
слабое по RFC 9110) → **`304 Not Modified`** без тела, без `Content-Length` и
без `Content-Type`; остаются `ETag`, `Last-Modified`, `Cache-Control`.
Заголовок `*` тоже попадает в `304`.

### Ошибки

| Код | Когда | Тело |
|---|---|---|
| `404` | файла ещё нет (известная зона, таймер ещё не публиковал) | `<name> not published` |
| `404` | имени зоны нет в whitelist (`nope.zone`, `ai4.zone.bak`, `../../etc/passwd`) | `404 Not Found` (nginx) |
| `503` | файл есть, но не проходит валидацию (обрезанный / мусор / ниже порога зоны) | `geo list unavailable` |
| `500` | файл не читается (права, I/O) | `geo list unavailable` |

Неизвестное имя зоны (`/router/geo/nope.zone`) — `404` от nginx: файл по такому
имени не читается ни при каких условиях. `405` — только не-GET/HEAD по
**известному** имени (`POST /router/geo/ru.zone`).

`503` — намеренно: лучше отдать ошибку, чем обрезанный список, который
потребитель применит и уведёт не те префиксы. Порог свой у каждой зоны:
`ru.zone` — 1000 CIDR (реально ~8.7 тыс.), остальные зоны — 8 (реально
десятки/сотни; размер генерации защищает публикатор, а не отдача). Ошибки не
кэшируются (`Cache-Control: no-store`).

## Где лежат файлы

Каталог **`/var/lib/frkn/geo`** (константа `GEO_DIR` в
`/etc/nginx/frkn-geo.js`, источник — `deploy/nginx/frkn-geo.js`):

```
/var/lib/frkn/geo/ru.zone              # то, что отдаёт /router/geo/ru.zone
/var/lib/frkn/geo/ru.zone.sha256       # копия контрольной суммы для оператора
/var/lib/frkn/geo/ai4.zone             # проверенные IP AI-сервисов
/var/lib/frkn/geo/ai4.zone.sha256
/var/lib/frkn/geo/ai4.count            # размер последней генерации (штук /32)
/var/lib/frkn/geo/ai4.state.json       # прошлое поколение по домену (не отдаётся)
/var/lib/frkn/geo/aicn4.zone           # проверенные IP китайских AI (direct whitelist)
/var/lib/frkn/geo/aicn4.zone.sha256
/var/lib/frkn/geo/aicn4.count
/var/lib/frkn/geo/aicn4.state.json
/var/lib/frkn/geo/adult4.zone          # проверенные IP взрослых платформ (direct whitelist -> всегда NL)
/var/lib/frkn/geo/adult4.zone.sha256
/var/lib/frkn/geo/adult4.count
/var/lib/frkn/geo/adult4.state.json
/var/lib/frkn/geo/exch4.zone           # проверенные IP китайских бирж (RU-выход)
/var/lib/frkn/geo/exch4.zone.sha256
/var/lib/frkn/geo/exch4.count
/var/lib/frkn/geo/exch4.state.json
/var/lib/frkn/geo/gov4.zone            # проверенные IP гос-сайтов (отдельный выход gov, msk-tw)
/var/lib/frkn/geo/gov4.zone.sha256
/var/lib/frkn/geo/gov4.count
/var/lib/frkn/geo/gov4.state.json
```

Ручка читает только `*.zone` и сама считает sha256 — файл `.sha256` нужен
людям/скриптам на хосте, потребителю он не нужен (поэтому рассинхрон соседних
файлов не может отдать неверную сумму). `.count` тоже считается из зоны, а не
читается из `<zone>.count`: у `ru` этого файла нет вовсе. `ai4.state.json` —
рабочее состояние резолвера: предыдущая генерация по каждому домену, чтобы
краткая недоступность сервиса не выбрасывала его IP из сета. Каталог должен
быть читаем пользователю nginx (`www-data` — `0755`/`0644`).

## Зоны отдаёт nginx

Решение владельца (22.11.2026): отдача зон — **не путь в коде api**. Пока имя
зоны было строкой в `ZONES`, каждая новая зона (`ai4`, `aicn4`, `exch4`,
`youtube4`, `gov4`, `adult4`) требовала правки кода, сборки и релиза api;
сборка дорогая (билд-хост уходит в OOM, релиз ~30+ мин), а зона — это файл,
который и так пишет таймер. Теперь файлы отдаёт nginx: новая зона —
`scp` файлов + имя в whitelist + `nginx -s reload`.

Отдачу делает njs-обработчик. Статический `alias` + `ETag on` не годится по
трём причинам:

1. **ETag.** `ETag on` даёт `"<mtime>-<size>"`, а потребители требуют
   `"sha256:<хэш тела>"`: `frkn-geo-ai4-refresh.sh` на нодах сравнивает ETag с
   sha256 скачанного тела и при расхождении **не применяет зону**
   (`ETag/checksum disagree, keeping the current set`). Native ETag молча
   остановил бы обновление сета.
2. **Производные ресурсы.** `.zone.sha256` и `.zone.count` — та же генерация:
   у них `ETag` и `Last-Modified` родительской зоны. Отдельный файл `.count`
   этого не даёт (а `ru.count` вообще нет).
3. **Порог.** Пустой/обрезанный файл обязан отдаваться как `503`, а не как
   «короткий список»: потребитель применил бы его и развёл маршруты.

### Файлы на хосте

| Файл | Что | Источник в репозитории |
|---|---|---|
| `/etc/nginx/frkn-geo.js` | njs-обработчик: whitelist, `ETag`/`Last-Modified`, `304`, `HEAD`, `503` | `deploy/nginx/frkn-geo.js` |
| `/etc/nginx/conf.d/frkn-geo-js.conf` | `js_import frkn_geo from /etc/nginx/frkn-geo.js;` (уровень `http`) | `deploy/nginx/frkn-geo-js.conf` |
| `/etc/nginx/snippets/frkn-geo.conf` | `location` с whitelist и `404`, включается в server `api.frkn.org` | `deploy/nginx/frkn-geo.conf` |
| `/etc/nginx/modules-enabled/50-mod-http-js.conf` | `load_module modules/ngx_http_js_module.so` | пакет `libnginx-mod-http-js` |

`libnginx-mod-http-js` (njs) — единственная новая зависимость: на Ubuntu 24.04
это `0.8.2-1ubuntu1` из `universe`, ставится без пересборки nginx и сам
добавляет `load_module`; правки `nginx.conf` не нужны.

### Snippet (`/etc/nginx/snippets/frkn-geo.conf`)

```nginx
location ~ ^/router/geo/(ru|ai4|aicn4|exch4|youtube4|gov4|adult4)\.zone(\.sha256|\.count)?$ {
    js_content frkn_geo.serve;
    gzip off;
    default_type "";
}

location /router/geo/ {
    return 404;
}
```

В server-блоке `api.frkn.org` (`/etc/nginx/sites-enabled/api`) — одной строкой
**выше** `location /` (иначе запрос уйдёт в api-прокси):

```nginx
include /etc/nginx/snippets/frkn-geo.conf;
```

`gzip off` — тело обязано остаться байт-в-байт (sha256 тела = ETag).
`default_type ""` — у `304` не должно быть `Content-Type`/`Content-Length`, как
и у api. Остальные заголовки ставит обработчик.

Whitelist — **единственное место, где перечислены имена зон**; всё, что не
совпало с regex (`nope.zone`, `ai4.zone.bak`, `ru.zone.sha256.bak`,
`../../etc/passwd`), получает `404` и до файлов не доходит.

### Что именно совпадает с api

`200` — те же `Content-Type: text/plain; charset=utf-8`, `Content-Length`,
`ETag: "sha256:<хэш файла>"`, `Last-Modified` (mtime зоны), `Cache-Control:
public, max-age=3600`; тело — байт-в-байт файл. `HEAD` — те же заголовки без
тела. `304` по `If-None-Match` (одиночный ETag, список, `W/"…"`, `*`) — только
`ETag`, `Last-Modified`, `Cache-Control`. Не-GET/HEAD по известному имени —
`405 HTTP method not allowed`. Известное имя без файла — `404 <name> not
published`. Файл не прошёл форму/порог (пустой, обрезанный, строка не IPv4
CIDR, меньше порога) — `503 geo list unavailable`, ошибки не кэшируются
(`Cache-Control: no-store`). Порог: `ru.zone` — 1000, остальные — 8.

Единственное отличие: неизвестное имя — `404`, а не `405` (артефакт api, см.
выше).

Сверено 22.11.2026 на всех семи зонах: тело, длина, `Content-Type`, `ETag`,
`Last-Modified`, `Cache-Control`, `304`, `HEAD`, `405` совпали с ответами api
байт-в-байт. `youtube4`/`gov4`/`adult4` api отдавал `405` (их не было в
`ZONES` выкаченного релиза) — теперь они отдаются из своих файлов, ради этого
задача и делалась.

### Опубликовать новую зону

```bash
# 1. файл и сумма — туда, откуда nginx их читает
sudo install -m 0644 <zone>.zone <zone>.zone.sha256 /var/lib/frkn/geo/
#    .count рядом пишет резолвер; ручка считает его сама, файл не обязателен

# 2. резолвер: имя зоны в юните (--zone-name <zone>.zone) + таймер, если зона новая
# 3. имя зоны — в regex /etc/nginx/snippets/frkn-geo.conf (единственное место)

# 4. reload и проверка
sudo nginx -t && sudo systemctl reload nginx
curl -sI https://api.frkn.org/router/geo/<zone>.zone
curl -s  https://api.frkn.org/router/geo/<zone>.zone | sha256sum
curl -s  https://api.frkn.org/router/geo/<zone>.zone.sha256
curl -s  https://api.frkn.org/router/geo/<zone>.zone.count
```

JS править не нужно (порог по умолчанию 8, как у всех проверенных зон), релиз
api — тоже. Резолвер публикует через `tempfile` + `os.replace`, поэтому nginx
всегда видит целую генерацию, а `ETag`/`Last-Modified` меняются вместе с ней.

### Проверка и диагностика

```bash
# тело = файл на диске, ETag = sha256 тела, .sha256 = тот же хэш, .count = wc -l
for z in ru ai4 aicn4 exch4 youtube4 gov4 adult4; do
  echo "$z $(curl -s https://api.frkn.org/router/geo/$z.zone | sha256sum | cut -c1-16)" \
       "$(sha256sum /var/lib/frkn/geo/$z.zone | cut -c1-16)" \
       "$(curl -s https://api.frkn.org/router/geo/$z.zone.count)" \
       "$(wc -l < /var/lib/frkn/geo/$z.zone)"
done

# 304 по текущему ETag
E=$(curl -sI https://api.frkn.org/router/geo/ai4.zone | tr -d '\r' \
    | awk -F': ' 'tolower($1)=="etag"{print $2}')
curl -sI -H "If-None-Match: $E" https://api.frkn.org/router/geo/ai4.zone | head -1

# отказы
curl -s -o /dev/null -w '%{http_code}\n' https://api.frkn.org/router/geo/nope.zone        # 404
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://api.frkn.org/router/geo/ai4.zone # 405

# почему 404/503 — обработчик пишет причину в error log
tail -50 /var/log/nginx/error.log | grep 'geo:'
```

Стенд без публичного эффекта: `server { listen 127.0.0.1:18080; include
/etc/nginx/snippets/frkn-geo.conf; }` в `conf.d`, проверять локально, потом
убрать. Откат: убрать `include …/frkn-geo.conf;` из server-блока и
`nginx -s reload` — запросы снова уходят в api.

### Отдача зон снята из api (`0.7.31`)

Кодовая отдача зон из api удалена в релизе **0.7.31** (выкачен на прод
22.11.2026), как и планировалось:

* `src/bin/api/http/handlers/geo.rs` — файл удалён целиком (`ZONES`,
  `ZoneSpec`, пороги `MIN_*_LINES`, `validate_zone`, `if_none_match_hits`,
  `GeoAsset` и 24 его теста);
* `handlers/mod.rs` — снят `pub(crate) mod geo;`;
* `routes.rs` — сняты монтирование (`geo_handlers::routes`) и `.or(geo_routes)`,
  `/healthcheck` и все остальные маршруты не тронуты;
* `config.rs` — снят `[service] geo_dir` (его читал только этот обработчик) и
  `default_geo_dir()`; в проде в `config.toml` ключа не было, поэтому конфиг
  менять не нужно.

Теперь `/router/geo/*` существует **только** в nginx: в api маршрута нет, и
прямой запрос в него (`127.0.0.1:3000`) отвечает так же, как на любой другой
неизвестный GET-путь в этом api, — `405` от универсального POST-фильтра
(`/foobar`, `/router/xyz` дают тот же ответ). Публичный путь до api не доходит:
его забирает `location` из nginx. Резолверы и таймеры
(`deploy/geo/`) не тронуты — они пишут файлы в `/var/lib/frkn/geo`, а этот
каталог читает nginx. Проверено после выката: все семь зон через nginx
отдают `200`/`304`, тело каждой зоны совпадает с файлом на диске, `.sha256` и
`.count` сходятся, неизвестное имя — `404`, `POST` — `405`, `/healthcheck` —
`Ok (v0.7.31)`.


## Зона `ai4.zone`: домены → проверенные IP

Решение владельца: **сначала правила ядра**, потому что это на порядок дешевле
userspace-прокси. AI-сервисы блокируют/иначе ведут себя по стране IP, поэтому
их трафик должен выходить отдельным (US) выходом. Маршрут принимается по
**IP-набору**, а не по domain-правилам sing-box: домены периодически
превращаются в набор IP, а маршрутизацию решает nft.

Принцип надёжности: «лучше чуть медленно открывается, чем через раз
открывается быстро». Отсюда — проверка каждого IP и запрет публиковать
сжавшийся набор.

### Источник правды: список доменов

`env`-репозиторий, `production/shard/0001/eu/domains/<group>.txt`: один суффикс
на строку, `#` — комментарий. На хосте api лежит копия
`/etc/frkn/geo/domains/<group>.txt` (конфиг, только чтение для юнита). Сейчас
четыре группы: `ai.txt` (exit-группа `ai`), `ai-cn.txt` (direct whitelist
`ai-cn`), `adult.txt` (direct whitelist `adult` → всегда NL) и `exchanges.txt`
(exit-группа `exch` на RU-марке). Правила файла и как
добавить сервис — `domains/README.md` рядом.

### Резолвер: `deploy/geo/update-ai4-zone.py`

Один скрипт публикует все четыре зоны; группу задают `--group`, `--domains` и
`--zone-name`:

```bash
# ai4.zone (группа ai)
update-ai4-zone.py --domains /etc/frkn/geo/domains/ai.txt --zone-name ai4.zone
# aicn4.zone (группа ai-cn, direct whitelist)
update-ai4-zone.py --group aicn4 --domains /etc/frkn/geo/domains/ai-cn.txt \
    --zone-name aicn4.zone
# adult4.zone (группа adult, direct whitelist -> всегда NL)
update-ai4-zone.py --group adult4 --domains /etc/frkn/geo/domains/adult.txt \
    --zone-name adult4.zone
# exch4.zone (группа exch, RU-выход)
update-ai4-zone.py --group exch4 --domains /etc/frkn/geo/domains/exchanges.txt \
    --zone-name exch4.zone
```

Запускается `frkn-geo-ai-update.timer` / `frkn-geo-aicn4-update.timer` /
`frkn-geo-adult4-update.timer` / `frkn-geo-exch4-update.timer` /
`frkn-geo-gov4-update.timer` на хосте api — **ежечасно** плюс случайная
задержка до 5 минут, `Persistent=true`. Что делает (одинаково для всех зон):

1. **DNS из нескольких резолверов.** Каждый домен (и `www.<домен>` для
   двухуровневого суффикса) спрашивается как A-запись у **1.1.1.1 и 8.8.8.8**,
   ответы объединяются. Два независимых рекурсора ловят отравленный или
   частично сломанный первый ответ. Не IPv4 (AAAA, CNAME-цели) отбрасывается.
2. **Проверка TLS/SNI — ядро надёжности.** Для каждой пары (имя, IP)
   открывается TLS на `IP:443` с `server_hostname=<имя>` и обычным
   `ssl.create_default_context()`: цепочка проверяется (`CERT_REQUIRED`) и имя
   сверяется с сертификатом (`check_hostname`, wildcard'ы учитываются). IP
   остаётся, только если хендшейк прошёл и сертификат **действительно выдан
   этому имени**. Устаревшая A-запись, чужой/паркованный IP, перехват — не
   проходят и в набор не попадают.
3. **Фолбэк на прошлое поколение — по домену.** Если домен не дал ни одного
   проверенного IP (сервис недоступен, DNS пуст), берётся его список из
   `ai4.state.json` и в лог идёт строка `no verified IP, reusing previous
   generation`. Так минутная недоступность Cloudflare не выбивает Claude из
   маршрута.
4. **Пороги (все отказывают, сохраняя текущую зону):**
   * свежую проверку прошло меньше половины доменов (`--min-fresh 50`) —
     сломан сам чекер или сеть хоста, а не сервисы: публиковать почти
     полностью устаревший набор нельзя;
   * новый набор меньше 60 % предыдущего (`--min-ratio 60`) — сжатие
     означает «половина сервисов перестанет маршрутизироваться»;
   * ноль проверенных IP.
   `--force` обходит пороги — только осознанно, руками.
5. **Публикация.** `/32` на строку, сортировка числовая (одинаковый набор →
   одинаковый sha256 → отдача отвечает `304`), запись в тот же каталог через
   `tempfile` + `os.replace` (атомарный rename), затем `<zone>.sha256`,
   `<zone-without-.zone>.count` и `<zone-without-.zone>.state.json`.
   `.count` — размер опубликованной генерации для оператора (и для логов нод);
   содержимое не изменилось — файл не трогается, mtime/ETag стабильны.

Ручной прогон (он же проверка после правки списка):

```bash
sudo /usr/local/lib/frkn/geo/update-ai4-zone.py \
    --domains /etc/frkn/geo/domains/ai.txt --verbose
# direct whitelist:
sudo /usr/local/lib/frkn/geo/update-ai4-zone.py --group aicn4 \
    --domains /etc/frkn/geo/domains/ai-cn.txt --zone-name aicn4.zone --verbose
# direct whitelist (adult -> всегда NL):
sudo /usr/local/lib/frkn/geo/update-ai4-zone.py --group adult4 \
    --domains /etc/frkn/geo/domains/adult.txt --zone-name adult4.zone --verbose
# RU-выход (биржи):
sudo /usr/local/lib/frkn/geo/update-ai4-zone.py --group exch4 \
    --domains /etc/frkn/geo/domains/exchanges.txt --zone-name exch4.zone --verbose
# без записи:
sudo /usr/local/lib/frkn/geo/update-ai4-zone.py --dry-run --verbose
journalctl -u frkn-geo-ai-update.service -n 30 --no-pager
journalctl -u frkn-geo-aicn4-update.service -n 30 --no-pager
journalctl -u frkn-geo-adult4-update.service -n 30 --no-pager
journalctl -u frkn-geo-exch4-update.service -n 30 --no-pager
```

Живой прогон 2026-11-21 (хост api): `ai4` — 29 доменов, 48 имён, **133
кандидата, 90 прошли TLS-проверку**, опубликовано 90 `/32`, 4.1 с. `aicn4` —
17 доменов, 25 имён, **57 кандидатов, 50 прошли TLS-проверку**, 50 `/32`,
sha256 `28b942d5…b5c4`; `volces.com` apex не имеет A-записи и даёт `0 IPs none`
(реальный фронт — `ark.cn-beijing.volces.com`). `exch4` — 26 доменов, 39 имён,
**197 кандидатов, 180 прошли TLS-проверку**, 180 `/32`; ни одного `0 IPs none`,
`tokocrypto.com` прошёл и остался в списке. `adult4` — 24 домена, 36 имён,
**162 кандидата, 98 прошли TLS-проверку**, 98 `/32`, sha256
`2e0a6295…5d3790`, ни одного `0 IPs none`. Через час штатный таймер
перегенерировал зону до **82 `/32`** (`sha256 d0fa9863…4ec`) — CDN ротирует
адреса, и это норма: отдача возвращает ровно ту генерацию, что лежит на диске.

Локальный прогон на macOS даёт меньше: системный Python там собран с LibreSSL
2.8.3, и Cloudflare-фронты (midjourney.com) отвечают
`TLSV1_ALERT_PROTOCOL_VERSION`. На хосте api Python с OpenSSL 3.0.13 и те же
домены проверяются нормально. Поэтому «настоящие» числа — только с api-хоста.

### Юниты

| Файл | Что делает |
|---|---|
| `deploy/geo/update-ai4-zone.py` | резолв + TLS-проверка + атомарная публикация (все зоны, `--group`) |
| `deploy/geo/frkn-geo-ai-update.service` | oneshot, `ProtectSystem=strict`, пишет только `/var/lib/frkn/geo` (`ai4.zone`) |
| `deploy/geo/frkn-geo-ai-update.timer` | ежечасно + до 5 мин, `Persistent=true` |
| `deploy/geo/frkn-geo-aicn4-update.service` | то же для `aicn4.zone` (`--group aicn4 --domains …/ai-cn.txt`) |
| `deploy/geo/frkn-geo-aicn4-update.timer` | ежечасно + до 5 мин, `Persistent=true` |
| `deploy/geo/frkn-geo-adult4-update.service` | то же для `adult4.zone` (`--group adult4 --domains …/adult.txt`) |
| `deploy/geo/frkn-geo-adult4-update.timer` | ежечасно + до 5 мин, `Persistent=true` |
| `deploy/geo/frkn-geo-exch4-update.service` | то же для `exch4.zone` (`--group exch4 --domains …/exchanges.txt`) |
| `deploy/geo/frkn-geo-exch4-update.timer` | ежечасно + до 5 мин, `Persistent=true` |

```bash
sudo install -d -m 0755 /usr/local/lib/frkn/geo /etc/frkn/geo/domains
sudo install -m 0755 deploy/geo/update-ai4-zone.py /usr/local/lib/frkn/geo/
sudo install -m 0644 <env>/production/shard/0001/eu/domains/{ai.txt,ai-cn.txt,adult.txt} /etc/frkn/geo/domains/
sudo install -m 0644 deploy/geo/frkn-geo-ai-update.service \
                    deploy/geo/frkn-geo-ai-update.timer \
                    deploy/geo/frkn-geo-aicn4-update.service \
                    deploy/geo/frkn-geo-aicn4-update.timer \
                    deploy/geo/frkn-geo-adult4-update.service \
                    deploy/geo/frkn-geo-adult4-update.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now frkn-geo-ai-update.timer frkn-geo-aicn4-update.timer frkn-geo-adult4-update.timer
sudo systemctl start frkn-geo-ai-update.service frkn-geo-aicn4-update.service frkn-geo-adult4-update.service   # первые прогоны
```

Резолвер не пишет в БД и не перезапускает api: обработчик читает файл на
каждый запрос.

## Резолвер `ru.zone`: `deploy/geo/update-ru-zone.sh`

Раз в сутки, источник — [ipdeny](https://www.ipdeny.com/ipblocks/)
`ru-aggregated.zone` (агрегированные подсети, ~8.7 тыс. CIDR).

| Файл | Что делает |
|---|---|
| `update-ru-zone.sh` | скачивает, валидирует и атомарно публикует список |
| `frkn-geo-update.service` | oneshot-юнит, запускает скрипт |
| `frkn-geo-update.timer` | ежедневно **04:30 UTC** + до 30 мин, `Persistent=true` |

Проверки перед публикацией: `curl -fsS` с таймаутами и ретраями, HTTPS only;
CRLF нормализуется; каждая строка — `a.b.c.d/0..32` с десятичными октетами
`<= 255`; строк не меньше `MIN_LINES` (1000). Только после этого `chmod 644`,
sha256, `.sha256`, `mv` в том же каталоге. Любая ошибка — ненулевой код,
`ru.zone` остаётся прежним.

## Применение на нодах пула `eu`

`sh-0001-nl01` / `sh-0001-nl02`. Схема ядра — `env`-репозиторий,
`production/shard/0001/eu/frkn-msk-route.sh` (создаёт таблицу `inet frkn_geo`:
сеты `ru4`/`ai4`/`aicn4`, mark-цепочки, `ip rule` для `0x2`→таблица 200 и
`0x3`→таблица 300; сет и правило `0x6` добавляет `frkn-youtube-route.sh`).

| Файл (ffcore) | Куда на ноде |
|---|---|
| `deploy/geo/nodes/frkn-geo-ai4-refresh.sh` | `/usr/local/bin/frkn-geo-ai4-refresh.sh` (общий движок обеих зон) |
| `deploy/geo/nodes/frkn-geo-ai4.service` | `/etc/systemd/system/` |
| `deploy/geo/nodes/frkn-geo-ai4.timer` | `/etc/systemd/system/` |
| `deploy/geo/nodes/frkn-geo-aicn4-refresh.sh` | `/usr/local/bin/frkn-geo-aicn4-refresh.sh` (обёртка: `aicn4.zone` → сет `aicn4`) |
| `deploy/geo/nodes/frkn-geo-aicn4.service` | `/etc/systemd/system/` |
| `deploy/geo/nodes/frkn-geo-aicn4.timer` | `/etc/systemd/system/` |

```bash
sudo install -m 0755 deploy/geo/nodes/frkn-geo-ai4-refresh.sh \
                    deploy/geo/nodes/frkn-geo-aicn4-refresh.sh /usr/local/bin/
sudo install -m 0644 deploy/geo/nodes/frkn-geo-ai4.{service,timer} \
                    deploy/geo/nodes/frkn-geo-aicn4.{service,timer} /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now frkn-geo-ai4.timer frkn-geo-aicn4.timer
sudo systemctl start frkn-geo-ai4.service frkn-geo-aicn4.service
```

Раз в час (jitter до 7 минут) скрипт делает **условный GET** своей зоны:
`304` → выходит, ничего не трогая — **кроме случая «сет в ядре пуст»**
(`frkn-msk-route.sh` пересоздаёт таблицу при деплое): тогда сохранённая на диске
зона применяется заново, иначе 304 оставлял бы пустой сет до следующей смены
зоны. На `200`:

1. сверяет sha256 скачанного файла с `<zone>.sha256` (ручка рядом) и с
   `ETag` из заголовков, чтобы обрыв на середине не применился;
2. проверяет форму (каждая строка — IPv4 CIDR) и размер: не меньше
   `FRKN_GEO_MIN_ELEMENTS` (8) и не меньше 60 % от того, что сейчас в сете
   (`FRKN_GEO_MIN_RATIO_PCT`) — сжавшийся список не применяется никогда;
3. пишет **один** файл для `nft -f` из `flush set` + `add element …` и отдаёт
   его nft целиком: nft коммитит файл атомарно, поэтому пакет никогда не
   увидит пустой или полузаполненный сет;
4. только после успешной транзакции сохраняет зону и ETag.

Логи: `frkn-geo-ai4: applied ai4.zone: 92 -> 95 elements (sha256 …)` /
`frkn-geo-aicn4: applied aicn4.zone: 0 -> 50 elements (sha256 …)`, старый и
новый размер сета в каждой строке. Сетевые ошибки → exit 0 (прошлая генерация
продолжает работать), ошибки валидации → exit 1 и `systemctl --failed`.

Проверка:

```bash
nft list set inet frkn_geo ai4 | head
nft list set inet frkn_geo aicn4 | head
nft list chain inet frkn_geo mark_pre
ip rule show | grep -E '0x2|0x3'
ip route show table 300      # только unreachable-гард, пока нет US-выхода
journalctl -u frkn-geo-ai4.service -n 20 --no-pager
journalctl -u frkn-geo-aicn4.service -n 20 --no-pager
systemctl list-timers 'frkn-geo*'
```

## Маршрутизация: группы `ai` и `ai-cn`

Порядок в mark-цепочках (первое совпадение):

```
fib daddr type local return            # не помечать собственные адреса ноды
[ct state established,related return]  # только в mark_out
ip daddr @aicn4 return                 # direct whitelist: не маркировать никогда
ip daddr @ai4 meta mark set 0x3        # AI-сервисы — раньше ru4
ip daddr @ru4 meta mark set 0x2        # RU-зона
```

`ai-cn` — **direct whitelist**, а не exit-группа: у неё нет ни марки, ни
`ip rule`, ни таблицы, ни выхода. `return` означает «оставить пакет без метки»,
и он уходит по `main`, то есть с IPv4 самой ноды. Поэтому проверка «работает»
здесь выглядит наоборот: не «марка ведёт в нужный туннель», а «марки нет и
пакет выходит с адреса ноды» (`ip route get <ip>` → `via … dev ens3 src
<IP ноды>`), при этом адрес, попавший и в `aicn4`, и в `ai4`, всё равно уходит
напрямую — `return` стоит выше марки.

`ip rule add fwmark 0x3 lookup 300 priority 3090`. У таблицы 300 есть нижняя
граница — гард `unreachable default metric 1000`: пока реального дефолта нет,
AI-трафик блокируется, а не выходит из NL (fail-closed по решению владельца;
историю и TODO смотреть в `routing.md` в `env`, §8). Когда US-выход появляется,
под гард ставится `ip route replace default dev <us-туннель> table 300 metric 10`
и гард остаётся инертным. Актуальное состояние — `ip route show table 300`:
дефолты = выход включён, только `unreachable` = группа закрыта.

## Маршрутизация: группа `exch` (биржи → RU-выход)

Китайские криптобиржи не заводят свою страну: им нужен **RU-адрес**, поэтому
сет `exch4` маркируется чужой маркой `0x2` и едет по существующей таблице 200
(выходы `wg-ru-ufo` `171.22.134.156` / `wg-ru-aeza` `171.22.130.17`, тот же
fail-closed гард и тот же watchdog). Своих `ip rule`, таблицы и probe-таблиц у группы
нет — иначе это была бы вторая копия тех же двух туннелей.

```
fib daddr type local return            # mark_pre; в mark_out — ct state established,related
ip daddr @aicn4    return              # direct whitelist (китайский AI) — выше всего
ip daddr @exch4    meta mark set 0x2 return   # биржи -> RU, ДО ai4 и youtube4, и СТОП
ip daddr @ai4      meta mark set 0x3   # AI (сейчас fail-closed гард: blocked)
ip daddr @ru4      meta mark set 0x2
ip daddr @youtube4 meta mark set 0x6   # только nl01
```

Почему именно так:

* **выше `ai4`/`youtube4`** — общий CDN-адрес, попавший в две группы, обязан
  дать RU для торговой сессии, а не `unreachable` (ai) и не KZ (youtube);
* **`return` в правиле `exch4` обязателен.** `meta mark set` не прекращает
  разбор цепочки: без `return` следующее сработавшее правило (`@ai4`/`@ru4`,
  `@youtube4`) перезапишет марку. Пересечение зон не гипотетическое — CloudFront
  отдаёт один и тот же edge и бирже, и AI-сервису (`99.84.91.11` был в `exch4`
  и `ai4` одновременно), и тогда биржа уходила в US-группу или в блок гарда.
  С `return` замер на netns-пире: адрес в обеих группах — `wg-ru-ufo` +2.3 КБ,
  AI-туннели 0; до правки — AI +2.5 КБ, RU 0;
* **та же марка, что `ru4`** — относительный порядок с `ru4` не важен, результат
  одинаковый; смена «родительской» группы автоматически меняет путь бирж;
* **`aicn4` остаётся выше** — whitelist это обещание «не маркировать никогда».

Файлы на нодах: сет засевается `frkn-msk-route.sh` из
`/etc/frkn/geo/exch4.zone` при пересоздании таблицы, дальше контент обновляет
`frkn-geo-exch4.timer` → `frkn-geo-exch4-refresh.sh` (обёртка над общим
`frkn-geo-ai4-refresh.sh`: условный GET → sha256 → порог 60 % → один `nft -f`).

Ограничение: проверяется и маршрутизируется только имя, отвечающее своим
сертификатом. Статические/CDN-хосты биржи (страница, статика, отдельный WS-путь)
в набор не попадут, пока не добавлены явно в список доменов — такая страница
может открыться с NL-адреса и показать «restricted region» при рабочем API.

## Маршрутизация: группа `adult` (adult → всегда NL)

Второй **direct whitelist** (первый — `ai-cn`). Владелец решил 21.11.2026:
взрослые сайты обязаны всегда выходить с публичного IPv4 NL-входа ноды и
никогда через RU (таблица 200), US (300) или KZ (600). Причина правовая, а не
сетевая: в Нидерландах рабочий режим без требований возрастной верификации в
стиле UK/US, тогда как RU-выход подставил бы пользователей под российское
законодательство; вдобавок `ru4` — большой агрегированный список префиксов,
который ловит hosting-адреса этих сайтов случайно.

```
nft set   inet frkn_geo adult4        # /32 из adult4.zone, владелец — frkn-geo-adult4.timer
mark_pre  ip daddr @adult4 return     # второй, сразу после @aicn4, выше всех марок
mark_out  ip daddr @adult4 return     # то же для трафика самой ноды
```

* **порядок — несущая часть.** Правило стоит сразу после `aicn4` и выше
  `exch4`/`ai4`/`ru4`/`youtube4`: тот же CDN-адрес, попавший в несколько
  наборов, всё равно выйдет из NL;
* **нет марки, `ip rule`, таблицы, выхода и watchdog'а** — и нет fail-closed
  гарда, который мог бы «заблокировать» группу: `return` не маркирует пакет, и
  он уходит по `main` с адресом ноды;
* **`aicn4` остаётся первым** — оба whitelist'а дают прямой путь, но если
  адрес попал в оба набора, порядок между ними на результат не влияет.

Приёмка (netns-пир, тот же `prerouting`, что у реального клиента; обе ноды):
`www.pornhub.com` (`66.254.114.41`) — **200**, exit IP = IP ноды
(`132.243.203.40` / `176.124.198.154`), дельты `wg-ru-ufo`/`wg-ru-aeza`/
`wg-us0`/`wg-us1`/`wg-kz0` — 0 байт; `ip route get <ip> mark 0x2` показывает
«увело бы в `wg-ru-ufo` table 200», то есть марка реально доступна правилу;
при временном добавлении адреса в `@ru4` результат тот же (200, туннели
стоят), а в контроле без членства в `adult4` запрос уходит в RU-туннель
(+2016 байт) и отвечает 301. После теста `ru4`/`adult4` переприменены из зон.

Файлы на нодах: сет засевается `frkn-msk-route.sh` из
`/etc/frkn/geo/adult4.zone` при пересоздании таблицы, дальше контент обновляет
`frkn-geo-adult4.timer` → `frkn-geo-adult4-refresh.sh` (обёртка над общим
`frkn-geo-ai4-refresh.sh`: условный GET → sha256 → порог 60 % → один `nft -f`).

## Как это потребляет роутер (`ru.zone`)

Порядок: условный GET → `304` = ничего не делать; `200` = скачать, сверить
sha256 с `.sha256`, применить; любая ошибка/таймаут = остаться на вшитой копии.
Вшитая копия в прошивке (`/etc/frkn/geo/ru.zone`) должна оставаться на месте —
она и есть fallback для офлайна.

```sh
#!/bin/sh
# /usr/lib/frkn/geo-refresh.sh — пример для роутера
set -eu
API="https://api.frkn.org/router/geo"
LIVE="/var/lib/frkn/geo/ru.zone"     # рабочая копия (tmpfs/overlay)
BAKED="/etc/frkn/geo/ru.zone"        # вшитая в прошивку
STATE="/var/lib/frkn/geo/etag"       # ETag прошлой удачной загрузки
TMP="$(mktemp)"
HDR="$(mktemp)"
trap 'rm -f "$TMP" "$HDR"' EXIT

# 1. Условный GET: если список не менялся, nginx ответит 304 и тела не будет.
code="$(curl -sS -o "$TMP" -D "$HDR" -w '%{http_code}' \
    --connect-timeout 10 --max-time 30 \
    -H "If-None-Match: $(cat "$STATE" 2>/dev/null || echo '')" \
    "$API/ru.zone" || echo 000)"
case "$code" in
  304) exit 0 ;;
  200) ;;
  *)   exit 0 ;;   # любая ошибка — остаёмся на прежней копии
esac

# 2. Сверяем контрольную сумму (её отдаёт соседняя ручка и тот же ETag).
want="$(curl -fsS --connect-timeout 10 --max-time 15 "$API/ru.zone.sha256" | awk '{print $1}')"
got="$(sha256sum "$TMP" | awk '{print $1}')"
if [ "$want" != "$got" ]; then exit 1; fi

# 3. Применяем целиком, не показывая полузаписанный файл.
mv "$TMP" "$LIVE"
tr -d '\r' < "$HDR" | awk -F': ' 'tolower($1)=="etag"{print $2}' > "$STATE"
```

Практические замечания:

- ETag можно не запоминать: `curl -z "$LIVE"` использует `If-Modified-Since`,
  который api сейчас **не** обрабатывает (поддержан только `If-None-Match`),
  поэтому ETag храните рядом с файлом или берите из предыдущего ответа.
- Скачивать в `tmp` **на том же разделе**, что `LIVE`, иначе `mv` будет
  копированием и окно полуфайла станет реальным.
- Проверять sha256 стоит всегда: обрыв связи на середине даёт `200`, но
  неполный файл.

## Остаточные режимы отказа `ai4` и чем они закрыты

| Режим | Что происходит | Чем закрыто |
|---|---|---|
| **Общий CDN-IP** (Cloudflare и подобные): один IP обслуживает тысячи имён | проверив `claude.ai`, мы заворачиваем в `ai4` весь этот IP, включая чужие сайты за тем же фронтом | принимается осознанно: ложное попадание означает «чужой сайт вышел через тот же VPN-выход», а не утечку; при появлении US-выхода это станет заметно по геолокации сайта — тогда IP-набор дробится по конкретным A-записям, а не по фронту |
| **Geo-DNS / разные ответы у резолверов** | 1.1.1.1 и 8.8.8.8 могут отдать разные edge-IP | оба ответа объединяются и **оба** проверяются TLS; в набор попадают только работающие |
| **Устаревший IP** (A-запись ушла, IP переехал другому) | трафик на этот IP уходил бы не туда | каждый час полная перепроверка; не прошедший TLS IP выпадает на следующем прогоне; из состояния он тоже уходит, как только домен даёт свежие IP |
| **Сервис недоступен ровно в момент прогона** | домен даёт 0 проверенных IP | берётся прошлое поколение этого домена (`ai4.state.json`), в лог — `reusing previous generation` |
| **Сломан DNS/TLS на самом хосте api** | почти все домены «мертвы» | порог `--min-fresh 50`: публикация отменяется, прошлая зона остаётся |
| **Массовое сжатие набора** | половина сервисов пропала бы из маршрута | порог `--min-ratio 60` на api и такой же на ноде |
| **Домен не отдаёт apex** (`oaiusercontent.com` — NXDOMAIN, только поддомены) | суффикс ничего не даёт | в списке указывается реальный хост (`files.oaiusercontent.com`); такие случаи видны в `--verbose` как `none` |
| **Проверка не видит «настоящий» край** (сертификат с другим SAN на том же IP) | IP отбрасывается, сервис не маршрутизируется | это цена надёжности: «медленнее, но всегда»; лечится добавлением в список того имени, которое реально отдаёт сервис |

## Проверить свежесть (поддержка / дежурный)

```bash
# Заголовки без скачивания файла: ETag, Last-Modified, Content-Length.
curl -sI https://api.frkn.org/router/geo/ai4.zone

# 304, если список не менялся с прошлого раза:
curl -sI -H 'If-None-Match: "sha256:<hex>"' \
    https://api.frkn.org/router/geo/ai4.zone | head -1

# Сколько /32 и какая сумма отдаётся сейчас:
curl -s https://api.frkn.org/router/geo/ai4.zone | wc -l
curl -s https://api.frkn.org/router/geo/ai4.zone.sha256
curl -s https://api.frkn.org/router/geo/ai4.zone | sha256sum

# На хосте api: что лежит на диске и когда обновлялось.
sha256sum /var/lib/frkn/geo/ai4.zone
jq '.generated_at, .stale, .cidr_count, .elapsed_s' /var/lib/frkn/geo/ai4.state.json
systemctl list-timers 'frkn-geo*'
systemctl status frkn-geo-ai-update.service --no-pager
```

Сверка «диск = ручка»: `sha256sum /var/lib/frkn/geo/ai4.zone` должен совпадать
с `curl -s .../ai4.zone | sha256sum`, а `ETag` из `curl -sI` — содержать тот же
хэш. `Last-Modified` старше часа — таймер не отработал (`systemctl --failed`,
`journalctl -u frkn-geo-ai-update`). На ноде: `nft list set inet frkn_geo ai4`
должен содержать столько же элементов, сколько строк в зоне
(`curl -s .../ai4.zone | wc -l`).
