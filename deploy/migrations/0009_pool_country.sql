-- 0009: страна пула как целого.
--
-- У членов пула страны разные (EU: at/ee/eu01/eu02/fr/pl), поэтому страна ноды
-- не годится для подписи под флагом на карточке пула — получалось «флаг ЕС,
-- подпись Poland». Нода-участник объявляет страну пула в своём конфиге
-- (`[node] pool_country_code` / `pool_country_name`), api отдаёт её в карточке
-- пула полями `pool_country_code` / `pool_country_name`.
--
-- Пусто = как раньше: клиент берёт страну ноды.

ALTER TABLE nodes ADD COLUMN IF NOT EXISTS pool_country_code TEXT;
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS pool_country_name TEXT;
