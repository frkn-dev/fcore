-- 0012: события подключения (connect-event).
--
-- Клиент сообщает «туннель поднялся» тем же запросом POST /v1/config с флагом
-- is_connect_event: true. Провалы сюда не пишутся — для них v1/report/block.
-- Таблица также создаётся кодом при старте (postgres/connect.rs).

CREATE TABLE IF NOT EXISTS connect_events (
    id              bigserial PRIMARY KEY,
    at              timestamptz NOT NULL DEFAULT now(),
    subscription_id uuid,
    node_id         uuid,
    connection_id   uuid,
    protocol        text NOT NULL,
    endpoint        inet,
    endpoint_port   int,
    support_tag     uuid,
    app_version     text NOT NULL,
    platform        text NOT NULL,
    country         text
);

CREATE INDEX IF NOT EXISTS connect_events_sub_time
    ON connect_events (subscription_id, at DESC);
CREATE INDEX IF NOT EXISTS connect_events_tag_time
    ON connect_events (support_tag, at DESC);
CREATE INDEX IF NOT EXISTS connect_events_endpoint_time
    ON connect_events (endpoint, at DESC);
