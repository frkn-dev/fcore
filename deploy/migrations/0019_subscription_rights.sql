-- Subscription rights as flags instead of a plan kind (docs: ICEBREAKER_TZ.md
-- neighbours; design note in HUMANS). Three orthogonal rights:
--   shared_access   — access to FRKN shared configs (products);
--   own_nodes_limit — how many own nodes may be registered;
--   resell_enabled  — partner may create child users on its own nodes.
--
-- Backfill keeps today's behaviour: only `selfhosted` accounts had no shared
-- configs and owned their nodes.

ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS shared_access boolean;
ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS own_nodes_limit integer;
ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS resell_enabled boolean;

UPDATE subscriptions
   SET shared_access = (plan_kind <> 'selfhosted')
 WHERE shared_access IS NULL;

UPDATE subscriptions
   SET own_nodes_limit = CASE WHEN plan_kind = 'selfhosted' THEN COALESCE(server_limit, 3) ELSE 0 END
 WHERE own_nodes_limit IS NULL;

UPDATE subscriptions
   SET resell_enabled = (plan_kind = 'selfhosted')
 WHERE resell_enabled IS NULL;

ALTER TABLE subscriptions ALTER COLUMN shared_access SET DEFAULT true;
ALTER TABLE subscriptions ALTER COLUMN own_nodes_limit SET DEFAULT 0;
ALTER TABLE subscriptions ALTER COLUMN resell_enabled SET DEFAULT false;

ALTER TABLE subscriptions ALTER COLUMN shared_access SET NOT NULL;
ALTER TABLE subscriptions ALTER COLUMN own_nodes_limit SET NOT NULL;
ALTER TABLE subscriptions ALTER COLUMN resell_enabled SET NOT NULL;
