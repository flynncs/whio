CREATE INDEX track_artist_credits_artist_idx ON track_artist_credits (artist_id);

CREATE INDEX track_play_stats_user_play_count_idx ON track_play_stats (user_id, play_count DESC);

CREATE INDEX track_play_stats_user_last_played_idx ON track_play_stats (user_id, last_played_at DESC);
