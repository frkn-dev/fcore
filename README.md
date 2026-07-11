[![Release](https://github.com/frkn-dev/fcore/actions/workflows/release.yml/badge.svg?branch=main)](https://github.com/frkn-dev/fcore/actions/workflows/release.yml)
[![Fcore Build](https://github.com/frkn-dev/fcore/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/frkn-dev/fcore/actions/workflows/rust.yml)

# Fc0re - a cluster platform for Xray/Shadowsocks/Hysteria2/Wireguard/Amnezia-Wireguard/MTproto

Fc0re is a lightweight control plane and orchestration platform for modern proxy protocols.
It simplifies the deployment and unified management of Xray, Shadowsocks, Hysteria2, MTproto, Wireguard and Amnezia-Wireguard servers,
providing a single pane of glass for your network infrastructure.

## Architecture

### Binaries

| Binary                 | Purpose                                                                                                           |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `api`                  | Core API: subscriptions, connections, nodes, traffic accounting, metrics ingestion, admin panel.                  |
| `node`                 | Agent that runs on every proxy node, drives Xray/Hysteria2/Wireguard/Amnezia-Wireguard/MTproto and reports stats. |
| `auth`                 | Auth sidecar: keeps a local copy of connection state and answers `/auth` requests from proxy nodes.               |
| `pixel-agent`          | Parses nginx pixel logs and serves web analytics with a built-in admin UI and Prometheus endpoint.                |
| `pixel-agent-backfill` | One-shot tool for rebuilding the pixel analytics snapshot from archived logs.                                     |
| `mrkting`              | Marketing service: trial creation, email capture and welcome emails.                                              |

### External dependencies

- **ZeroMQ** — control bus between API, nodes and auth services.
- **PostgreSQL** — subscription and node data storage.
- **Xray Core**
- **Hysteria2**
- **Teleproxy (MTProxy)**
- **Wireguard**
- **Amnezia Wireguard**
- **Nginx** — reverse proxy and pixel image endpoint

### Features

- Standalone Node — can run without external dependencies.
- Automatic Xray Config Parsing — reads `xray-config.json` to fetch inbounds and settings automatically.
- Low Resource Usage — works perfectly on low-cost 1 CPU ($3 VPS) machines.
- Protocol Support — handles VLESS TCP, VLESS gRPC, VLESS Xhttp, Hysteria2, Wireguard and Amnezia Wireguard connections.
- Cluster Management — API manages users and nodes across the entire cluster.
- Node Health Monitoring — API periodically checks the health and status of all connected nodes.
- Metrics System — system and logic metrics are collected in Graphite format and stored in memory with snapshot persistence.
- Web Analytics — built-in pixel analytics (visits, top pages, countries, referrers).
- Trial and Marketing Flows — managed by the standalone `mrkting` service.

## Getting Started

### Prerequisites

- **Rust** (nightly toolchain)
- **PostgreSQL** 17+
- **ZeroMQ** libraries installed on your system
- **Protobuf Compiler** (`protoc`)

### Build from source

```bash
git clone https://github.com/frkn-dev/fcore.git
cd fcore

cargo build --release --bin api --no-default-features
cargo build --release --bin auth --no-default-features
cargo build --release --bin node --features xray,wireguard,amnezia-wg
cargo build --release --bin pixel-agent --no-default-features
cargo build --release --bin pixel-agent-backfill --no-default-features
cargo build --release --bin mrkting --no-default-features
```

### Configuration

Each binary has its own example config, systemd unit and README inside `src/bin/<name>/`:

```bash
cp src/bin/api/api-example.toml /etc/fcore/api/config.toml
cp src/bin/auth/auth-example.toml /etc/fcore/auth/config.toml
cp src/bin/node/node-example.toml /etc/fcore/node/config.toml
cp src/bin/pixel_agent/pixel-agent-example.toml /etc/pixel-agent/config.toml
cp src/bin/mrkting/mrkting-example.toml /etc/fcore/mrkting/config.toml
```

### Deploy from a GitHub Release

Each binary has an install script in `deploy/`:

```bash
sudo ./deploy/api-deploy.sh v0.5.16
sudo ./deploy/auth-deploy.sh v0.5.16
sudo ./deploy/node-deploy.sh v0.5.16
sudo ./deploy/pixel-agent-deploy.sh v0.5.16
sudo ./deploy/mrkting-deploy.sh v0.5.16
```

## License

This project is licensed under the GPLv3 - see the LICENSE file for details.
