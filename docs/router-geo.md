# Гео-листы для роутера: `/router/geo/*`

Публичные plain-text ручки api, из которых прошивка OpenWrt-роутера обновляет
**вшитый** список российских IPv4-подсетей. Вшитый список стареет вместе с
прошивкой; здесь лежит свежий, который обновляется раз в сутки и отдаётся с
условным GET, чтобы ежедневный опрос стоил один `304`.

Источник данных — [ipdeny](https://www.ipdeny.com/ipblocks/)
`ru-aggregated.zone` (агрегированные подсети, ~8.7 тыс. CIDR). Никакой БД за
ручками нет: истина — файл на диске, его sha256 — и сильный ETag, и то, что
отдаёт `.sha256`.

## Ручки

| Метод | Путь | Ответ |
|---|---|---|
| `GET`, `HEAD` | `/router/geo/ru.zone` | список CIDR, `text/plain; charset=utf-8`, один CIDR на строку, `\n` |
| `GET`, `HEAD` | `/router/geo/ru.zone.sha256` | `<sha256>  ru.zone\n` (формат `sha256sum`) |

Обе ручки — **без авторизации** и **без AGW-конверта**, как `/sub/<token>` и
`/key/validate`. Полный URL на проде: `https://api.frkn.org/router/geo/ru.zone`.

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
| `404` | файла ещё нет (обновлятор не запускался) | `ru.zone not published` |
| `503` | файл есть, но не проходит валидацию (обрезанный / мусор) | `geo list unavailable` |
| `500` | файл не читается (права, I/O) | `geo list unavailable` |

`503` — намеренно: лучше отдать ошибку, чем обрезанный список, который роутер
применит и уведёт не те префиксы. Роутер в этом случае остаётся на вшитой
копии. Ошибки не кэшируются (`Cache-Control: no-store`).

## Где лежит файл

`[service] geo_dir` в конфиге api, по умолчанию **`/var/lib/frkn/geo`**:

```
/var/lib/frkn/geo/ru.zone          # то, что отдаёт /router/geo/ru.zone
/var/lib/frkn/geo/ru.zone.sha256   # копия контрольной суммы для оператора
```

api читает **только** `ru.zone` и сам считает его sha256 — файл `.sha256`
нужен людям/скриптам на хосте, ручка его не читает (поэтому рассинхрон
соседних файлов не может отдать роутеру неверную сумму). Каталог должен быть
читаем пользователю, под которым работает api (`fcore`), файл — `0644`.

## Обновлятор (systemd, раз в сутки)

Файлы в репозитории — `deploy/geo/`:

| Файл | Что делает |
|---|---|
| `update-ru-zone.sh` | скачивает, валидирует и атомарно публикует список |
| `frkn-geo-update.service` | oneshot-юнит, запускает скрипт |
| `frkn-geo-update.timer` | расписание: ежедневно **04:30 UTC** + случайная задержка до 30 мин, `Persistent=true` |

Установка на хост api:

```bash
sudo install -d -m 0755 /usr/local/lib/frkn/geo /var/lib/frkn/geo
sudo install -m 0755 deploy/geo/update-ru-zone.sh /usr/local/lib/frkn/geo/
sudo install -m 0644 deploy/geo/frkn-geo-update.service \
                    deploy/geo/frkn-geo-update.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now frkn-geo-update.timer

# разовый прогон (он же «--once»): первый запуск и проверка после правок
sudo systemctl start frkn-geo-update.service
journalctl -u frkn-geo-update.service -n 20 --no-pager
```

Тот же прогон без systemd (например, на другой машине):

```bash
sudo GEO_DIR=/var/lib/frkn/geo ./deploy/geo/update-ru-zone.sh
```

### Что проверяет скрипт перед публикацией

1. `curl -fsS` с `--connect-timeout 10 --max-time 30`, HTTPS only, 2 ретрая.
2. `CRLF` нормализуется — прокси, переписавший переводы строк, не считается
   порчей.
3. Каждая строка — `a.b.c.d/0..32`, десятичные октеты `<= 255`, без пустых
   строк (awk: и регулярка, и числовое сравнение октетов).
4. Строк не меньше `MIN_LINES` (1000; реально ~8.7 тыс.) — ловит обрезанную
   загрузку и HTML-страницу ошибки.

Только после этого: `chmod 644`, `sha256sum`, запись `.sha256`, и **`mv`**
(rename в том же каталоге, атомарно) поверх `ru.zone`. Любая ошибка — ненулевой
код, `ru.zone` остаётся прежним, юнит падает видимо в `systemctl --failed`.
Если скачанный файл совпал по sha256 с текущим, скрипт ничего не трогает —
`Last-Modified`/ETag остаются стабильными, роутеры получают `304`.

Та же планка (`MIN_ZONE_LINES`, ~1000 строк) продублирована в api
(`src/bin/api/http/handlers/geo.rs`): обновлятор не публикует мусор, а api ещё
и отказывается его отдавать, если файл подменили руками.

## Как это потребляет роутер

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

# 1. Условный GET: если список не менялся, api ответит 304 и тела не будет.
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

## Проверить свежесть (поддержка / дежурный)

```bash
# Заголовки без скачивания файла: ETag, Last-Modified, Content-Length.
curl -sI https://api.frkn.org/router/geo/ru.zone

# 304, если список не менялся с прошлого раза:
curl -sI -H 'If-None-Match: "sha256:a115697bb8cacf10c211c59848448862c05411a62b21e343e8fb3fab0cb0a645"' \
    https://api.frkn.org/router/geo/ru.zone | head -1

# Сколько CIDR и какая сумма отдаётся сейчас:
curl -s https://api.frkn.org/router/geo/ru.zone | wc -l
curl -s https://api.frkn.org/router/geo/ru.zone.sha256
curl -s https://api.frkn.org/router/geo/ru.zone | sha256sum

# На хосте api: что лежит на диске и когда обновлялось.
sha256sum /var/lib/frkn/geo/ru.zone
stat -c '%y %s' /var/lib/frkn/geo/ru.zone
systemctl list-timers frkn-geo-update.timer
systemctl status frkn-geo-update.service --no-pager
```

Сверка «диск = ручка»: `sha256sum /var/lib/frkn/geo/ru.zone` должен совпадать
с `curl -s .../ru.zone | sha256sum`, а `ETag` из `curl -sI` — содержать тот же
хэш. `Last-Modified` старше суток — таймер не отработал (`systemctl --failed`,
`journalctl -u frkn-geo-update`).
