# Central API in Docker

This is a deployment scaffold, not a ready-to-run production configuration. The API and PostgreSQL are containerized; proxy nodes and auth run separately. Docker Compose v2 is required.

1. Copy `src/bin/api/api-example.toml` to `docker/central/config.toml`. Set `[service] listen = "0.0.0.0"`, choose real tokens, keys, URLs and network settings, and set `[pg] host = "db"`, `db = "api"`, `username = "fcore"`, and a strong `password`.
2. Create `docker/central/.env` with `POSTGRES_PASSWORD` set to the same password. Protect both files, e.g. `chmod 600 docker/central/.env` and make the config readable by container UID 10001. Do not commit them.
3. Provision a complete PostgreSQL schema. The migrations are incremental and do not create the entire base schema; `dev/dev.sql` is a development reference and contains test data. Verify the schema source for this deployment before using it.
4. Apply the migrations in `deploy/migrations/` in order before starting or updating API, as described in `deploy/migrations/README.md`.
5. Build and start with `docker compose -f docker/central/compose.yaml up -d --build` after database preparation. The HTTP API binds to localhost; configure a reverse proxy and TLS as needed. Ports 3001 and 3002 bind to localhost; connect remote nodes through a protected tunnel or configure a secured transport.

The build uses Rust nightly and can consume several GB of disk space. Volumes persist database data, snapshots and logs. PostgreSQL credentials in `.env` initialize only a new database volume; changing them later does not update existing database passwords.
