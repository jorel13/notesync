CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT    NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

-- One row per signed-in device. Only a SHA-256 hash of the token is stored.
CREATE TABLE devices (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    token_hash TEXT    NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    revoked    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX devices_user ON devices(user_id);

CREATE TABLE vaults (
    id         INTEGER PRIMARY KEY,
    owner_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    head_seq   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE UNIQUE INDEX vaults_owner_name ON vaults(owner_id, name COLLATE NOCASE) WHERE deleted_at IS NULL;

-- File contents, stored on disk per vault. created_at is the last upload time.
CREATE TABLE blobs (
    vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    hash       TEXT    NOT NULL,
    size       INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, hash)
);

-- The vault's history: every upsert or delete, numbered per vault.
CREATE TABLE changes (
    vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    op         TEXT    NOT NULL CHECK (op IN ('upsert', 'delete')),
    hash       TEXT,
    size       INTEGER,
    device_id  INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, seq)
);
CREATE INDEX changes_path ON changes(vault_id, path);
CREATE INDEX changes_hash ON changes(vault_id, hash);

-- Latest state of each path, kept in step with changes.
CREATE TABLE current_files (
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    path     TEXT    NOT NULL,
    path_key TEXT    NOT NULL,
    hash     TEXT    NOT NULL,
    size     INTEGER NOT NULL,
    seq      INTEGER NOT NULL,
    PRIMARY KEY (vault_id, path)
);
CREATE UNIQUE INDEX current_files_key ON current_files(vault_id, path_key);
CREATE INDEX current_files_hash ON current_files(vault_id, hash);
