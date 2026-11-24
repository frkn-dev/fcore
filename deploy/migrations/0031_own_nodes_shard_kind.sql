-- 0031: own nodes are a capability of any account.
--
-- Two schema changes, no data backfill:
--   1. `env_capacity.kind` learns the two account-owned shard kinds:
--      `selfhosted` — one shard per own-node account (`custompersonal<uuid>`),
--      with exactly one pool (`own`) inside it — and `resell` — a partner's
--      `custom<hex>` shard. The old spellings (`personal`, `premium`,
--      `legacy`) stay valid: rows already carry them.
--   2. `subscriptions.own_nodes_limit` gets default 3 for future rows, so a
--      newly created top-level account can host a node. Existing rows keep
--      their value (0 for most): the owner decided against a backfill, and
--      the api applies `[service.private] default_own_nodes_limit` on
--      creation anyway.
--
-- Idempotent like every migration: safe to run on every deploy.
BEGIN;

ALTER TABLE env_capacity DROP CONSTRAINT IF EXISTS env_capacity_kind_chk;
ALTER TABLE env_capacity
    ADD CONSTRAINT env_capacity_kind_chk
    CHECK (kind IN ('core', 'premium', 'personal', 'legacy', 'selfhosted', 'resell'));

-- Own-node shards: one per account, named after its subscription.
UPDATE env_capacity
   SET kind = 'selfhosted'
 WHERE kind <> 'selfhosted'
   AND (env LIKE 'custompersonal%' OR env LIKE 'personal%');

-- Reseller shards: `custom<hex>`, never a `custompersonal…` one.
UPDATE env_capacity
   SET kind = 'resell'
 WHERE kind <> 'resell'
   AND env LIKE 'custom%'
   AND env NOT LIKE 'custompersonal%'
   AND substring(env from 7) ~ '^[0-9a-f]{8,}$';

ALTER TABLE subscriptions ALTER COLUMN own_nodes_limit SET DEFAULT 3;

-- Reports separate the two account-owned kinds from the regions we run, the
-- same way 0022 did for `legacy` / `premium` / `personal`.
CREATE OR REPLACE VIEW shard_overview_selfhosted AS
    SELECT shard, title, plan, pools, nodes, peers_live, address_hosts
      FROM shard_overview
     WHERE kind = 'selfhosted';

CREATE OR REPLACE VIEW shard_overview_resell AS
    SELECT shard, title, plan, pools, nodes, peers_live, address_hosts
      FROM shard_overview
     WHERE kind = 'resell';

COMMIT;
