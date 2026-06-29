CREATE TABLE IF NOT EXISTS users (
    id SERIAL PRIMARY KEY,
    seven_tv_id TEXT NOT NULL UNIQUE,
    folder_name TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    last_synced_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    emote_count INTEGER DEFAULT 0
);
