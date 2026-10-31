-- 0011: откат страны пула.
--
-- Фича отменена: подпись под флагом снова берётся у ноды, как было. Код
-- поля больше не читает и не пишет, значения из таблицы уже обнулены.
-- Колонки убираем совсем, чтобы схема не обещала того, чего нет.

ALTER TABLE nodes DROP COLUMN IF EXISTS pool_country_code;
ALTER TABLE nodes DROP COLUMN IF EXISTS pool_country_name;
