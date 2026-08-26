

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


-- App Store IAP binding: original_transaction_id -> subscription.
-- Created automatically at api startup when [service.apple] is configured;
-- kept here for schema reference.
CREATE TABLE IF NOT EXISTS iap_transactions (
    original_transaction_id TEXT PRIMARY KEY,
    subscription_id UUID NOT NULL,
    product_id TEXT,
    environment TEXT,
    installation_uuid UUID,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now()
);


-- Client-generated WireGuard public key (Amnezia gateway /v1/config flow):
-- when set, the node registers this key as the WG peer instead of the key
-- derived from wg_privkey.

-- AmneziaWgMobile: dedicated address pool (10.77.0.0/16) for mobile clients.
ALTER TYPE proto
ADD VALUE 'amnezia_wg_mobile';

-- Backfill example: create an AmneziaWgMobile connection for every active
-- subscription that has an AmneziaWg connection, allocating addresses from
-- the mobile pool starting at 10.77.0.2 (first_peer_ip). Adjust envs to
-- match enabled_conns of the api config. Keys must be generated per row
-- (see the deploy runbook); restart api afterwards so it reloads PG state.
-- INSERT INTO connections (id, proto, subscription_id, env, wg_privkey, wg_address)
-- SELECT gen_random_uuid(), 'amnezia_wg_mobile', c.subscription_id, c.env,
--        '<generated_privkey>', '10.77.0.2/32'
-- FROM connections c
-- WHERE c.proto = 'amnezia_wg' AND NOT c.is_deleted AND c.subscription_id IS NOT NULL;


-- Named devices: user-facing label for extra named connections a user
-- creates on their subscription (e.g. "Мама Андроид"). NULL means a
-- system/default connection created by the backend. PG-only on purpose:
-- the label never crosses the rkyv wire to the nodes (same precedent as
-- deleted_reason); the api keeps it in an in-memory conn_id -> label side
-- map rebuilt from this column.
ALTER TABLE connections
    ADD COLUMN IF NOT EXISTS label TEXT;


-- Share tokens (frkn://conn/<token>): a scoped credential that lets a
-- recipient import exactly one server. At mint time the backend creates a
-- "child" connection on the same subscription (own UUID/keys, same env and
-- proto as the source connection) and flags it issued_via = 'share'. The
-- flag is PG-only like `label`: the api mirrors it into an in-memory set so
-- share children stay hidden from every owner-facing listing (/v1/services,
-- account info device count, the site device list, the whole-sub feed).
ALTER TABLE connections
    ADD COLUMN IF NOT EXISTS issued_via TEXT;

-- Applied automatically at api startup; kept here for fresh environments.
-- token: Crockford base32, 16 chars (80 bits), stored contiguous lowercase.
-- No TTL: a token lives until explicit revoke. connection_id is the child
-- connection created at mint; node_id pins the config to one node.
CREATE TABLE IF NOT EXISTS share_tokens (
    token TEXT PRIMARY KEY,
    subscription_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    node_id UUID NOT NULL,
    source_connection_id UUID NOT NULL,
    label TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

-- Idempotent mint: a repeat request for the same (source, node, label)
-- triple returns the existing live token instead of duplicating the child.
CREATE UNIQUE INDEX IF NOT EXISTS share_tokens_active_triple
    ON share_tokens (source_connection_id, node_id, label)
    WHERE revoked_at IS NULL;

-- Node-pinned connections ("named devices"): the pin is the node's uuid
-- (nodes.uuid), not the row id. The column existed historically and was
-- dropped above (line 122) — re-added here. NULL = env-wide (current
-- behavior); only named devices created with a node pin set it. PG-only,
-- like label/issued_via: the api mirrors it into an in-memory side map.
alter table connections add column node_id uuid;

-- Extra entry IPs of a node (anti IP-blocking). TEXT[], not INET[]:
-- values are validated as Ipv4Addr at the API boundary, so no
-- tokio-postgres type plumbing is needed. The first element is the
-- primary (== nodes.address); NULL = single-address node.
alter table nodes add column node_ips text[];

-- Per-inbound PersistentKeepalive (seconds) injected into client WG/AWG
-- configs. Set from the node's config.toml ([wg]/[awg]/[awg_mobile]
-- keepalive); it cannot live in the interface .conf because wg-quick/awg
-- refuse to start with a keepalive in [Interface]. NULL = clients get the
-- default 25.
alter table inbounds add column keepalive integer;
