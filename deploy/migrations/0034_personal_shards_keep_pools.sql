-- Personal shards keep several pools (0.7.47 shipped the opposite).
--
-- 0.7.47 wrote a single `own` pool on every node of an account's own shard, so
-- the catalog collapsed all of its machines into one card (a pool is one card)
-- and every node's /v1/config carried the addresses of the whole shard. The
-- registration paths no longer invent that name: a node keeps the pool it
-- declares, and a node without one keeps a card of its own.
--
-- The stamp survives on rows written by that build, and re-registering a node
-- keeps the stored value, so it has to be cleared once. Only the synthetic name
-- goes: a pool an account actually chose is left alone.
--
-- Idempotent: after the first run nothing matches.
BEGIN;

UPDATE nodes
SET pool = NULL,
    modified_at = now()
WHERE pool = 'own'
  AND env LIKE 'custompersonal%';

COMMIT;
