-- the navidrome upstream is retired: whio runs beside the media and reads it
-- directly, so a linked server is no longer a source of anything.

-- without the connector these sources cannot resolve, and the scan will
-- rediscover the same files as local sources, so keeping them would only leave
-- unplayable duplicates in the catalogue.
DELETE FROM track_sources WHERE provider_id = 'navidrome';

-- tracks that existed only through that source have nothing left to resolve.
DELETE FROM tracks
WHERE NOT EXISTS (
    SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
);

DROP TABLE upstream_links;
DROP TABLE upstream_servers;
