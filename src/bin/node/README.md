# FRKN Node Agent

Agent that runs on every proxy node. It registers the node with the API, syncs connections and reports stats.

## What it does

- Registers node info and inbounds with the API.
- Subscribes to API updates over ZeroMQ.
- Optionally drives Xray, WireGuard, AmneziaWG, Hysteria2 and MTProto proxies.
- Reports traffic/online metrics to the API metric receiver.
- Periodically saves local snapshots for fast restart.

## Files

- `node-example.toml` — full example configuration.
- `node.service` — systemd unit.

## Quick start

```bash
cp src/bin/node/node-example.toml /etc/fcore/node/config.toml
# edit config
cargo build --release --bin node --features xray,wireguard,amnezia-wg
sudo cp target/release/node /usr/local/bin/node
sudo cp src/bin/node/node.service /etc/systemd/system/node.service
sudo systemctl daemon-reload
sudo systemctl enable --now node
```

## Deploy from release

```bash
sudo ./deploy/node-deploy.sh v0.5.16
```

Choose the architecture file downloaded by the script: `node-x86_64`, `node-aarch64` or `node-armv7`.
