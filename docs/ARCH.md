# ffcore Architecture

## Overview

`ffcore` is a Rust VPN control plane and data plane. It is split into several binaries that share core domain types via the `fcore` library crate.

## Binaries

| Binary | Path | Responsibility |
|---|---|---|
| `api` | `src/bin/api/` | Central control plane. HTTP API (Warp), PostgreSQL persistence, in-memory state cache, ZeroMQ publisher, metrics aggregation, admin/premium panels. |
| `node` | `src/bin/node/` | Per-environment proxy node. Subscribes to ZMQ, applies connection changes to local backends (Xray, WireGuard, AmneziaWG, Hysteria2, MTProto), collects and publishes metrics, snapshots local state. |
| `auth` | `src/bin/auth/` | External authentication provider for Hysteria2. Subscribes to the `auth` topic, keeps a local cache of Hysteria2 tokens, exposes `/auth` HTTP endpoint for Hysteria2 auth requests. |

Feature flags gate protocol support at compile time:

- `wireguard` — WireGuard backend (`defguard_wireguard_rs`)
- `amnezia-wg` — AmneziaWG backend (netlink)
- `xray` — Xray gRPC handler/stats clients

## Data flow

```
┌─────────────┐      HTTP/Warp      ┌─────────────────┐
│  Clients    │ ◄──────────────────►│   api binary    │
│  (admin,    │                     │  • service.rs   │
│   premium,  │                     │  • sync/tasks   │
│   public)   │                     │  • postgres/*   │
└─────────────┘                     └────────┬────────┘
                                             │
                              write-first    │
                         ┌───────────────────┼───────────────────┐
                         ▼                   ▼                   ▼
                  ┌────────────┐     ┌─────────────┐     ┌──────────────┐
                  │ PostgreSQL │     │ In-memory   │     │ ZMQ Publisher│
                  │ (source of │     │ Cache       │     │              │
                  │  truth)    │     │ (Cache<N,   │     │              │
                  └────────────┘     │  C, S>)     │     └──────┬───────┘
                                     └─────────────┘            │
                                                                 │
                              ┌──────────────────────────────────┼──────────┐
                              ▼                                  ▼          ▼
                        ┌──────────┐                        ┌────────┐  ┌────────┐
                        │  node    │                        │  node  │  │  auth  │
                        │ (env N)  │                        │(env M) │  │        │
                        └──────────┘                        └────────┘  └────────┘
```

Typical write path:

1. HTTP handler validates the request.
2. `SyncOp` writes to PostgreSQL first.
3. On success, the in-memory `Cache` is updated.
4. A ZMQ message (or batch) is published to the relevant topic.
5. Nodes/auth service receive the message and apply it to the local proxy backend.

## Key modules

| Module | Role |
|---|---|
| `src/bin/api/http` | Warp routes, filters, request/response handlers, admin/premium panels. |
| `src/bin/api/postgres` | `PgContext` + repositories for nodes, connections, subscriptions, traffic, keys. |
| `src/bin/api/sync` | `MemSync` wrapper and `SyncOp` trait — the single path for DB→memory→ZMQ consistency. |
| `src/bin/api/tasks` | Background jobs: DB sync, expiry cleanup, node heartbeat monitor, traffic persistence. |
| `src/memory` | Core domain types: `Connection`, `Subscription`, `Node`, `Env`, `ProtoTag`, `Connections`, `Subscriptions`, storage traits. |
| `src/memory/connection` | Connection structs (`Conn`, `Base`), protocol enum (`Proto`), WireGuard params, operations. |
| `src/metrics` | `MetricEnvelope`, `MetricStorage`, `MetricBuffer`, Prometheus helpers. |
| `src/zmq` | `Publisher`, `Subscriber`, `Topic`, `Message`, `Action`. |
| `src/proto` | Optional backends: `xray`, `wireguard`, `amnezia_wg`. |
| `src/http` | Shared HTTP helpers and the service-token auth filter. |

## State synchronization

- **Database as source of truth.** Every mutating `SyncOp` persists to Postgres before updating memory.
- **Periodic DB sync.** `api` reloads nodes/connections/subscriptions from Postgres on a configurable interval (`tasks.db_sync_interval_sec`) and replaces the in-memory `Cache`.
- **Node registration.** On startup a node registers itself via `POST /node` and then calls `POST /connections/sync` for each supported tag. The API publishes all matching non-deleted connections to that node's `init-<uuid>` topic.
- **Connection expiry.** Background tasks delete expired connections/subscriptions from DB, memory, and nodes via ZMQ `Delete` messages.
- **Subscription restore.** When a subscription is re-activated, the restore task sends `Update` messages for previously deleted connections.
- **Node status.** Heartbeats arrive on the `metrics` topic; the API marks nodes `Online`/`Offline` in DB and memory.

## Snapshots

1. **Connection snapshots** (`src/memory/snapshot.rs`, used by `node` and `auth`)
   - `SnapshotManager` serializes `Connections<C>` to a file with rkyv.
   - On startup the node loads the snapshot and re-creates peers in WG/AWG/Xray.
   - A background task writes a new snapshot every `service.snapshot_interval` seconds.

2. **Metrics snapshot** (`src/metrics/storage.rs`, used by `api`)
   - `MetricStorage` saves/loads its time-series state to `settings.metrics.snapshot_path`.
   - Restored on API bootstrap; saved every 60 seconds.

## Premium panel concept

A subscription can be turned into a **premium parent** by an admin:

- `POST /admin/api/subscriptions/{id}/premium` sets `scope_env` and generates a `premium_token`.
- The parent authenticates to `/premium/*` endpoints with `Authorization: Bearer <premium_token>`.
- A parent can create **child subscriptions** (`POST /premium/child`). Children store `parent_id = parent.id`.
- Child connections created via `POST /premium/child/{id}/connections` are constrained to the parent's `scope_env`.
- Premium endpoints allow listing children, managing their connections, and viewing aggregated traffic.
- Child subscriptions do not participate in the referral bonus program.

## Authentication layers

| Layer | Mechanism | Used by |
|---|---|---|
| **Service token** | `Authorization: Bearer <settings.service.token>` | Subscription, connection, node, key management endpoints. Implemented in `src/http/filters.rs`. |
| **Admin token** | `Authorization: Bearer <settings.service.admin_token>` (API) or `?token=<admin_token>` (HTML page) | `/admin/api/*` and `/admin`. Configured by `admin_enabled` + `admin_token`. |
| **Premium token** | `Authorization: Bearer <subscription.premium_token>` | `/premium/*` endpoints. Filter in `src/bin/api/http/filters.rs`. |

Public endpoints (no auth): health check, subscription links/info/traffic, node lists, cluster lists, trial, key validation/activation, Amnezia gateway.

## Amnezia gateway

When `service.agw_private_key_path` is configured, the API exposes encrypted Amnezia-compatible endpoints under `/v1/*`:

- `POST /v1/services`
- `POST /v1/account_info`
- `POST /v1/config`

Requests and responses are encrypted with RSA+AES. See `docs/API.md` for a Python example, and `src/bin/api/http/crypto.rs` for the server-side implementation.

## Configuration

Example API config: `config-api-example.toml`. Key sections:

- `service` — listen address, tokens, CORS, admin flags, networks.
- `postgres` — DB connection.
- `zmq` — publisher/bind addresses.
- `tasks` — sync intervals and limits.
- `metrics` — snapshot path and retention.
- `enabled_envs` / `enabled_tags` — which envs/protocols the API allows.
