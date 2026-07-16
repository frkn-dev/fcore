-- Fix share_percent type for existing dashboard databases.
-- DOUBLE PRECISION is natively supported by tokio-postgres for Rust f64.

ALTER TABLE dashboard.partners
    ALTER COLUMN share_percent TYPE DOUBLE PRECISION USING share_percent::DOUBLE PRECISION;
