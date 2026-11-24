-- Pool-scoped address blocks: /16 per shard is the interface of its nodes,
-- each pool gets its own /20 inside it.
--
-- The old table kept one /16 per env, so every pool drew from the same range:
-- the US pool handed out .22 while it had six live peers, because eu/ru rows had
-- consumed the earlier slots. A pool block makes the number mean something and
-- lets a pool be reasoned about (and firewalled) on its own.
--
-- This migration REPLACES the env-wide block of shard 0001 with per-pool /20s.
-- Addresses issued from the old block are not carried over: `10.88.0.0/20`,
-- `10.88.16.0/20` and `10.88.32.0/20` cover .2-.47 only, and every peer of the
-- shard is recreated on connect (the wipe is an explicit operator step, not part
-- of this file).
BEGIN;

ALTER TABLE env_networks ADD COLUMN IF NOT EXISTS pool text;

-- The old exclusion rule judged overlaps across every env, so a /20 could never
-- sit inside a /16. Overlap must still be impossible, but only within the same
-- block space: (env, pool).
ALTER TABLE env_networks DROP CONSTRAINT IF EXISTS env_networks_no_overlap;

CREATE UNIQUE INDEX IF NOT EXISTS env_networks_env_pool_subnet
    ON env_networks (env, COALESCE(pool, ''), subnet);

-- One /16 per shard: the node interface address lives here.
DELETE FROM env_networks WHERE env = '0001' AND pool IS NULL;

-- /20 per pool: 4096 addresses, ~4093 peers.
INSERT INTO env_networks (env, pool, subnet) VALUES
    ('0001', 'eu', '10.88.0.0/20'),
    ('0001', 'ru', '10.88.16.0/20'),
    ('0001', 'us', '10.88.32.0/20')
ON CONFLICT DO NOTHING;

COMMIT;
