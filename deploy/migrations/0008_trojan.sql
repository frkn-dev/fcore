-- Append only: the tag is also serialized on the node bus and in snapshots.
ALTER TYPE proto ADD VALUE IF NOT EXISTS 'trojan';
