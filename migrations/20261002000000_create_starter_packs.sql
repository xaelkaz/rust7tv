-- Curated packs the app offers with a one-tap "Add to WhatsApp", managed from the dashboard.
CREATE TABLE IF NOT EXISTS starter_packs (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    emoji TEXT,
    -- WhatsApp keeps animated and static stickers in separate packs
    animated BOOLEAN NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    published BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Snapshot of each emote as search returned it, so a pack doesn't change if 7TV does.
CREATE TABLE IF NOT EXISTS starter_pack_items (
    pack_id INTEGER NOT NULL REFERENCES starter_packs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    emote_id TEXT NOT NULL,
    emote_name TEXT NOT NULL,
    file_name TEXT NOT NULL,
    url TEXT NOT NULL,
    animated BOOLEAN NOT NULL,
    animated_preview_url TEXT,
    poster_url TEXT,
    tags TEXT[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (pack_id, emote_id)
);

CREATE INDEX IF NOT EXISTS idx_starter_pack_items_pack ON starter_pack_items(pack_id, position);
