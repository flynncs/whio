ALTER TABLE playlists
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0;

ALTER TABLE playlist_entries
    ADD COLUMN IF NOT EXISTS entry_id UUID;

UPDATE playlist_entries
SET entry_id = md5(playlist_id::text || ':' || position::text)::uuid
WHERE entry_id IS NULL;

ALTER TABLE playlist_entries
    ALTER COLUMN entry_id SET DEFAULT gen_random_uuid();

ALTER TABLE playlist_entries
    ALTER COLUMN entry_id SET NOT NULL;

ALTER TABLE playlist_entries
    DROP CONSTRAINT IF EXISTS playlist_entries_track_unique;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'playlist_entries'::regclass
          AND conname = 'playlist_entries_entry_pkey'
    ) THEN
        ALTER TABLE playlist_entries DROP CONSTRAINT IF EXISTS playlist_entries_pkey;
        ALTER TABLE playlist_entries
            ADD CONSTRAINT playlist_entries_entry_pkey PRIMARY KEY (entry_id);
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'playlist_entries'::regclass
          AND conname = 'playlist_entries_position_unique'
    ) THEN
        ALTER TABLE playlist_entries
            ADD CONSTRAINT playlist_entries_position_unique UNIQUE (playlist_id, position);
    END IF;
END $$;
