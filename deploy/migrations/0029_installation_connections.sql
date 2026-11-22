BEGIN;

CREATE TABLE IF NOT EXISTS device_installations (
    sub_id uuid NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    installation_uuid uuid NOT NULL,
    revoked_at timestamptz,
    PRIMARY KEY (sub_id, installation_uuid)
);

CREATE TABLE IF NOT EXISTS device_installation_connections (
    sub_id uuid NOT NULL,
    installation_uuid uuid NOT NULL,
    base_conn_id uuid NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    conn_id uuid NOT NULL UNIQUE REFERENCES connections(id) ON DELETE CASCADE,
    PRIMARY KEY (sub_id, installation_uuid, base_conn_id),
    FOREIGN KEY (sub_id, installation_uuid)
        REFERENCES device_installations(sub_id, installation_uuid) ON DELETE CASCADE,
    CHECK (base_conn_id <> conn_id)
);

CREATE INDEX IF NOT EXISTS device_installation_connections_base
    ON device_installation_connections (base_conn_id);

COMMIT;
