-- One tunnel per (shard, pool, key), not per shard.
--
-- `POST /v1/peers/prepare` provisions a tunnel in every pool of a grant with
-- ONE device key, so the same key must be allowed to be live in several pools
-- of an env at once (the address cap of the shard still bounds the total).
-- `peers_live_key` made that impossible: it was unique on (env, public_key).
--
-- Idempotency is now per (env, pool, public_key): a retry of the same pool
-- still finds its own tunnel, while another pool gets its own row. Rows written
-- before 0025 keep `pool = NULL` and are treated as their own group
-- (`COALESCE(pool, '')`), so no existing row is affected.
--
-- Idempotent like every migration: safe to run on every deploy.
BEGIN;

DROP INDEX IF EXISTS peers_live_key;

CREATE UNIQUE INDEX IF NOT EXISTS peers_live_pool_key
    ON peers (env, COALESCE(pool, ''), public_key) WHERE revoked_at IS NULL;

COMMIT;
