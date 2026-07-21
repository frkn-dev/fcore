# FRKN Business Dashboard

Internal dashboard that aggregates business metrics from pixel analytics, payment gateway, and mrkting services.

## What it shows

- Visits (24h) from pixel-agent
- Trials (24h) — wired once the payment-gateway trials endpoint is ready
- Payments (24h) from payment gateway
- Revenue (24h) from payment gateway
- Revenue and visits charts
- Sales breakdown by type, duration, and promocode

## Files

- `dashboard-example.toml` — full example configuration.
- `dashboard.service` — systemd unit.
- `docs/nginx.conf` — example nginx reverse proxy for `dashboard.frkn.org`.

## Configuration

```toml
[pixel]
endpoint = "http://127.0.0.1:9102"

[payment]
endpoint = "http://127.0.0.1:3006"
analytics_token = "your-analytics-token"

[mrkting]
endpoint = "http://127.0.0.1:3007"

[dashboard]
listen = "127.0.0.1"
port = 9103
```

## Quick start

```bash
cargo build --release --bin dashboard --no-default-features
sudo cp target/release/dashboard /usr/local/bin/dashboard
sudo cp src/bin/dashboard/dashboard.service /etc/systemd/system/dashboard.service
sudo systemctl daemon-reload
sudo systemctl enable --now dashboard
```

## Deploy from release

```bash
sudo ./deploy/dashboard-deploy.sh v0.5.21
```

## Nginx

See `src/bin/dashboard/docs/nginx.conf`. The dashboard is protected with basic auth at `dashboard.frkn.org`.
