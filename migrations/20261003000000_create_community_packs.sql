-- Packs users publish from the app. Each one is reviewed in the dashboard before the app shows it.
CREATE TABLE IF NOT EXISTS community_packs (
    id SERIAL PRIMARY KEY,
    -- Random per-install id the app also sends with creator requests
    device_id TEXT NOT NULL,
    name TEXT NOT NULL,
    author_name TEXT,
    -- WhatsApp keeps animated and static stickers in separate packs
    animated BOOLEAN NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    -- Distinct devices that added the pack, see community_pack_adds
    add_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    reviewed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_community_packs_status
    ON community_packs(status, add_count DESC, reviewed_at DESC);
CREATE INDEX IF NOT EXISTS idx_community_packs_device ON community_packs(device_id);

-- The stickers as they were when published. Image URLs are built from the 7TV id when served,
-- so a pack can only point at 7TV's CDN.
CREATE TABLE IF NOT EXISTS community_pack_items (
    pack_id INTEGER NOT NULL REFERENCES community_packs(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    emote_id TEXT NOT NULL,
    emote_name TEXT NOT NULL,
    PRIMARY KEY (pack_id, emote_id)
);

CREATE INDEX IF NOT EXISTS idx_community_pack_items_pack ON community_pack_items(pack_id, position);

-- One add per app install, so add_count reflects distinct devices.
CREATE TABLE IF NOT EXISTS community_pack_adds (
    pack_id INTEGER NOT NULL REFERENCES community_packs(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (pack_id, device_id)
);
