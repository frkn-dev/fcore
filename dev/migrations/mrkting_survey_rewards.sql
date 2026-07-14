-- Survey rewards tracking for marketing campaigns.

CREATE TABLE IF NOT EXISTS mrkting.survey_rewards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL,
    email_hmac TEXT NOT NULL,
    campaign TEXT NOT NULL,
    answers JSONB,
    key_id UUID,
    rewarded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(email_hmac, campaign)
);

CREATE INDEX IF NOT EXISTS idx_survey_rewards_hmac ON mrkting.survey_rewards(email_hmac);
CREATE INDEX IF NOT EXISTS idx_survey_rewards_campaign ON mrkting.survey_rewards(campaign);
