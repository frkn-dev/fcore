-- Dashboard partner schema.

CREATE SCHEMA IF NOT EXISTS dashboard;

CREATE TABLE IF NOT EXISTS dashboard.partners (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    share_percent NUMERIC(5,2) DEFAULT 0,
    show_share BOOLEAN NOT NULL DEFAULT false,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS dashboard.partner_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    partner_id UUID NOT NULL REFERENCES dashboard.partners(id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_partner_sessions_token ON dashboard.partner_sessions(token);
CREATE INDEX IF NOT EXISTS idx_partner_sessions_partner ON dashboard.partner_sessions(partner_id);

CREATE TABLE IF NOT EXISTS dashboard.partner_promocodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    partner_id UUID NOT NULL REFERENCES dashboard.partners(id) ON DELETE CASCADE,
    code TEXT NOT NULL,
    payment_promocode_id UUID,
    discount_percent INTEGER NOT NULL DEFAULT 0,
    max_uses INTEGER,
    duration_days INTEGER,
    expires_at TIMESTAMPTZ,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(partner_id, code)
);

CREATE INDEX IF NOT EXISTS idx_partner_promocodes_partner ON dashboard.partner_promocodes(partner_id);
CREATE INDEX IF NOT EXISTS idx_partner_promocodes_code ON dashboard.partner_promocodes(code);
