# meshd — подготовка выкладки (не выполнять отсюда)

Отдельный бинарь. Старый модуль `src/mesh/` внутри api не трогаем и не выключаем этим шагом.

## Сборка

```
cargo build --release --bin meshd
```

Бинарь: `target/release/meshd`.

## Каталог и конфиг

```
sudo install -d -o fcore -g fcore -m 750 /var/lib/fcore/mesh /var/log/fcore/mesh /etc/fcore/mesh
```

`/etc/fcore/mesh/config.toml`:

```toml
[service]
listen = "127.0.0.1:8095"
db_path = "/var/lib/fcore/mesh/mesh.db"

[pg]
host = "127.0.0.1"
port = 5432
db = "fcore"
username = "fcore"
password = "..."
```

Секция `[pg]` — та же база, что у api, только чтение: `SELECT is_deleted FROM subscriptions WHERE id = $1`. Срок подписки не смотрится. Пользователю БД достаточно `SELECT` на `subscriptions`.

## systemd

Образец: `src/bin/api/api.service`. Юнит для meshd: `src/bin/mesh/meshd.service`.

```
sudo cp target/release/meshd /usr/local/bin/meshd
sudo cp src/bin/mesh/meshd.service /etc/systemd/system/meshd.service
sudo systemctl daemon-reload
sudo systemctl enable --now meshd
```

## nginx

На том же хосте, что api. Скопировать `/etc/nginx/sites-enabled/api` в `mesh.frkn.app` и заменить:

- `server_name mesh.frkn.app;`
- `proxy_pass http://127.0.0.1:8095;`
- сертификат тем же способом, что у api (тот же выпуск certbot / тот же сниппет ssl).

Для long-poll (`POST /v1/mesh/poll`, до 30 с) в `location` выставить `proxy_read_timeout 60s;` и не буферизовать ответ дольше таймаута. `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;` — meshd берёт IP клиента из этого заголовка для лимита 60/мин.

```
sudo nginx -t && sudo systemctl reload nginx
```

## После переката, отдельным шагом

- выключить маршруты `/v1/mesh` в api (модуль `src/mesh/` в этом изменении не трогался);
- тестовый реестр 9-значных UIN живёт в снимке api, не в SQLite meshd — его сносим отдельно, устройства тестовые.
