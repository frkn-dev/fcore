-- Peers: the client owns the key pair, the api hands out the address.
--
-- Additive by design: nothing in this file is read by the legacy (v1) paths,
-- so applying it does not change how existing connections work.
--
-- NOTE: `ALTER TYPE ... ADD VALUE` may not be used in the same transaction
-- that adds the value, so this file must be applied statement-by-statement
-- (psql without -1), same as 0015_node_status_degraded.sql.

CREATE EXTENSION IF NOT EXISTS btree_gist;

ALTER TYPE proto ADD VALUE IF NOT EXISTS 'amnezia_peer';
ALTER TYPE proto ADD VALUE IF NOT EXISTS 'wireguard_peer';

-- Address space of an env. An env may hold several blocks; blocks of
-- different envs must not overlap, otherwise a client that runs two tunnels
-- at once gets the same address on both interfaces.
CREATE TABLE IF NOT EXISTS env_networks (
    env    text NOT NULL,
    subnet cidr NOT NULL,
    PRIMARY KEY (env, subnet)
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'env_networks_no_overlap'
    ) THEN
        ALTER TABLE env_networks
            ADD CONSTRAINT env_networks_no_overlap
            EXCLUDE USING gist (subnet inet_ops WITH &&);
    END IF;
END
$$;

-- What a subscription is entitled to: one row per env.
CREATE TABLE IF NOT EXISTS subscription_access (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id uuid NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    env             text NOT NULL,
    protos          proto[] NOT NULL DEFAULT '{}',
    -- Which plan bought this row (standard / super-duper / …). Used to keep a
    -- subscription on one shard and to count load per shard.
    plan            text,
    sources         text[]  NOT NULL DEFAULT '{}',
    expires_at      timestamptz,
    traffic_limit   bigint,
    revoked_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz
);

CREATE UNIQUE INDEX IF NOT EXISTS subscription_access_env_uniq
    ON subscription_access (subscription_id, env) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS subscription_access_live
    ON subscription_access (subscription_id) WHERE revoked_at IS NULL;

-- Soft capacity of a shard. `max_subscriptions` is a goal, not a hard gate:
-- the api still assigns the least-loaded shard when every shard is full and
-- raises an alert instead of refusing the payer.
-- Soft capacity per shard. NULL = compute from live nodes
-- (`nodes * peers.subscriptions_per_node`); a number = explicit override.
-- One env serves exactly one plan: a shard is a hardware/business decision
-- (standard machines vs paid-traffic machines), never a mix — otherwise two
-- plans would silently share (and double) one node's capacity.
CREATE TABLE IF NOT EXISTS env_capacity (
    env               text PRIMARY KEY,
    plan              text NOT NULL,
    max_subscriptions integer,
    enabled           boolean NOT NULL DEFAULT true
);

-- Registry of what exists, for the admin UI and for the "which shards and
-- locations do we run" question. Never used for assignment: that reads live
-- nodes, so a card cannot outlive its machines here.
CREATE TABLE IF NOT EXISTS pools (
    env              text NOT NULL,
    pool             text NOT NULL,
    pool_translation jsonb,
    first_seen       timestamptz NOT NULL DEFAULT now(),
    last_seen        timestamptz NOT NULL DEFAULT now(),
    node_count       integer NOT NULL DEFAULT 0,
    PRIMARY KEY (env, pool)
);

-- Paid but not served: activation could not be assigned a shard. The key is
-- NOT consumed, so the user can retry once a shard is live; the row exists so
-- support and ops see the case instead of hearing it from the user.
CREATE TABLE IF NOT EXISTS activation_failures (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    key_id          uuid,
    key_code        text,
    product         text,
    plan            text,
    reason          text NOT NULL,
    subscription_id uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    resolved_at     timestamptz
);

CREATE INDEX IF NOT EXISTS activation_failures_open
    ON activation_failures (created_at DESC) WHERE resolved_at IS NULL;

-- The device owns the key pair; peers are its tunnels.
CREATE TABLE IF NOT EXISTS devices (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id uuid NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    device_uid      uuid NOT NULL,
    device_name     text,
    public_key      text NOT NULL,
    platform        text,
    support_tag     text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    rotated_at      timestamptz
);

CREATE UNIQUE INDEX IF NOT EXISTS devices_uid_uniq
    ON devices (subscription_id, device_uid);
CREATE UNIQUE INDEX IF NOT EXISTS devices_support_uniq
    ON devices (support_tag);
CREATE INDEX IF NOT EXISTS devices_sub
    ON devices (subscription_id);

CREATE TABLE IF NOT EXISTS peers (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    device_id       uuid NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    subscription_id uuid NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    env             text NOT NULL,
    proto           proto NOT NULL,
    public_key      text NOT NULL,
    address         cidr NOT NULL,
    label           text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    revoked_at      timestamptz,
    revoke_reason   text
);

-- Address and public key are unique among live peers of one env: that is the
-- fuse against two api instances handing out the same address.
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_addr
    ON peers (env, address) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_key
    ON peers (env, public_key) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_label
    ON peers (device_id, label) WHERE revoked_at IS NULL AND label IS NOT NULL;
CREATE INDEX IF NOT EXISTS peers_live_sub
    ON peers (subscription_id) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS peers_live_dev
    ON peers (device_id) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS peers_env_live
    ON peers (env) WHERE revoked_at IS NULL;

-- Per-subscription overrides. Absent row (or 0) = defaults from the api
-- config. Written by the admin, never by the client: a client claiming to be
-- a router must not be able to raise its own limit.
CREATE TABLE IF NOT EXISTS subscription_limits (
    subscription_id        uuid PRIMARY KEY REFERENCES subscriptions(id) ON DELETE CASCADE,
    peers_per_device       integer,
    peers_per_subscription integer,
    updated_at             timestamptz NOT NULL DEFAULT now()
);

-- Traffic of peers, same shape as connection_traffic so the two can be summed.
-- Deliberately no FK to peers: quota history must survive a peer or device
-- being deleted, otherwise the limit is bypassed by recreating the tunnel.
CREATE TABLE IF NOT EXISTS peer_traffic (
    peer_id         uuid NOT NULL,
    subscription_id uuid NOT NULL,
    env             text NOT NULL,
    period          text NOT NULL,
    bucket          timestamptz NOT NULL,
    uplink_bytes    bigint NOT NULL DEFAULT 0,
    downlink_bytes  bigint NOT NULL DEFAULT 0,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (peer_id, period, bucket)
);

CREATE INDEX IF NOT EXISTS peer_traffic_sub_env
    ON peer_traffic (subscription_id, env, period);

-- One read for "how much did this subscription use", regardless of whether
-- it came through a legacy connection or a peer.
CREATE OR REPLACE VIEW subscription_traffic AS
    SELECT subscription_id, env, period, bucket, uplink_bytes, downlink_bytes
      FROM connection_traffic
    UNION ALL
    SELECT subscription_id, env, period, bucket, uplink_bytes, downlink_bytes
      FROM peer_traffic;
