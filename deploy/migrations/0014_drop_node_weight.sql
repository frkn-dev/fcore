-- Contract step for 0013_node_weights.sql.
--
-- DO NOT run this before the api build that no longer reads `nodes.weight`
-- is deployed and healthy: an older binary names the column in its SELECT and
-- INSERT and would start answering 500 on every node registration.
--
-- Order: api with 0.6.117+ (node_weights) -> healthcheck shows the version ->
-- this file -> re-check node registration (fnode re-registers every 10-70s).
ALTER TABLE nodes DROP COLUMN IF EXISTS weight;
