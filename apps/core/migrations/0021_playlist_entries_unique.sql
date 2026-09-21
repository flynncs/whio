ALTER TABLE playlist_entries
    ADD CONSTRAINT playlist_entries_track_unique UNIQUE (playlist_id, track_id);
