# FRKN Pixel Analytics Agent

Standalone daemon that parses nginx pixel logs and exposes web analytics with a built-in admin UI and Prometheus metrics.

## What it does

- Tails `/var/log/nginx/pixel.log` every polling interval.
- Parses pixel hits into page views, referrers, countries and UTM data.
- Aggregates metrics into time buckets and persists snapshots.
- Serves an admin dashboard on `admin_listen:admin_port`.
- Exposes `/metrics` in Prometheus format.

## Files

- `pixel-agent-example.toml` — full example configuration.
- `pixel-agent.service` — systemd unit.
- `pixel-agent-backfill.service` / `pixel-agent-backfill.timer` — periodic backfill.
- `docs/nginx.conf` — example nginx reverse proxy for the admin UI.

## Quick start

```bash
cp src/bin/pixel_agent/pixel-agent-example.toml /etc/pixel-agent/config.toml
# edit config
cargo build --release --bin pixel-agent --no-default-features
sudo cp target/release/pixel-agent /usr/local/bin/pixel-agent
sudo cp src/bin/pixel_agent/pixel-agent.service /etc/systemd/system/pixel-agent.service
sudo systemctl daemon-reload
sudo systemctl enable --now pixel-agent
```

## Backfill

```bash
sudo /usr/local/bin/pixel-agent-backfill /etc/pixel-agent/backfill.toml /var/log/nginx/pixel.log
```

## Deploy from release

```bash
sudo ./deploy/pixel-agent-deploy.sh v0.5.16
```
