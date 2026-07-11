# FRKN Auth Service

Authentication sidecar that keeps a local copy of connection state and answers `/auth` requests from proxy nodes.

## What it does

- Subscribes to API updates over ZeroMQ.
- Syncs connection state for a specific node/env from the API.
- Exposes `/auth` endpoint for inbound authentication requests.
- Reports metrics to the API metric receiver.

## Files

- `auth-example.toml` — full example configuration.
- `auth.service` — systemd unit.

## Quick start

```bash
cp src/bin/auth/auth-example.toml /etc/fcore/auth/config.toml
# edit config
cargo build --release --bin auth --no-default-features
sudo cp target/release/auth /usr/local/bin/auth
sudo cp src/bin/auth/auth.service /etc/systemd/system/auth.service
sudo systemctl daemon-reload
sudo systemctl enable --now auth
```

## Deploy from release

```bash
sudo ./deploy/auth-deploy.sh v0.5.16
```
