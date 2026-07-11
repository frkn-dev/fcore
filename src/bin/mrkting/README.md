# FRKN Marketing Service

Standalone service for marketing flows: trial creation, email capture, welcome emails and referral tracking.

## What it does

- `POST /account` — creates a trial subscription via the API or attaches an email to an existing subscription.
- `GET /validate/ref_code?code=XXX` — checks whether a referral code is valid.
- `GET /referrals?code=XXX` — returns the number of users invited by a referral code.
- `GET /subscription/by_ref_code?code=XXX` — returns `subscription_id` linked to a referral code.
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
