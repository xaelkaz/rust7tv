-- Where a creator's sticker sits in their 7TV emote set, so the app gets them in the channel's
-- order. Empty for trending rows and for creators not synced since this was added.
ALTER TABLE stickers ADD COLUMN IF NOT EXISTS position INTEGER;
