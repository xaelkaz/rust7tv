-- WhatsApp-ready sticker files, uploaded by the app that published a community pack, so people
-- adding the pack download them instead of converting every sticker on their phone.
ALTER TABLE community_pack_items ADD COLUMN IF NOT EXISTS sticker_url TEXT;
ALTER TABLE community_pack_items ADD COLUMN IF NOT EXISTS sticker_bytes INTEGER;

-- Ready-made packs made from a community pack keep its files.
ALTER TABLE starter_pack_items ADD COLUMN IF NOT EXISTS sticker_url TEXT;
