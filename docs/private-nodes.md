# Private user nodes

Self-serve nodes owned by a subscription. Isolated from FRKN shared envs
(`dev`/`ru`/`wl`/…). Auth for register: one-time install token (B).

## Flow

1. Cabinet: `POST /private/install-token` `{ "subscription_id": "<uuid>" }`
   - Ensures personal scope env `custompersonal{uuid_simple}` on the sub when
     `scope_env` is empty (does not overwrite an existing premium scope).
   - Returns `inst_…` token (TTL 1h, max 3 active unused per sub).
2. Install (beta for now):
   `curl -fsSL https://beta.frkn.org/install | bash -s -- --token inst_…`
3. Node: `POST /private/nodes` with `Authorization: Bearer inst_…`
   - API forces personal env, burns install token, returns durable `node_…`
   - fnode persists it to `api.token` and uses it for `POST /connections/sync`
4. Cabinet: `GET /private/nodes?subscription_id=…`,
   `DELETE /private/nodes/{uuid}?subscription_id=…`.

## Isolation

- Personal env never accepted on mgmt `POST /node` (service/admin token).
- Personal nodes hidden from public `GET /nodes`, `GET /status`, `GET /node/:id`.
- Private register rejects any non-personal / shared env.
- Max 3 private nodes per subscription.
- Only `/connections/sync` accepts `node_…` in addition to the service token.

## Not in this slice

- cabinet UI “Add node” button
- `include_in_main_feed`
- trust weights / health scoring
- opt-in share node into FRKN pool
- Arch install path
