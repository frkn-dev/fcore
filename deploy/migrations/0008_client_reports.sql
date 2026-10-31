-- 0008: отчёты клиента и журнал попыток подключения.
--
-- Клиент 4.8.14.79+ присылает отчёт целиком, а не «одна цель = одна строка»:
--   туннель выключен → targets[] (состояние каждого адреса) + attempts[] (что
--                      реально произошло: исход, код, сколько ждали)
--   туннель включён  → mode="tunnel", target, tunnel, session, mtu
--
-- Поэтому данные разложены по трём местам, у каждого своя единица хранения:
--   block_observations  — «цель отвечает или нет» (существующая, агрегаты тут)
--   client_reports      — отчёт целиком: кто прислал и в каком состоянии
--   connection_attempts — журнал попыток: что не получилось и сколько ждали
--
-- Таблицы создаются и в рантайме (PgReport::ensure_table), миграция нужна для
-- явного применения и для истории схемы.

CREATE TABLE IF NOT EXISTS client_reports (
    id               uuid PRIMARY KEY,
    subscription_id  uuid,
    support_tag      text,
    mode             text,
    app_version      text NOT NULL,
    platform         text NOT NULL,
    os               text,
    app              jsonb,
    config           jsonb,
    mtu              jsonb,
    tunnel           jsonb,
    network          jsonb,
    session          jsonb,
    device           jsonb,
    payload          jsonb NOT NULL,
    country_code     text,
    asn              int,
    client_country   char(2),
    client_org       text,
    client_city      text,
    ip_prefix        cidr,
    ip_hash          bytea,
    observed_at      timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS client_reports_tag_time
    ON client_reports (support_tag, created_at DESC);
CREATE INDEX IF NOT EXISTS client_reports_sub_time
    ON client_reports (subscription_id, created_at DESC);
CREATE INDEX IF NOT EXISTS client_reports_time
    ON client_reports (created_at DESC);

CREATE TABLE IF NOT EXISTS connection_attempts (
    id               uuid PRIMARY KEY,
    report_id        uuid NOT NULL,
    subscription_id  uuid,
    support_tag      text,
    app_version      text NOT NULL,
    platform         text NOT NULL,
    address          inet,
    port             int,
    protocol         text,
    outcome          text,
    detail           text,
    waited_ms        int,
    client_country   char(2),
    client_org       text,
    ip_hash          bytea,
    attempted_at     timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS conn_attempts_tag_time
    ON connection_attempts (support_tag, created_at DESC);
CREATE INDEX IF NOT EXISTS conn_attempts_addr_time
    ON connection_attempts (address, port, created_at DESC);
CREATE INDEX IF NOT EXISTS conn_attempts_report
    ON connection_attempts (report_id);
