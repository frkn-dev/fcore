# Fc0re — AGENTS.md

Cluster control plane for Xray/Shadowsocks/Hysteria2/Wireguard/Amnezia-Wireguard/MTproto.
Rust, **nightly toolchain**, PostgreSQL + ZeroMQ. Binaries: `api`, `node`, `auth`, `mesh`.
Architecture and features: `README.md`. Internals: `docs/ARCH.md`.

## HUMANS.md — work status

The repo root has `HUMANS.md`: a human-readable status file with the current branch, task, done items, next steps, and blockers.

- At task start, update `Current` (branch, task, status, date).
- At task end, add a `Done` entry (date, short description, commit hash if any) and refresh `Next`.
- Keep `Done` short: last 5–10 items; full history lives in git.
- `HUMANS.md` is "what we are doing now"; `AGENTS.md` is "how we work". Do not duplicate rules.
- Never put secrets in it: tokens, passwords, private keys.
- For cross-repo work, use the same short Task ID and link the other `HUMANS.md` files.
- If blocked, record it under `Blockers / Notes`, not only in chat.

## What to read first

| Doc | Covers |
|---|---|
| `README.md` | what the binaries do, prerequisites, build & release flow |
| `docs/ARCH.md` | module layout, task model, how api/node/auth talk |
| `docs/API.md` | HTTP API surface (`/v1/...`, `/sub/<token>`, `/private/...`) |
| `docs/PROTO.md` | api ↔ node ↔ auth protocol on the ZeroMQ bus |
| `docs/node-pool.md`, `docs/private-nodes.md` | pools, private nodes, share tokens |
| `docs/*-setup.md` | per-protocol node setup (VLESS/REALITY, Shadowsocks, VMess, SNI) |
| `deploy/migrations/README.md` | migration policy |

## Layout

- `src/bin/{api,node,auth,mesh}/` — each has its own `*-example.toml`, README and systemd unit.
- `src/proto/` — xray protobuf; `build.rs` compiles it with a vendored `protoc` (feature `xray`).
- `deploy/` — `api-deploy.sh`, `auth-deploy.sh`, `node-deploy.sh`, `migrate.sh`, `migrations/`.
- `dev/` — local helpers: `docker-compose.yml` (throwaway postgres), `*.sql`, `*.requests`, `xray-config.json`, `h2.yaml`.
- `docs/`, `experimental/`, `snapshots/` — reference material; do not treat snapshots as source.

## Build & test

```bash
cargo build --release --bin api  --no-default-features
cargo build --release --bin auth --no-default-features
cargo build --release --bin node --features xray,wireguard,amnezia-wg

cargo test --lib
```

- Feature flags are load-bearing: `api`/`auth` build **without** default features, `node` needs `xray,wireguard,amnezia-wg`. A plain `cargo build` is not the release build.
- CI (`rust.yml`, `release.yml`) is `workflow_dispatch`-only and runs on the self-hosted build server. The normal path is `frkn-services`'s `deploy/server-release.sh`, not GitHub Actions.
- Version lives in `Cargo.toml` and is bumped on the feature branch.

## Migrations & DB

- SQL lives in `deploy/migrations/`, applied in filename order by `deploy/migrate.sh`.
- Migrations run **at deploy time, never at api startup**: run `migrate.sh` *before* installing/restarting `api` — the new binary may read new columns unconditionally.
- Local DB: `docker compose -f dev/docker-compose.yml up -d` (postgres on 127.0.0.1:5432, data in `dev/data/`).

## Git

- Work on `feature/<name>`; never commit straight to `main`.
- No merge into `main` without the word "мержим" from the user. `--no-ff` only.
- Commit and push only on explicit request. Do not commit `target/`, local snapshots, or real configs with secrets.

## Deploy & safety

1. **No prod actions without explicit permission** — ssh to hosts, config changes, service restarts, migrations, binary uploads.
2. Release: tag pushed first, then from the `frkn-services` checkout
   `BUILD_HOST=root@us.frkn.org ./deploy/server-release.sh fcore <tag>`.
   fcore builds take ~45 min; if the local ssh times out, look at `/opt/build/out/<tag>/fcore/` on the build server and finish `gh release create` manually.
3. Install on a host from the published release: `sudo ./deploy/api-deploy.sh vX.Y.Z` (`auth-deploy.sh`, `node-deploy.sh` same shape).
4. Real `config.toml` files stay on the hosts; only `*-example.toml` is committed.

## Pitfalls

- Node metrics live **in memory**: restarting `api` resets the 24h/7d charts (they refill from scratch).
- One config = one server: each node has its own keys (AWG pubkey / REALITY), so "one card with failover" is only possible inside a pool.
- Share tokens: `share_tokens.node_id` is nullable — an env-wide token serves every online node of the env via `/sub/<token>`, and `/v1/config` for it returns one node's config plus `node_ips` of the pool.
- In `/v1/services` keep the naming format `Устройство (NL4)` / `Устройство (Finland)`; ` · ` prefix stays reserved for the protocol suffix.
- `panic = "abort"` and LTO in release profile — backtraces in release builds are useless, debug with a dev build.

## Style

- Code comments and commit messages are written in **English**; Russian comments in the code you touch get translated as part of that change (user-facing docs/HUMANS.md stay Russian).
- Minimal diffs, idiomatic Rust matching the surrounding module, no speculative abstraction.
- Public API changes: update `docs/API.md` and the endpoint list in the sibling repo that consumes it.
- If behaviour described here changes, update this file.

## Sibling repos

| Repo | Why it matters |
|---|---|
| `../frkn-services` | holds `deploy/server-release.sh` (build server) and the payment/marketing services |
| `../frkn.org` | site + cabinet consuming `/private/...`; subscription pages |
| `../dopamine` | client that imports `/sub/<token>` configs |
| `../env` | live server configs, node inventory, cross-repo index |
