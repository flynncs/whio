UPDATE track_sources SET artwork_ref = NULL WHERE artwork_kind = 'upstream';

ALTER TABLE track_sources DROP COLUMN artwork_kind;
