-- 0023: drop tables that nothing reads (PLANNED — apply by hand).
--
-- Run the checks at the bottom first on the target database. Everything here is
-- additive-free removal: no Rust code and no shell script in this repo references
-- these objects, and the Grafana-facing views are already looking at an empty
-- table (`node_metrics` has no writer — metrics live in the rkyv snapshot file
-- under `metrics.snapshot_path`).
--
-- NOT part of the automatic migration chain: apply only after `0022` is in place
-- and only once someone has confirmed that no external dashboard/export reads
-- `grafana_metrics`.

BEGIN;

-- 1. Views over the empty table go first.
DROP VIEW IF EXISTS grafana_metrics;
DROP VIEW IF EXISTS node_metrics_1m;

-- 2. Per-node metric history. Written by nothing; metrics are in the snapshot file.
DROP TABLE IF EXISTS node_metrics;

-- 3. Digest bookkeeping. Digests moved to mrkting in 0.6.139; api has neither
--    the scheduler nor an insert path left. The 4 remaining rows are historical.
DROP TABLE IF EXISTS digest_runs;

COMMIT;

-- ---------------------------------------------------------------------------
-- Deliberately NOT dropped here — needs a decision, see docs/db-cleanup.md:
--
--   pools            empty (no node declares `pool` any more); `shard_overview`
--                    still selects from it. Either put `pool` back into node
--                    configs or drop the table and the `pools` column of the view.
--   client_reports   write path exists (`postgres/report.rs:516`), zero rows:
--                    the app does not send reports. Keep until the client path
--                    is confirmed dead.
--   connection_attempts  same: writer exists, zero rows.
--   block_observations   written by a service outside this repo (api only
--                    purges it); keep.
--   iap_transactions / activation_failures / subscription_limits /
--   subscription_alert_chats  legitimately empty — features not launched or
--                    no overrides set yet. Keep.
-- ---------------------------------------------------------------------------
--
-- Check before applying:
--   SELECT count(*) FROM node_metrics;
--   SELECT count(*) FROM digest_runs;
--   SELECT * FROM pg_depend WHERE refobjid = 'node_metrics'::regclass;
