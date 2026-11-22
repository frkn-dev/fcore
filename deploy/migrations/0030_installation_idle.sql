BEGIN;

-- Existing installations start their initial idle window at migration time.
ALTER TABLE device_installations
    ADD COLUMN IF NOT EXISTS issued_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS idle_generation bigint NOT NULL DEFAULT 0;

-- Keep observations when an installation's current bindings are detached.
-- Node UUIDs can outlive registrations, so node removal must not erase a
-- recent positive sample.
CREATE TABLE IF NOT EXISTS installation_observations (
    conn_id uuid NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    node_id uuid NOT NULL,
    sampled_at timestamptz NOT NULL,
    observed_since timestamptz NOT NULL,
    last_online_at timestamptz,
    PRIMARY KEY (conn_id, node_id)
);

COMMIT;
