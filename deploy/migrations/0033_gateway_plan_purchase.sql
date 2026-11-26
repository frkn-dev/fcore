-- The payment gateway fulfils cabinet plan purchases (`POST
-- /subscription/{id}/plan`) as its own transaction kind, so a bought plan is
-- never mistaken for a day-based renewal in analytics.
--
-- The `transaction_type` enum lives in the **payment** database, not in `api`:
-- `deploy/migrate.sh` runs every file on every deploy against whichever
-- database it is pointed at, and `ALTER TYPE` on a type that is absent aborts
-- the whole run (ON_ERROR_STOP), taking the later migrations with it. So the
-- statement is guarded: outside the payment database this file is a no-op.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_type t
          JOIN pg_namespace n ON n.oid = t.typnamespace
         WHERE t.typname = 'transaction_type'
    ) THEN
        ALTER TYPE transaction_type ADD VALUE IF NOT EXISTS 'PLAN_PURCHASE';
    END IF;
END
$$;
