-- 'degraded': the node registered and keeps serving clients, but its inbounds
-- differ from the rest of its pool (see pool_inbounds_conflict in api).
-- Registration used to answer a bare 409 and drop the node, which left pools on
-- a stale snapshot: peers stopped arriving and fresh client configs did not work.
--
-- ALTER TYPE ... ADD VALUE must run outside a transaction block, so apply this
-- file with plain psql (no -1) and before restarting the api binary.
ALTER TYPE node_status ADD VALUE IF NOT EXISTS 'degraded';
