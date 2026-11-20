-- Which card a tunnel was created for.
--
-- `card_id` = `<shard>:<pool>:<proto>` is an opaque handle the app echoes back
-- to `POST /v1/peers`. The address comes from the shard's block, but the
-- endpoint, the server public key, the MTU, the DNS and the obfuscation come
-- from the node that carries the pool — so a config rebuilt from
-- `GET /v1/peers` (without talking to the api again) can only be assembled if
-- the pool of the tunnel is known. Until now it was dropped on create.
ALTER TABLE peers ADD COLUMN IF NOT EXISTS pool text;

-- Backfill only what is unambiguous: a shard whose live nodes all declare the
-- same pool (or declare none at all, i.e. the "default" card) has one card per
-- proto, so the pool of an old tunnel can be derived. A shard with several
-- pools keeps NULL: the client falls back to `env` + `proto` for those rows.
UPDATE peers p
   SET pool = q.pool
  FROM (
        SELECT env, min(COALESCE(pool, 'default')) AS pool
          FROM nodes
         GROUP BY env
        HAVING count(DISTINCT COALESCE(pool, 'default')) = 1
       ) q
 WHERE p.env = q.env
   AND p.pool IS NULL;

CREATE INDEX IF NOT EXISTS peers_pool_live
    ON peers (env, pool) WHERE revoked_at IS NULL;
