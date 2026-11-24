-- Explicit migration plane of a subscription.
--
-- v1: the subscription lives on per-protocol `connections` rows (the old plane).
-- v2: it holds shard `peers` (the peer plane) and no live v1 connections.
-- dual: it holds both — a client that imported a v1 config and then created a
--       peer, or one that keeps a v1 connection next to its peer.
--
-- The marker exists so migration progress is counted instead of guessed: the
-- first live peer moves a v1 subscription to v2 (or to dual when it still has
-- a live connection), and `v2_since` keeps the moment it happened.
--
-- Idempotent like every migration: `ADD COLUMN IF NOT EXISTS`, and both
-- backfill UPDATEs preserve an already recorded `v2_since`.
BEGIN;

ALTER TABLE subscriptions
  ADD COLUMN IF NOT EXISTS plane text NOT NULL DEFAULT 'v1',
  ADD COLUMN IF NOT EXISTS v2_since timestamptz;

-- Both planes in use. Runs before the v1 -> v2 pass and is not limited to
-- plane='v1': a subscription that got a peer earlier and a v1 connection
-- afterwards is dual too.
UPDATE subscriptions s
   SET plane = 'dual',
       v2_since = COALESCE(s.v2_since, now())
 WHERE EXISTS (SELECT 1
                 FROM peers p
                WHERE p.subscription_id = s.id AND p.revoked_at IS NULL)
   AND EXISTS (SELECT 1
                 FROM connections c
                WHERE c.subscription_id = s.id AND c.is_deleted = false);

-- Peer plane only. `plane = 'v1'` keeps this pass from downgrading a row the
-- previous statement already marked dual.
UPDATE subscriptions s
   SET plane = 'v2',
       v2_since = COALESCE(s.v2_since, now())
 WHERE s.plane = 'v1'
   AND EXISTS (SELECT 1
                 FROM peers p
                WHERE p.subscription_id = s.id AND p.revoked_at IS NULL);

COMMIT;
