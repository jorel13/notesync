# Changelog

The plugin and server share one version number. Every change to either one gets a new
version, so Settings > notesync > Status (plugin) and `/api/v1/health` (server) show
whether a fix has reached a device.

## 0.4.0 (2026-11-09)

### Server
- **The admin page has its own port.** It's served on `NOTESYNC_ADMIN_BIND_ADDR`
  (`127.0.0.1:8081` by default, `0.0.0.0:8081` in the container) and no longer on the sync
  port. Publish it as its own name, such as a `notes-admin` Tailscale Service limited to you,
  so people who only sync can't reach the admin sign-in.
- **Delete a user:** the admin page's **Delete user** (asks for your password) and
  `notesync-server user delete <name>` remove the account, its vaults, history and stored
  files. The only admin can't be deleted.

### Docs
- Deployment guide: publishing the two ports, Tailscale grants that keep the admin page to
  you, and a new "Managing people and devices" section, including sharing the server with
  family.

### Plugin
- No changes apart from the version number.

## 0.3.0 (2026-11-09)

### Server
- **Admin web page** at `/admin`, for admin accounts: add users, set passwords, disable or
  enable accounts, grant or remove admin rights, clear lockouts, sign out devices, restore
  deleted vaults, see storage use and settings, and run cleanup.
  - Sign-ins are short sessions (30 minutes idle, 12 hours at most) in a secure,
    `SameSite=Strict`, `HttpOnly` cookie, with the same lockout and rate limits as devices.
  - Every request needs the `X-Notesync-Admin` header, so other websites can't act as a
    signed-in admin. The page has a strict Content Security Policy.
  - Setting a password and granting admin rights need the admin's own password.
  - Every action is logged.
  - `NOTESYNC_ADMIN_PAGE=off` turns it off.
- Changing a password, disabling an account or removing admin rights also ends that
  person's admin page sessions.

### Plugin
- No changes apart from the version number. 0.2.0 plugins keep working with this server.

## 0.2.0 (2026-11-08)

Security release. **0.2.0 servers refuse 0.1.x plugins**: update the server, then every
device's plugin. Devices stay signed in.

### Plugin
- **Security:** paths sent by the server are checked before anything is written, and
  `.obsidian` (or a renamed settings folder), `.trash` and `.git` are excluded in any letter
  case. Before, a compromised server or a stolen token could write `.Obsidian/plugins/...` and
  run code on every device the next time Obsidian started.
- **Security:** the server address must use `https://`; `http://` only works for
  `localhost`, for testing.
- The sign-in renews itself about once a day while syncing, and settings show when it would
  expire.
- Your excluded paths now ignore letter case, like Windows and Android do.
- Reports its real version to the server (it always said 0.1.0 before).
- Rate-limit messages from the server say how long to wait.

### Server
- **Sign-ins expire:** device tokens last `NOTESYNC_TOKEN_DAYS` (30), and devices swap them
  for new ones about once a day (`POST /auth/refresh`).
- **Lockouts are stored in the database:** 10 wrong passwords in 15 minutes lock the
  username for 15 minutes, across restarts. `user list` shows lockouts; `user unlock` clears
  them. Failed sign-ins and lockouts are logged.
- **Rate limits:** sign-ins per username and sync runs per device (10 at once, then one every
  5 seconds), sign-ins overall, and all requests per device. All configurable.
- **Deleting a vault needs the password.** New `vault list --deleted` and `vault restore`
  commands.
- Changing the password signs out other devices by default.
- Password hashing runs off the request threads, two at a time.
- Request size limits: 16 KB for sign-in, 16 MB for everything except file uploads.
- Paths in `.obsidian`, `.trash` and `.git` are rejected in any letter case.
- `last_seen` is written at most once a minute per device.
- Minimum plugin version is now 0.2.0.

## 0.1.1 (2026-11-08)

### Plugin
- **Fixed:** a rename that only changed letter case (`Note.md` → `note.md`) couldn't be
  applied on Windows, because the new name looked like an existing file. The device kept the
  old name and retried the same server changes on every sync without moving past them.
- Settings > notesync > Status now shows the plugin version that Obsidian loaded.

### Docs
- Diagrams of the plugin's parts, the server's modules and a typical deployment.
- Limits, storage use, automatic cleanup and maintenance in the deployment guide.

### Server
- No changes apart from the version number.

## 0.1.0 (2026-11-07)

First release: the server, the Obsidian plugin for Windows and Android, and automated tests.
