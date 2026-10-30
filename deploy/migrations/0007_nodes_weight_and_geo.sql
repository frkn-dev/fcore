-- nodes.weight: приоритет ноды в пуле (больше = предпочтительнее).
--
-- Отдаётся клиенту в /v1/services (connections[].weight), чтобы он мог
-- сортировать пул: сначала больший вес, потом скор/задержка. 0 = нейтрально,
-- отрицательный вес = нода нежелательна, но остаётся рабочей.
--
-- Применяется на деплое (deploy/migrate.sh), до старта нового бинаря:
-- PgNode::all() читает колонку безусловно.

ALTER TABLE nodes ADD COLUMN IF NOT EXISTS weight INT NOT NULL DEFAULT 0;

-- Телеметрия: оператор и город клиента по его IP.
--
-- client_org  — organization из GeoLite2-ASN («MegaFon», «MTS PJSC»);
-- client_city — город из GeoLite2-City (английское имя).
-- Оба поля опциональны: без баз остаются NULL, отчёты продолжают приниматься.

ALTER TABLE block_observations ADD COLUMN IF NOT EXISTS client_org TEXT;
ALTER TABLE block_observations ADD COLUMN IF NOT EXISTS client_city TEXT;
