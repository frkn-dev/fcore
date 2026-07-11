

CREATE TYPE node_status AS ENUM ('online', 'offline');
CREATE TYPE proto AS ENUM (
'vless_tcp_reality',
'vless_grpc_reality',
'vless_xhttp_reality',
'vmess',
'shadowsocks',
'wireguard',
'hysteria2',
'mtproto'
);

CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    referred_by VARCHAR(13),
    refer_code CHAR(13),
    created_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT now(),
    expires_at TIMESTAMP WITH TIME ZONE ,
    is_deleted BOOL NOT NULL DEFAULT false
);

INSERT INTO subscriptions (refer_code, expires_at)
VALUES
('TEST', now() + interval '7 days');

CREATE INDEX idx_subscriptions_expires_at ON subscriptions(expires_at);
CREATE INDEX idx_subscriptions_referred_by ON subscriptions(referred_by);
CREATE INDEX idx_subscriptions_refcode ON subscriptions(refer_code);



CREATE TABLE connections (
    id UUID PRIMARY KEY,
    proto proto NOT NULL,
    subscription_id UUID REFERENCES subscriptions(id) ON DELETE CASCADE,
    env TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    modified_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    expires_at TIMESTAMP WITH TIME ZONE,
    online BIGINT NOT NULL DEFAULT 0,
    uplink BIGINT NOT NULL DEFAULT 0,
    downlink BIGINT NOT NULL DEFAULT 0,
    wg_privkey TEXT,
    wg_pubkey TEXT,
    wg_address TEXT,
    password TEXT,
    token UUID DEFAULT NULL,
    node_id UUID,
    is_deleted BOOL NOT NULL DEFAULT false
);


CREATE TABLE nodes (
    id UUID PRIMARY KEY,
    env TEXT NOT NULL,
    hostname TEXT NOT NULL,
    address INET NOT NULL,
    status node_status NOT NULL,
    uuid UUID NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    modified_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    label TEXT NOT NULL,
    interface TEXT NOT NULL,
    cores INTEGER NOT NULL DEFAULT 1,
    country TEXT NOT NULL,
    max_bandwidth_bps BIGINT NOT NULL DEFAULT 100000000,
    UNIQUE(uuid, env)
);


CREATE TABLE inbounds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id UUID NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    tag PROTO NOT NULL,
    port INTEGER NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    modified_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    stream_settings JSONB,
    uplink BIGINT,
    downlink BIGINT,
    conn_count BIGINT,
    dns INET[],
    wg_pubkey TEXT,
    wg_privkey TEXT,
    wg_interface TEXT,
    wg_network TEXT,
    wg_address TEXT,
    h2 JSONB,
    mtproto_secret TEXT DEFAULT NULL
);

CREATE UNIQUE INDEX inbounds_node_id_tag_key
ON inbounds (node_id, tag);



CREATE TABLE keys (
    id UUID PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    activated BOOLEAN DEFAULT false,
    days SMALLINT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    modified_at TIMESTAMPTZ DEFAULT NOW(),
    subscription_id UUID DEFAULT NULL,
    distributor VARCHAR(4) NOT NULL DEFAULT 'FRKN'
);

CREATE INDEX idx_keys_code ON keys(code);



CREATE TYPE node_type AS ENUM ('common', 'premium');

ALTER TABLE nodes
ADD COLUMN node_type node_type NOT NULL DEFAULT 'common';



alter table connections drop column "node_id";
alter table connections drop column "wg_pubkey";

alter table subscriptions add column limit_bytes bigint;
alter table subscriptions add column downlink_bytes bigint;
alter table subscriptions add column uplink_bytes bigint;
alter table subscriptions add column last_traffic_reset_at timestamptz;
alter table subscriptions add column daily_start_uplink_bytes bigint;
alter table subscriptions add column daily_start_downlink_bytes bigint;
alter table subscriptions add column last_daily_reset_at timestamptz;


alter table inbounds drop column  uplink;
alter table inbounds drop column  downlink ;
alter table inbounds drop column  conn_count;

alter table inbounds drop column  wg_pubkey;
alter table inbounds drop column  wg_network;


ALTER TABLE inbounds
    ADD COLUMN awg_privkey TEXT,
    ADD COLUMN awg_interface TEXT,
    ADD COLUMN awg_address TEXT,
    ADD COLUMN awg_dns INET[],
    ADD COLUMN awg_obfuscation JSONB;

ALTER TABLE inbounds
    ADD COLUMN awg_mtu SMALLINT;

ALTER TABLE nodes
    ADD COLUMN cluster TEXT;

ALTER TYPE proto
ADD VALUE 'amnezia_wg';

ALTER TYPE proto
ADD VALUE 'vless_xhttp_cdn';

ALTER TABLE subscriptions
ADD COLUMN IF NOT EXISTS referral_bonus_awarded BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE subscriptions
ADD COLUMN IF NOT EXISTS parent_id UUID REFERENCES subscriptions(id),
ADD COLUMN IF NOT EXISTS scope_env TEXT,
ADD COLUMN IF NOT EXISTS premium_token TEXT UNIQUE;

-- Per-connection daily/monthly traffic.
CREATE TABLE IF NOT EXISTS connection_traffic (
    connection_id UUID NOT NULL,
    subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    env TEXT NOT NULL,
    period TEXT NOT NULL CHECK (period IN ('day', 'month')),
    bucket TIMESTAMPTZ NOT NULL,
    uplink_bytes BIGINT NOT NULL DEFAULT 0,
    downlink_bytes BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ DEFAULT now(),
    PRIMARY KEY (connection_id, period, bucket)
);

CREATE INDEX IF NOT EXISTS idx_connection_traffic_sub_bucket
    ON connection_traffic(subscription_id, period, bucket);
CREATE INDEX IF NOT EXISTS idx_connection_traffic_env
    ON connection_traffic(subscription_id, env, period, bucket);

-- Watermark for per-connection traffic persistence.
ALTER TABLE connections
    ADD COLUMN IF NOT EXISTS last_traffic_persist_at TIMESTAMPTZ;

UPDATE connections
    SET last_traffic_persist_at = now()
    WHERE last_traffic_persist_at IS NULL;

-- Subscription-level traffic counters are now kept in connection_traffic.
ALTER TABLE subscriptions
    DROP COLUMN IF EXISTS uplink_bytes,
    DROP COLUMN IF EXISTS downlink_bytes,
    DROP COLUMN IF EXISTS last_traffic_reset_at,
    DROP COLUMN IF EXISTS daily_start_uplink_bytes,
    DROP COLUMN IF EXISTS daily_start_downlink_bytes,
    DROP COLUMN IF EXISTS last_daily_reset_at;

-- Remove referral columns that moved to the mrkting DB.
ALTER TABLE subscriptions DROP COLUMN IF EXISTS referred_by;
ALTER TABLE subscriptions DROP COLUMN IF EXISTS referral_bonus_awarded;

