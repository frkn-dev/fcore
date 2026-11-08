-- The development schema predates the node_type variants used by this API.
ALTER TYPE node_type ADD VALUE IF NOT EXISTS 'service';
ALTER TYPE node_type ADD VALUE IF NOT EXISTS 'agent';
