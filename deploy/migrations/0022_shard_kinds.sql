-- 0022: legacy shards and the kind-specific overviews.
--
-- Every env that already has machines becomes a `legacy` shard: it serves v1
-- traffic exactly as before, it is never picked by the peers assignment, and its
-- row is what lets the registration check be strict ("a node must register in a
-- declared shard") without killing the machines that predate shards.

-- 1. The kind vocabulary grows by `legacy`.
ALTER TABLE env_capacity DROP CONSTRAINT IF EXISTS env_capacity_kind_chk;
ALTER TABLE env_capacity
    ADD CONSTRAINT env_capacity_kind_chk
    CHECK (kind IN ('core', 'premium', 'personal', 'legacy'));

-- 2. Backfill a row for every env a node currently reports and that has no policy
--    yet. `plan = 'legacy'` matches no real plan, so nothing is assigned here.
INSERT INTO env_capacity (env, plan, max_subscriptions, enabled, title, kind)
SELECT DISTINCT n.env, 'legacy', NULL, true, NULL, 'legacy'
  FROM nodes n
 WHERE NOT EXISTS (SELECT 1 FROM env_capacity c WHERE c.env = n.env);

-- 3. Same for envs that only carry address blocks (a shard declared before its
--    machines arrived): a block without a policy row is a half-declared shard.
INSERT INTO env_capacity (env, plan, max_subscriptions, enabled, title, kind)
SELECT DISTINCT w.env, 'legacy', NULL, true, NULL, 'legacy'
  FROM env_networks w
 WHERE NOT EXISTS (SELECT 1 FROM env_capacity c WHERE c.env = w.env);

-- 4. Kind-specific overviews: same shape as `shard_overview`, one per kind.
--    Whoever manages premium slots or customers' machines does not have to filter
--    the mixed list by hand.
CREATE OR REPLACE VIEW shard_overview_legacy AS
    SELECT shard, title, plan, pools, nodes, peers_live, address_hosts
      FROM shard_overview
     WHERE kind = 'legacy';

CREATE OR REPLACE VIEW shard_overview_premium AS
    SELECT shard, title, plan, pools, nodes, peers_live, address_hosts
      FROM shard_overview
     WHERE kind = 'premium';

CREATE OR REPLACE VIEW shard_overview_personal AS
    SELECT shard, title, plan, pools, nodes, peers_live, address_hosts
      FROM shard_overview
     WHERE kind = 'personal';
