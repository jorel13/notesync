-- Device tokens expire unless the device refreshes them. A refresh replaces
-- the token; the previous one keeps working for a few minutes in case the
-- device never received the new one.
ALTER TABLE devices ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN prev_token_hash TEXT;
ALTER TABLE devices ADD COLUMN prev_expires_at INTEGER NOT NULL DEFAULT 0;
-- Devices signed in before this change get the default 30 days from now.
UPDATE devices SET expires_at = CAST(strftime('%s', 'now') AS INTEGER) + 30 * 86400;
CREATE INDEX devices_prev_token ON devices(prev_token_hash);

-- Failed sign-ins per username (lower case), kept in the database so a
-- lockout survives a restart and an admin can see and clear it.
CREATE TABLE login_failures (
    username_key TEXT    PRIMARY KEY,
    failures     INTEGER NOT NULL,
    window_start INTEGER NOT NULL,
    locked_until INTEGER NOT NULL DEFAULT 0
);
