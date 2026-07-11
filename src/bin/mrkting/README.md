# FRKN Marketing Service

Standalone service for marketing flows: trial creation, email capture and welcome emails.

## What it does

- `POST /trial` — creates a trial subscription via the API, stores the email locally and sends a welcome email.
- `POST /email` — attaches an email to an existing subscription.
- Stores emails encrypted so they can be decrypted for mailouts.

## Files

- `mrkting-example.toml` — full example configuration.
- `mrkting.service` — systemd unit.
- `docs/nginx.conf` — example nginx reverse proxy.

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
