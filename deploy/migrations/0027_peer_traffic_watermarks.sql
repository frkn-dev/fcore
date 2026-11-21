-- Watermark of the peer traffic persister: the end of the last booked window
-- and the cumulative bytes booked into `peer_traffic` for that peer.
--
-- Mirrors `connections.uplink` / `connections.downlink` /
-- `connections.last_traffic_persist_at` for v1 connections. Before this table
-- the origin of the window lived only in memory and was seeded with
-- `now - interval`, so every api restart re-counted up to one interval of
-- already persisted peer traffic.
--
-- No FK to `peers` on purpose, same as `peer_traffic`: quota history must
-- survive a peer or device being deleted.
CREATE TABLE IF NOT EXISTS peer_watermarks (
    peer_id         uuid PRIMARY KEY,
    subscription_id uuid NOT NULL,
    env             text NOT NULL,
    uplink          bigint NOT NULL DEFAULT 0,
    downlink        bigint NOT NULL DEFAULT 0,
    last_persist_at timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS peer_watermarks_sub
    ON peer_watermarks (subscription_id, env);
