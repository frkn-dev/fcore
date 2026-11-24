# Private user nodes

Self-serve nodes owned by a subscription. Isolated from FRKN shared envs
(`dev`/`ru`/`wl`/`gaming`/`bs`/`bs-prem`/…). Auth for register: one-time install token (B).

## Selfhosted account (`plan_kind = selfhosted`)

An empty account anyone can open without an activation key:
`POST /private/account` (public, per-IP rate-limited, currently 5 per 10 min)
returns a fresh subscription with `plan_kind = selfhosted`,
`expires_at IS NULL`, a personal scope env `custompersonal{uuid_simple}` and
zero connections. The cabinet then opens it at `/subscription/?id=<uuid>`
(`/en/subscription/?id=<uuid>`) and the user installs a node via the
install-token flow below.

- `expires_at IS NULL` is the normal state — the account lives while it exists.
  `PlanKind::Selfhosted` is always active, never time-expired and never
  top-up-able.
- New accounts start with `server_limit = 3` and `user_limit = 20`; both can be
  raised or lowered per account from the admin panel (see «Limits» below).
- `reconcile_enabled_connections` skips selfhosted subs, so shared
  `enabled_conns` are never seeded into the personal env.
- mrkting `can_create_device` accepts `plan_kind = selfhosted` with a NULL
  expiry (named devices for the user's own nodes).
- The install-token path is unchanged and does not overwrite the personal
  scope already set at creation.

## Flow

1. Cabinet: `POST /private/install-token` `{ "subscription_id": "<uuid>" }`
   - Ensures personal scope env `custompersonal{uuid_simple}` on the sub when
     `scope_env` is empty (does not overwrite an existing premium scope).
   - Allocates the shard's address block on first mint — `<next free /16>` out of
     `[service.peers].auto_subnet_pool` into `env_networks` (an env that already
     holds one keeps it) — and returns it as `env_block` (`10.89.0.0/16`).
   - Returns `inst_…` token (TTL 1h, max 3 active unused per sub), `scope_env`,
     `env_block` and `node_version` (the node build the api ships with).
   - `GET /private/install-token/<token>` answers the same `scope_env`,
     `env_block`, `node_version` without burning the token: the installer needs
     them *before* it writes the first interface, and a re-run must heal rather
     than fail. Unknown/expired/used → `401`.
2. Install:
   `curl -fsSL https://frkn.org/install | bash -s -- --token inst_…`
   The installer writes `<env_block>.1/16` on `awg0-peer` (UDP 51821 — 443 stays
   hysteria2's), NATs exactly `<env_block>` out of the default-route interface
   (PostUp/PostDown of the enabled `awg-quick@awg0-peer`, uplink read at call
   time), and puts the full `scope_env` into `[node]` — the bus topic is derived
   from it, and a wrong env means the node never hears the api.
3. Node: `POST /private/nodes` with `Authorization: Bearer inst_…`
   - API forces personal env, burns install token, returns durable `node_…`
   - fnode persists it to `api.token` and uses it for `POST /connections/sync`
   - Registration also (a) grants the subscription the protocols the node
     announces (`amnezia_peer`, `hysteria2`) so `/v1/locations` has a card,
     (b) checks the peer interface sits in the shard block (adopting the block
     the node brings when the shard has none) and (c) publishes the
     authoritative `peers-init` for `(env, pool)` — on every registration,
     including the `200` refresh path, so a node that lost its snapshot heals on
     `systemctl restart fnode` (or `systemctl reload fnode`, SIGHUP: re-register;
     `fnode config.toml --peers-resync` ignores the local snapshot once).
4. Cabinet: `GET /private/nodes?subscription_id=…`,
   `DELETE /private/nodes/{uuid}?subscription_id=…`,
   `GET /private/state?subscription_id=…` (used/limit/remaining, see «Limits»).

## Peer isolation of an own shard

An own shard is one account, and the bus is treated as public: topic strings are
derived from what the installer prints (`peers-update-custompersonal<uuid>-own`),
so anybody who knows the account uuid can subscribe to them.

- Peers of a personal env are **never** published on the shard-wide stream.
  `publish_peer` answers a change there by pushing the authoritative
  `peers-init-<node_uuid>` (unicast, the uuid is a v4 random capability) to every
  node of that env; the interface is then reconciled as a diff.
- The peer set is chosen from the env stored for the node (token-derived at
  registration, env from the row on refresh) and `peers_for_node` filters on it
  again — a request can never widen it.
- Subscription-wide streams of shared shards (`peers-update-0001-eu`) are
  untouched: those are shards we run.
- Tests: `an_own_shard_sees_only_its_own_subscription` (a mixed slice yields only
  this account's rows), plus the pool scoping tests.

## Limits (servers / users)

Two caps live on the subscription owning the account
(`subscriptions.server_limit`, `subscriptions.user_limit`; PG `INTEGER NOT
NULL DEFAULT 3 / 20`, added by `ensure_schema` at api start):

- **servers** — how many own nodes the account may hold. The cap applies to
  *additional* nodes: `SELECT count(*) … WHERE env = $1 AND uuid <> $2`, so
  re-registering a known node never consumes a second slot. Offline nodes
  count (there is no soft-delete for nodes — a deleted node frees its slot).
- **users** — how many child subscriptions (`parent_id = owner`, not
  `is_deleted`) it may hold. `GET /private/state` counts them and
  `POST /private/users` enforces the cap (see «Users (multi-server)»).
- Enforcement is **transactional**: the account row that owns the personal
  `scope_env` is locked (`SELECT … FOR UPDATE`), the current usage counted and
  the node/subscription inserted in the same transaction — two concurrent
  registrations can't both take the last free slot
  (`PgNode::insert_private_node_with_limit`,
  `PgSubscription::insert_child_within_user_limit`).
- Rejection is `409` with a machine-readable code and the counters:
  `{"status":409,"message":"server limit reached (3/3)",
    "response":{"code":"server_limit_reached","used":3,"limit":3,"remaining":0}}`
  (also `user_limit_reached`). `POST /private/install-token` pre-checks the
  server cap and returns the same 409 instead of minting a command that could
  never be installed.
- `GET /private/state?subscription_id=…` (public, like the rest of the
  cabinet API — the UUID is the credential) returns
  `{subscription_id, plan_kind, servers:{used,limit,remaining},
    users:{used,limit,remaining}}`; the cabinet renders it as
  «Серверы: N из M · свободно K».
- Admin: `PUT /admin/api/subscriptions/{owner_uuid}/selfhosted-limits` with
  `{"server_limit":N,"user_limit":M}` (bearer admin token; requires
  `admin_enabled`) rewrites the caps in PG and in the in-memory cache.
  Raising a cap only widens the account; **lowering deletes nothing** — the
  account keeps every node and child it already has, it just can't grow past
  the new value (`used` may exceed `limit`, `remaining` clamps at 0).

## Users (multi-server)

A selfhosted account can hand its nodes to other people as **users**. A user
is a *child subscription*: `parent_id = <owner>`,
`plan_kind = selfhosted`, `expires_at IS NULL`, caps `0/0`, its **own**
connections inside the owner's personal env (own AWG keys and address, so
traffic is per-user and a revoke cuts only that user). The owner shares the
link `frkn://sub/<child_uuid>` with the user.

`POST /private/users`
`{"subscription_id":"<owner>","all_servers":false,"node_ids":["<uuid>",…]}`

- **Selection**: `all_servers` XOR a non-empty `node_ids` without duplicates —
  both modes at once, an empty subset and duplicates are `400`.
- **Nodes**: only nodes of the owner's personal env. An unknown uuid is `404`
  `node not found`, a node of another env `403`
  `node belongs to another account`. A node without an AWG inbound is `400`.
- **Protos**: AWG family only — `AmneziaWg`, plus `AmneziaWgMobile` when the
  api has `amnezia_wireguard_mobile_network` configured. Hysteria2 is out of
  scope: per-node isolation is not reliable there yet.
- `all_servers = false` → one connection per (node, proto) **pinned** to that
  node; `all_servers = true` → **unpinned** connections, which every node of
  the env serves, including nodes registered later.
- A user with no mintable server (empty account, or no AWG inbound) gets
  `400` `the account has no amnezia-wg servers yet`.
- The slot is taken **transactionally** under `user_limit` (lock owner →
  count live children → insert child), and a full cap answers
  `409 user_limit_reached` with the counters.
- If minting a connection fails half-way, everything already created is rolled
  back (each connection is deleted with a tombstone to its node, the child row
  is dropped) — the owner never sees a half-built user.
- Response:
  `{subscription_id, subscription_url:"frkn://sub/<child>", all_servers,
    node_ids:[…], connections:[{id,proto,node_id}], users:{used,limit,remaining}}`.

`GET /private/users?subscription_id=<owner>` → every live user with its link,
`connections` count and the current selection (`all_servers`, `node_ids`),
read back from the connection pins.

`DELETE /private/users/{child_uuid}?subscription_id=<owner>` → revokes one
user: cuts every connection of the child (each delete publishes a tombstone,
so its AWG peers disappear from the node) and marks the child deleted, which
frees the slot. A foreign uuid, a premium child or an already deleted one is a
uniform `404 user not found`.

Nesting is refused: a user cannot invite users (`400`), and
`POST /private/install-token` answers `403` for a child, so a user never gets
a personal env or nodes of its own.

Serving: the user's link is consumed by the app through
`/v1/services` + `/v1/config` for the child uuid; those endpoints already
resolve node-pinned and unpinned connections through `pinned_to`, so users
need **no** `private_node_feed` flag. The legacy general `GET /sub` feed still
shows personal nodes only when the owner opted the node into the main feed
(unchanged behaviour).

## Isolation

- Personal env never accepted on mgmt `POST /node` (service/admin token).
- Personal nodes hidden from public `GET /nodes`, `GET /status`, `GET /node/:id`.
- Private register rejects any non-personal / shared env.
- Max `server_limit` private nodes per subscription (default 3; per-account
  column, see «Limits» above).
- A user's connections live in the owner's personal env as pins (or unpinned);
  a user never gets shared envs, its own nodes (install-token `403`) or a
  premium child's scope.
- Only `/connections/sync` accepts `node_…` in addition to the service token.

## Not in this slice

- cabinet UI “Add node” button / “Invite user” form
- `include_in_main_feed`
- trust weights / health scoring
- opt-in share node into FRKN pool
- Arch install path
- per-user traffic quotas (a user's traffic is accounted, but not capped
  separately from the account)
- Hysteria2 for users (per-node isolation on the node side is not reliable yet)
