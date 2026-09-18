DELETE FROM track_sources WHERE provider_id = 'navidrome';

DELETE FROM tracks
WHERE NOT EXISTS (
    SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
);

DROP TABLE upstream_links;
DROP TABLE upstream_servers;
