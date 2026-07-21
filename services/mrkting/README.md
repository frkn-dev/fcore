# FRKN Marketing Service

Standalone service for marketing flows: trial creation, email capture, welcome emails and referral tracking.

## What it does

- `GET /healthcheck` — no auth.
- `POST /account` — no auth (public registration/trial).
- `GET /referrals?code=XXX` — no auth.
- `GET /check/ref_code?code=XXX` — no auth, returns `{ "valid": true|false }`.
- `GET /validate/ref_code?code=XXX` — **requires `Authorization: Bearer {service.token}`**, returns `{ "valid": true|false, "subscription_id": "..." }`. Trial subscriptions are rejected.
- `GET /subscription/by_ref_code?code=XXX` — **requires `Authorization: Bearer {service.token}`**.
- `GET /subscription/trial?subscription_id=XXX` — **requires `Authorization: Bearer {service.token}`**, returns `{ "subscription_id": "...", "trial": true|false }`.
- `POST /subscription/extend` — **requires `Authorization: Bearer {service.token}`**, body `{ "subscription_id": "...", "expires_at": "2026-..." }`. Sets `trial = false` and updates `expires_at`.
- `GET /analytics/trials?period=&granularity=` — **requires `Authorization: Bearer {service.token}`**.
- `GET /analytics/conversions?period=&granularity=` — **requires `Authorization: Bearer {service.token}`**. Считает trial-подписки, у которых `converted_at` заполнен (то есть оплатили).
- `POST /surveys/reward` — **requires `Authorization: Bearer {survey.token}`**. Выдаёт триальный ключ за прохождение опроса. Проверяет дубликаты по `email` + `campaign`. Отправляет ключ на email.
- `GET /analytics/referrals?period=&granularity=` — **requires `Authorization: Bearer {service.token}`**. Считает подписки, созданные по реферальному коду (`referred_by IS NOT NULL AND referred_by != 'WEB'`).
- Stores emails encrypted so they can be decrypted for mailouts.

## Files

- `mrkting-example.toml` — full example configuration.
- `mrkting.service` — systemd unit.
- `docs/nginx.conf` — example nginx reverse proxy.

## Database

The service uses its own PostgreSQL database (default `mrkting`). Apply the migration:

```bash
psql -h localhost -U postgres -d mrkting -f dev/migrations/mrkting.sql
```

## Quick start

```bash
cp src/bin/mrkting/mrkting-example.toml /etc/fcore/mrkting/config.toml
# edit config
cargo build --release --bin mrkting --no-default-features
sudo cp target/release/mrkting /usr/local/bin/mrkting
sudo cp src/bin/mrkting/mrkting.service /etc/systemd/system/mrkting.service
sudo systemctl daemon-reload
sudo systemctl enable --now mrkting
```

## Deploy from release

```bash
sudo ./deploy/mrkting-deploy.sh v0.5.16
```

## Email encryption key

`email_encryption.key` must be a base64-encoded 32-byte AES-256-GCM key. Generate one with:

```bash
openssl rand -base64 32
```
