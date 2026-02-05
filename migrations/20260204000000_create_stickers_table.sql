-- Add migration script here
CREATE TABLE IF NOT EXISTS stickers (
    id SERIAL PRIMARY KEY,
    seven_tv_id TEXT NOT NULL,
    emote_name TEXT NOT NULL,
    file_name TEXT NOT NULL,
    url TEXT NOT NULL,
    owner_name TEXT,
    tags TEXT[],
    animated BOOLEAN DEFAULT false,
    folder_name TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    -- Unique constraint to prevent duplicate stickers in the same folder
    UNIQUE(seven_tv_id, folder_name)
);

-- Index for fast searches by folder
CREATE INDEX IF NOT EXISTS idx_stickers_folder_name ON stickers(folder_name);

-- GIN index for fast searches by tags
CREATE INDEX IF NOT EXISTS idx_stickers_tags ON stickers USING GIN (tags);
