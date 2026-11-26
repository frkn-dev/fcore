-- The payment gateway fulfils cabinet plan purchases (`POST
-- /subscription/{id}/plan`) as its own transaction kind, so a bought plan is
-- never mistaken for a day-based renewal in analytics.
-- The `transaction_type` enum lives in the gateway's database, not here.
ALTER TYPE transaction_type ADD VALUE IF NOT EXISTS 'PLAN_PURCHASE';
