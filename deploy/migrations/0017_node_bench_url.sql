-- nodes.bench_url: base URL of the node's own speed-test endpoint.
--
-- Consumers (status page, Dopamine) must not derive the bench host from the node
-- hostname: bench names live in the beacon zone as
-- <hoster>-<cc>-<city>-bench-<nn>.bench.beacon.frkn.org, and neither provider nor
-- geo can be derived from `eu01`/`at`/`msk-tw`. The api returns this value
-- verbatim as `speedtest_url` (null = node has no bench).
--
-- The column is deliberately NOT written by node registration: fnode does not
-- know the name, so the upsert leaves whatever is stored here untouched.
-- Applied at deploy time by deploy/migrate.sh, must stay idempotent.

ALTER TABLE nodes ADD COLUMN IF NOT EXISTS bench_url TEXT;
