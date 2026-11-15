-- Icebreaker (WebTransport/HTTP3) transport, docs/ICEBREAKER_TZ.md.
-- The enum value is what connections.proto stores; the token lives in its
-- own table because it is per-connection and revocable independently.

ALTER TYPE proto ADD VALUE IF NOT EXISTS 'icebreaker';

CREATE TABLE IF NOT EXISTS icebreaker_connections (
    connection_id     UUID PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
    node_id           UUID NOT NULL,
    token_ciphertext  BYTEA,
    token_hash        BYTEA NOT NULL UNIQUE,
    vpn_ip            INET NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ,
    revoked_at        TIMESTAMPTZ,
    last_seen_at      TIMESTAMPTZ
);

-- A VPN IP is unique per node: the node allocates from its own pool.
CREATE UNIQUE INDEX IF NOT EXISTS icebreaker_connections_node_ip_idx
    ON icebreaker_connections (node_id, vpn_ip);
CREATE INDEX IF NOT EXISTS icebreaker_connections_node_idx
    ON icebreaker_connections (node_id);
