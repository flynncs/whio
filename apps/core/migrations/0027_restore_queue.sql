CREATE TABLE IF NOT EXISTS restore_queues (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    current_index INTEGER,
    position_ms BIGINT NOT NULL DEFAULT 0,
    revision BIGINT NOT NULL DEFAULT 0,
    changed_by UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT restore_queues_current_index_nonnegative
        CHECK (current_index IS NULL OR current_index >= 0),
    CONSTRAINT restore_queues_position_ms_nonnegative CHECK (position_ms >= 0)
);

CREATE TABLE IF NOT EXISTS restore_queue_entries (
    queue_user_id UUID NOT NULL REFERENCES restore_queues (user_id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    entry_id UUID NOT NULL DEFAULT gen_random_uuid(),
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    CONSTRAINT restore_queue_entries_pkey PRIMARY KEY (queue_user_id, position),
    CONSTRAINT restore_queue_entries_entry_unique UNIQUE (entry_id),
    CONSTRAINT restore_queue_entries_position_nonnegative CHECK (position >= 0)
);

CREATE INDEX IF NOT EXISTS restore_queue_entries_track_idx
    ON restore_queue_entries (track_id);
