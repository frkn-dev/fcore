# Peers API

Схема «пиров»: пара ключей живёт на устройстве, адрес выдаёт только api. Заменяет серверные ключи
для новых env (`standard-*`, `super-duper-*`); `connections` (vless, hysteria2, статик wg/awg)
продолжают работать как раньше.

Контракт целиком: `../../PEERS-SPEC.md` (вне репозитория). Здесь — то, что нужно вызывающему.

## Кто есть кто

* **grant** — право подписки на env: `protos[]`, свой срок, свой лимит трафика. Одна строка на env.
* **device** — установка приложения: `device_uid` (uuid, его генерит клиент), `device_name`, ключ по
  умолчанию, `support_tag` — короткий код для поддержки (`7K2M-9XQ4P`).
* **peer** — туннель: устройство + env + протокол + адрес `/32`. Может нести свой ключ (роутер).

## Клиентские ручки (секрет — uuid подписки)

### `GET /v1/locations?subscription_id=<uuid>`

Что доступно и куда подключаться.

```json
{
  "access": [
    { "env": "standard-ru", "protos": ["amnezia_peer", "wireguard_peer"],
      "expires_at": null, "traffic_limit": null, "used_bytes": 0 }
  ],
  "pools": [
    { "env": "standard-ru", "pool": "MSK", "pool_translation": {"ru": "Москва"},
      "proto": "amnezia_peer",
      "nodes": [{ "endpoint": "1.2.3.4:51820", "server_public_key": "…",
                  "mtu": 1280, "dns": ["1.1.1.1"], "keepalive": 25,
                  "obfuscation": {"jc": 4, "jmin": 40, "jmax": 70, "s1": 30, "s2": 60,
                                  "h1": "…", "h2": "…"} }] }
  ]
}
```

`pools` — карточки в приложении (одна на пул внутри env). Адрес и ключ у всех карточек одного env
общие, меняются только `endpoint`/`server_public_key`/обфускация.

### `POST /v1/peers`

```json
{ "subscription_id": "…", "device_uid": "…", "device_name": "iPhone",
  "env": "standard-ru", "proto": "amnezia_peer", "public_key": "…", "label": null }
```

`public_key` обязателен при первом вызове с новым `device_uid`; дальше берётся ключ устройства.
`label` — имя канала у роутера (`ch2`), у телефона пустой.

Ответ: `{ peer: {...}, support_tag, pools: [...] }` — конфиг собирается сразу, без второго запроса.

Идемпотентность: тот же `public_key` в том же env → тот же пир и `200`.

Коды: `400` битый ключ/метка · `402` грант истёк или квота исчерпана · `403` нет права на env ·
`404` подписка/устройство · `409` лимит (5 на устройство, 50 на подписку) или занятый ключ/метка ·
`503` нет свободных адресов.

### `GET /v1/peers?subscription_id=<uuid>&device_uid=<uuid>`

Живые и недавно отозванные туннели. `revoke_reason`: `user` / `expired` / `quota` /
`subscription_ended` — по нему приложение отличает «сервер снял» от «подписка кончилась».

### `DELETE /v1/peers/{peer_id}` — тело `{ "subscription_id": "…" }`

Снимает туннель (`revoke_reason = user`), адрес возвращается в пул.

### Устройства

* `GET /v1/devices?subscription_id=` — `{id, device_uid, device_name, support_tag, peers}`
* `PATCH /v1/devices/{id}` — `{subscription_id, device_name}`
* `DELETE /v1/devices/{id}` — `{subscription_id}`, снимает все туннели устройства

## Активация ключа: две ручки

Тело запроса и ответ — одинаковые. Разница только в том, как выдаётся доступ:

| ручка | модель | тумблер раскатки |
|---|---|---|
| `POST /v1/key/activate` | старая: каталог продуктов + срок подписки | пока сюда смотрит сайт |
| `POST /v1/activate` | новая: шард + грант, без отката | сюда переключается сайт, когда тест окончен |

`/v1/activate`:

- выбирает шард (`env_capacity.plan` + живые ноды), пишет `subscription_access`;
- коннекты v1 создаются **из гранта**, а не из каталога;
- если для плана не объявлено ни одного шарда или все они без живых нод → `503`, ключ
  **не расходуется**, попытка пишется в `activation_failures` для поддержки;
- при успехе в ответе есть `env` и `capacity {used, limit, overflow}`.

Переключение и откат — это URL на сайте, без рестарта api. Когда старая ручка перестанет получать
трафик, её можно удалить одним коммитом.

## Служебные ручки (Bearer-токен сервиса)

### `GET /v1/access?subscription_id=` — права подписки (для ЛК)

### `POST /v1/access` — upsert права

```json
{ "subscription_id": "…", "env": "standard-ru",
  "protos": ["amnezia_peer", "wireguard_peer"], "source": "payment",
  "expires_at": "2027-01-01T00:00:00Z", "traffic_limit": null }
```

Семантика: `protos` и `sources` объединяются, срок берётся позднее (`NULL` = бессрочно), лимит
заменяется, если пришёл не `NULL`. Env, которого нет в `env_networks`, отклоняется `403` — так
опечатка в имени видна сразу, а не у юзера кнопкой «подключиться».

### `DELETE /v1/access` — `{subscription_id, env}`

Отзывает право и сразу снимает все пиры этого env (не ждёт фонового таска).

### `GET /v1/admin/support/{support_tag}` — поиск для поддержки

Только чтение: устройство, подписка, туннели, права с расходом. Поддержка получает код от юзера;
код не даёт доступа к туннелям.

## Шина

* `peers-update-<env>` — `Vec<PeerMessage>`: `Create`/`Delete` одного туннеля. Слушают все ноды env.
* `peers-init-<uuid>` — полный набор для конкретной ноды, публикуется при её регистрации. Набор
  авторитетный: нода применяет его и снимает всё, чего в нём нет.

`PeerMessage = { peer_id, action, tag, public_key, address }` — без приватных ключей и без секретов.
