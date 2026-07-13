-- Run this against the mrkting database.
-- The mrkting service stores emails/referrals/trials separately from the API DB.
-- subscription_id is intentionally a plain UUID: the actual subscription lives in the API DB.

CREATE SCHEMA IF NOT EXISTS mrkting;

CREATE TABLE IF NOT EXISTS mrkting.emails (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID,
    email TEXT,
    email_hmac TEXT,
    trial BOOLEAN NOT NULL DEFAULT false,
    referred_by VARCHAR(13),
    ref_code VARCHAR(13),
    created_at TIMESTAMPTZ DEFAULT now(),
    expires_at TIMESTAMPTZ,
    converted_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_emails_subscription_id ON mrkting.emails(subscription_id);
CREATE INDEX IF NOT EXISTS idx_emails_hmac ON mrkting.emails(email_hmac);
CREATE INDEX IF NOT EXISTS idx_emails_trial_created ON mrkting.emails(trial, created_at);
CREATE INDEX IF NOT EXISTS idx_emails_referred_by ON mrkting.emails(referred_by);
CREATE INDEX IF NOT EXISTS idx_emails_ref_code ON mrkting.emails(ref_code);
