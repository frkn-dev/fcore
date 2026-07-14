-- Survey campaigns and pre-generated keys for rewards.

CREATE TABLE IF NOT EXISTS mrkting.survey_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    token TEXT NOT NULL UNIQUE,
    distributor TEXT NOT NULL,
    key_days INT NOT NULL,
    campaign_days INT NOT NULL,
    limit_bytes BIGINT,
    subject TEXT,
    starts_at TIMESTAMPTZ NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS mrkting.survey_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL REFERENCES mrkting.survey_campaigns(id) ON DELETE CASCADE,
    key_id UUID NOT NULL,
    code TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'available',
    email_hmac TEXT,
    issued_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(campaign_id, code)
);

CREATE INDEX IF NOT EXISTS idx_survey_keys_campaign_status ON mrkting.survey_keys(campaign_id, status);
CREATE INDEX IF NOT EXISTS idx_survey_keys_issued ON mrkting.survey_keys(email_hmac);

ALTER TABLE mrkting.survey_rewards
    ADD COLUMN IF NOT EXISTS campaign_id UUID REFERENCES mrkting.survey_campaigns(id) ON DELETE SET NULL;

ALTER TABLE mrkting.survey_rewards
    DROP CONSTRAINT IF EXISTS survey_rewards_email_hmac_campaign_key;

ALTER TABLE mrkting.survey_rewards
    ADD CONSTRAINT survey_rewards_email_campaign_unique UNIQUE (email_hmac, campaign_id);
