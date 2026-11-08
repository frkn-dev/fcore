-- Alert routing for self-hosted (personal) nodes.
--
-- A node on a customer's own machine lives in env `custompersonal<sub_id>`.
-- When that owner binds a Telegram chat here, alerts about their servers go to
-- them instead of the global chat (which only logs them, see
-- [alerts] quiet_env_prefixes). The api creates this table itself on startup
-- (PgAlerts::ensure_table); this file is here for a manual, explicit apply.
--
-- Idempotent: safe to run twice.

CREATE TABLE IF NOT EXISTS subscription_alert_chats (
    subscription_id   uuid PRIMARY KEY,
    telegram_chat_id  text NOT NULL,
    enabled           boolean NOT NULL DEFAULT true,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
