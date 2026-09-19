ALTER TABLE tracks
    ADD COLUMN title_normalized TEXT GENERATED ALWAYS AS (
        lower(regexp_replace(normalize(title, NFC), '\s+', ' ', 'g'))
    ) STORED;
