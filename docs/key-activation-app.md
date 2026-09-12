# Активация ключей из приложения

Как приложению принимать активационный ключ (вида `PAXOY-NWQXM-AACRS-SJNHE-NTTC7-M`),
валидировать его, активировать и довести пользователя до конфига.

Base URL: `https://api.frkn.org`

Обе ручки публичные (без auth-токена). Ключ одноразовый: активировать можно один раз.

## Форматы ключей

Два типа, внешне неотличимы — тип определяется ответом `/key/validate`:

| kind | Что даёт | Особенности |
|---|---|---|
| `standard` | N дней подписки | Можно активировать "в себя" (новая подписка) или на существующую (`subscription_id`) |
| `lite` | N ГиБ трафика, без срока | Требует `email` при активации; подписка без `expires_at`, живёт пока есть трафик |

Пользователи путают ключ с subscription id (UUID) и ссылкой на конфиг — на вводе
принимайте и нормализуйте: обрезать пробелы/переносы, верхний регистр. Если ввели
UUID — это не ключ, направляйте в флоу подписки, а не активации.

## Workflow

```
ввод ключа → GET /key/validate → показать что даст ключ (дни/трафик)
           → POST /key/activate → subscription_id
           → GET /subscription/{id} (статус) / фиды конфигов
```

### 1. Валидация: `GET /key/validate?key={code}`

Ответ 200, ключ валиден:

```json
{
  "status": 200,
  "message": "Key is valid",
  "response": {
    "id": "a9e75125-...",
    "instance": {
      "Key": {
        "code": "PAXOY-...",
        "days": 30,
        "activated": false,
        "kind": "standard",
        "traffic_bytes": null,
        "traffic_gib": null,
        "subscription_id": null
      }
    }
  }
}
```

Важно: объект ключа лежит в `response.instance.Key` (обёртка варианта), не в
`response.instance` напрямую.

- `kind: "standard"` → показываем «N дней» (`days`)
- `kind: "lite"` → показываем «N ГБ» (`traffic_gib`)
- `activated: true` + message `"Key is valid and already activated"` → ключ уже
  использован; активация вернёт 400, покажите «ключ уже активирован»

Ошибки:

| HTTP | message | Значение |
|---|---|---|
| 400 | `Key is not valid` | Не прошёл checksum/подпись — опечатка в ключе |
| 404 | `Key is not found` | Корректный формат, но такого ключа нет |

### 2. Активация: `POST /key/activate`

```json
{
  "code": "PAXOY-NWQXM-AACRS-SJNHE-NTTC7-M",
  "subscription_id": null,
  "email": null
}
```

- `subscription_id` (опционально): продлить/пополнить существующую подписку
  вместо создания новой. Приложение передаёт его, если ключ вводится из профиля
  с уже привязанной подпиской.
- `email`: **обязателен для lite-ключей** — к нему привязывается аккаунт
  (восстановление доступа, уведомления о кончающемся трафике). Для standard
  игнорируется. Если lite без email → `400 "email_required"` — покажите поле
  ввода email и повторите.
- Опциональный заголовок `x-trace-id`: для сквозной трассировки в аудите.

Успех 200: message `"Key {id} activated"`, в `response.instance.Key.subscription_id`
— id подписки. Дальше работаем с ней как обычно (создание устройств, конфиги).

Ошибки:

| HTTP | message | Что показать |
|---|---|---|
| 404 | `Key not found` | «Ключ не найден» |
| 400 | `Key already activated` | «Ключ уже использован» |
| 400 | `email_required` | Запросить email (только lite) |

### 3. После активации

`GET /subscription/{id}` — статус подписки:

- standard: `expires` (RFC3339), `plan_kind: "standard"`
- lite: `expires: null`, `limit_bytes`, `used_bytes`, `remaining_bytes`,
  `plan_kind: "lite"`

Конфиги/подключения — стандартные ручки (`/subscription/{id}?format=...`,
`/v1/config` и т.д., см. [API.md](API.md)).

## Маппинг ошибок для UI (шпаргалка)

| Ответ API | Текст для пользователя |
|---|---|
| 400 `Key is not valid` | «Проверьте ключ — похоже, опечатка» |
| 404 `Key is not found` | «Такой ключ не существует» |
| 400 `Key already activated` | «Ключ уже активирован» |
| 400 `email_required` | «Введите email — к нему привяжем аккаунт» |
| 200, `activated: true` на validate | «Ключ уже активирован» (до отправки activate) |

## Замечания

- Не пытайтесь определить тип ключа по строке — только через `/key/validate`.
- Активация неидемпотентна по ключу (повтор → 400), идемпотентность по
  подписке даёт `x-trace-id` при ретраях сети.
- Продление трафиком на истёкшей подписке (top-up без ключа) — отдельный флоу
  через платёжный шлюз, не через `/key/activate`.
