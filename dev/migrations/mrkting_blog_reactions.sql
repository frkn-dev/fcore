CREATE SCHEMA IF NOT EXISTS mrkting;

CREATE TABLE IF NOT EXISTS mrkting.blog_reactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    article_id TEXT NOT NULL,
    user_key TEXT NOT NULL,
    reaction SMALLINT NOT NULL CHECK (reaction IN (-1, 1)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(article_id, user_key)
);

CREATE INDEX IF NOT EXISTS idx_blog_reactions_article ON mrkting.blog_reactions(article_id);
