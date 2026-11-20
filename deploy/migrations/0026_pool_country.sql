-- Country of the pool, not of the machine.
--
-- The card the app shows is a pool; a pool may span countries (e.g. a park of
-- DE + NL machines sold as one location), and the flag must then be the one the
-- operator declared for the pool — not whatever node happened to be listed
-- first. The value comes from the node config (`[node] pool_country_code` /
-- `pool_country_name`) and is exposed on the card as `country` /
-- `country_name`; a pool that declares nothing falls back to the country of its
-- first node.
--
-- The columns existed once (0009) and were dropped (0011) when the v1 cards
-- stopped using them; the peers cards need them again. The api also adds both
-- at startup (`ensure_pool_column`), so applying this file is not a hard
-- prerequisite for the binary.
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS pool_country_code TEXT;
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS pool_country_name TEXT;
