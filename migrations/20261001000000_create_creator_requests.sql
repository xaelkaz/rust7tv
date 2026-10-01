-- Streamers requested by app users, deduplicated by Twitch channel.
CREATE TABLE IF NOT EXISTS creator_requests (
    id SERIAL PRIMARY KEY,
    -- Lowercase Twitch login, e.g. "dn9n"
    channel_name TEXT NOT NULL UNIQUE,
    -- Parsed from the optional 7TV link; the first valid value wins
    seven_tv_user_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    request_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_requested_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_creator_requests_status_count
    ON creator_requests(status, request_count DESC);

-- One vote per app install, so request_count reflects distinct devices.
CREATE TABLE IF NOT EXISTS creator_request_votes (
    request_id INTEGER NOT NULL REFERENCES creator_requests(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (request_id, device_id)
);
