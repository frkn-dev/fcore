-- Shards are opaque ids (`0001`) so a name never has to lie about the plan or
-- the locations inside it. Everything human lives next to the id:
--   title  — what the operator calls it ("Standard NL/DE"),
--   plan   — which plan may be assigned here (added in 0020),
--   pools  — the cards clients see, registered by the nodes themselves.
-- Additive: no table is rewritten, no row is touched.

ALTER TABLE env_capacity ADD COLUMN IF NOT EXISTS title text;

-- What a shard is for. Declared here, never derived from the name: a shard
-- called `premium-eu` must not become a premium slot by accident.
ALTER TABLE env_capacity ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'core';

UPDATE env_capacity SET kind = 'premium'
  WHERE kind = 'core' AND (env LIKE 'premium%' OR env LIKE 'custompremium%');
UPDATE env_capacity SET kind = 'personal'
  WHERE kind = 'core' AND (env LIKE 'personal%' OR env LIKE 'custompersonal%');

ALTER TABLE env_capacity DROP CONSTRAINT IF EXISTS env_capacity_kind_chk;
ALTER TABLE env_capacity ADD CONSTRAINT env_capacity_kind_chk
  CHECK (kind IN ('core', 'premium', 'personal'));

-- One row per shard with everything an operator needs to answer
-- "what is 0001?": plan, human title, pools, nodes, live peers and the address
-- budget (hosts = 2^(32-cidr) - 3 for a normal block: network, broadcast and
-- the node's own .1).
CREATE OR REPLACE VIEW shard_overview AS
SELECT c.env                                                              AS shard,
       c.title,
       c.plan,
       c.kind,
       c.enabled,
       COALESCE((SELECT array_agg(p.pool ORDER BY p.pool)
                   FROM pools p WHERE p.env = c.env), '{}')                AS pools,
       COALESCE((SELECT sum(p.node_count)
                   FROM pools p WHERE p.env = c.env), 0)::bigint           AS nodes,
       (SELECT count(*) FROM peers pe
          WHERE pe.env = c.env AND pe.revoked_at IS NULL)                  AS peers_live,
       COALESCE((SELECT sum(power(2::numeric, 32 - masklen(w.subnet)) - 3)
                   FROM env_networks w WHERE w.env = c.env), 0)::bigint    AS address_hosts
FROM env_capacity c;

-- Legacy rows written before the env was normalised: `Premium1e3e…`,
-- `premium26a2…`. Their canonical form is `custompremium…` (that is what
-- `Env::Custom` prints), so a hand-written mixed-case value would change
-- identity on the next save and lose its nodes/connections. Normalise once,
-- skipping target names that already exist (a collision would abort the file).
UPDATE connections c SET env = 'custom' || lower(c.env)
 WHERE lower(c.env) LIKE 'premium%' AND c.env NOT LIKE 'custom%'
   AND NOT EXISTS (SELECT 1 FROM connections d
                    WHERE d.env = 'custom' || lower(c.env) AND d.id <> c.id);
UPDATE connections c SET env = 'custom' || lower(c.env)
 WHERE lower(c.env) LIKE 'personal%' AND c.env NOT LIKE 'custom%'
   AND NOT EXISTS (SELECT 1 FROM connections d
                    WHERE d.env = 'custom' || lower(c.env) AND d.id <> c.id);

UPDATE nodes n SET env = 'custom' || lower(n.env)
 WHERE lower(n.env) LIKE 'premium%' AND n.env NOT LIKE 'custom%'
   AND NOT EXISTS (SELECT 1 FROM nodes m
                    WHERE m.env = 'custom' || lower(n.env) AND m.uuid <> n.uuid);
UPDATE nodes n SET env = 'custom' || lower(n.env)
 WHERE lower(n.env) LIKE 'personal%' AND n.env NOT LIKE 'custom%'
   AND NOT EXISTS (SELECT 1 FROM nodes m
                    WHERE m.env = 'custom' || lower(n.env) AND m.uuid <> n.uuid);
