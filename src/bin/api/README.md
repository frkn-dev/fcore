# FRKN API Service

Core API that manages subscriptions, connections, nodes, traffic accounting and metrics ingestion.

## What it does

- Stores subscriptions and connection state in PostgreSQL.
- Accepts metric samples from nodes/agents over ZeroMQ.
- Serves client subscription configs, admin panel and internal API.
- Runs background tasks: subscription expiry/restore, connection expiry, traffic persistence, node monitoring.
- Writes audit logs for subscription day-balance changes.

## Files

- `api-example.toml` — full example configuration.
- `api.service` — systemd unit.
- `docs/nginx.conf` — example nginx reverse proxy.

## Quick start

```bash
cp src/bin/api/api-example.toml /etc/fcore/api/config.toml
# edit config
cargo build --release --bin api --no-default-features
sudo cp target/release/api /usr/local/bin/api
sudo cp src/bin/api/api.service /etc/systemd/system/api.service
sudo systemctl daemon-reload
sudo systemctl enable --now api
```

## Deploy from release

```bash
sudo ./deploy/api-deploy.sh v0.5.16
```
