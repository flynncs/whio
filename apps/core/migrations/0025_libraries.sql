CREATE SEQUENCE libraries_numeric_id_seq AS INTEGER START 1;

CREATE TABLE libraries (
    id UUID PRIMARY KEY,
    numeric_id INTEGER NOT NULL UNIQUE DEFAULT nextval('libraries_numeric_id_seq'),
    label TEXT NOT NULL UNIQUE CHECK (char_length(label) BETWEEN 1 AND 100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE library_roots (
    library_id UUID NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    root TEXT NOT NULL CHECK (char_length(root) BETWEEN 1 AND 4096),
    PRIMARY KEY (library_id, root)
);
