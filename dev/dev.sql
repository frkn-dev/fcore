

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

ALTER TYPE proto
ADD VALUE 'amnezia_wg';



