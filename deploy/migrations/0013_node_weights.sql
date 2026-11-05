-- Per-address weights for pool nodes (config.toml `[node] node_weights`).
--
-- The api also adds this column at startup (`ensure_pool_column`), so the
-- migration is for fresh databases and for documenting the shape. Older api
-- binaries ignore the column, so it can be applied before or after a release.
--
-- Shape: {"93.89.220.48": 75, "77.221.152.88": 25}
-- Missing address = default weight 100. The legacy `nodes.weight` column is
-- deleted in 0014, AFTER the api that no longer reads it is deployed.
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS node_weights JSONB;
