CREATE TABLE boxes (
    code              TEXT    PRIMARY KEY NOT NULL,
    delete_token_hash TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL
);

CREATE INDEX boxes_expires_at ON boxes (expires_at);

CREATE TABLE files (
    id           TEXT    PRIMARY KEY NOT NULL,
    box_code     TEXT    NOT NULL REFERENCES boxes (code) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    name         TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    content_type TEXT    NOT NULL
);

CREATE INDEX files_box_code ON files (box_code);
