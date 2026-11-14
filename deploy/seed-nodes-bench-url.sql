-- nodes.bench_url seed for the three live pools.
--
-- Bench hosts live in the beacon zone as
-- <hoster>-<cc>-<city>-bench-<nn>.bench.beacon.frkn.org (see
-- devops/frkn-dns-zones); they cannot be derived from the node hostname, so the
-- values are listed here. Idempotent: safe to re-run, and rows that do not exist
-- in this environment are simply not touched.
--
-- Run once per environment after 0017_node_bench_url.sql:
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/seed-nodes-bench-url.sql

UPDATE nodes SET bench_url = 'https://mis-at-vie-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'at.frkn.org' AND bench_url IS DISTINCT FROM 'https://mis-at-vie-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://soul-ee-tll-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'ee.frkn.org' AND bench_url IS DISTINCT FROM 'https://soul-ee-tll-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'eu01.frkn.org' AND bench_url IS DISTINCT FROM 'https://ncr-nl-ams-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://aeza-nl-ams-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'eu02.frkn.org' AND bench_url IS DISTINCT FROM 'https://aeza-nl-ams-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://aeza-fr-par-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'fr.frkn.org' AND bench_url IS DISTINCT FROM 'https://aeza-fr-par-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://fbw-fr-par-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'pl.frkn.org' AND bench_url IS DISTINCT FROM 'https://fbw-fr-par-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://hk-tr-ist-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'tr.frkn.org' AND bench_url IS DISTINCT FROM 'https://hk-tr-ist-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://tw-ru-msk-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'msk-tw.frkn.org' AND bench_url IS DISTINCT FROM 'https://tw-ru-msk-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://adm-ru-nsk-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'msk-tw-nvsb.frkn.org' AND bench_url IS DISTINCT FROM 'https://adm-ru-nsk-bench-01.bench.beacon.frkn.org:19443';
UPDATE nodes SET bench_url = 'https://vdska-ru-msk-bench-01.bench.beacon.frkn.org:19443'
 WHERE hostname = 'msk-vdska.frkn.org' AND bench_url IS DISTINCT FROM 'https://vdska-ru-msk-bench-01.bench.beacon.frkn.org:19443';
