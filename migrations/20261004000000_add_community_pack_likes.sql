-- Likes on community packs: one per app install, and Popular sorts by them.
ALTER TABLE community_packs ADD COLUMN IF NOT EXISTS like_count INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS community_pack_likes (
    pack_id INTEGER NOT NULL REFERENCES community_packs(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (pack_id, device_id)
);

CREATE INDEX IF NOT EXISTS idx_community_packs_popular
    ON community_packs(status, like_count DESC, add_count DESC);
