BEGIN;

CREATE TABLE IF NOT EXISTS installation_idle_catalog (
    singleton boolean PRIMARY KEY CHECK (singleton),
    revision bigint NOT NULL CHECK (revision >= 0)
);

INSERT INTO installation_idle_catalog (singleton, revision)
VALUES (true, 0)
ON CONFLICT (singleton) DO NOTHING;

COMMIT;
