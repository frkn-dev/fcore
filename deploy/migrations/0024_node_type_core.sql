-- 0024: node_type gets `core` and `selfhosted` next to the legacy `common`.
--
-- `common` stays in the enum on purpose: old fnode builds keep sending it and
-- the api still has to read those rows. New machines register as `core`, a
-- user's own machine as `selfhosted`.
--
-- ALTER TYPE ... ADD VALUE cannot be used in the same transaction that uses the
-- new label, so this file deliberately has no BEGIN/COMMIT.
--
-- Data backfill (`UPDATE nodes SET node_type='core' WHERE node_type='common'`)
-- is a separate step and must happen only after every api instance understands
-- `core` — otherwise a rollback would fail to deserialize the rows.

ALTER TYPE node_type ADD VALUE IF NOT EXISTS 'core';
ALTER TYPE node_type ADD VALUE IF NOT EXISTS 'selfhosted';
