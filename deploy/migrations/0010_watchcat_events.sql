-- 0010: события watchcat (переключения маршрутов, поломки перехвата).
--
-- watchcat не уведомляет сам: он шлёт сигнал в api, а api решает, кого и
-- когда оповещать. Здесь — журнал сигналов: кто, в какой группе выходов,
-- что случилось и когда.
--
-- Уникальный индекс по (hostname, group_tag, kind, occurred_at) делает
-- повторную отправку идемпотентной: ретрай демона не создаёт дубль.
-- Таблица также создаётся кодом при старте (postgres/watchcat.rs),
-- миграция нужна, чтобы схема была в репозитории и применялась до деплоя.

CREATE TABLE IF NOT EXISTS watchcat_events (
    id          bigserial PRIMARY KEY,
    hostname    text NOT NULL,
    group_tag   text NOT NULL,
    kind        text NOT NULL,
    from_tag    text,
    to_tag      text,
    reason      text,
    detail      text,
    occurred_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    notified    boolean NOT NULL DEFAULT false
);

CREATE UNIQUE INDEX IF NOT EXISTS watchcat_events_dedup
    ON watchcat_events (hostname, group_tag, kind, occurred_at);

CREATE INDEX IF NOT EXISTS watchcat_events_group_time
    ON watchcat_events (group_tag, occurred_at DESC);

CREATE INDEX IF NOT EXISTS watchcat_events_time
    ON watchcat_events (occurred_at DESC);
