-- Run this against existing mrkting databases to add conversion tracking.

ALTER TABLE mrkting.emails
    ADD COLUMN IF NOT EXISTS converted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_emails_converted_at ON mrkting.emails(converted_at);
