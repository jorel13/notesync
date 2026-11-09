# notesync

Self-hosted sync for Obsidian vaults: a small Rust server you run yourself, and an
Obsidian plugin for desktop and mobile.

> **Status: early development (0.3.0, see the [changelog](CHANGELOG.md)).** The server and plugin are complete for v1 and
> covered by automated tests, but haven't had much real-world use yet. Keep a backup of
> your vault.

- Multiple users per server, each with any number of private vaults
- Accounts are created by an admin; devices sign in with their own revocable tokens
- Edits to different notes never conflict; overlapping edits are merged or kept as
  conflict copies, never silently dropped
- Old versions and deleted files are kept for a configurable period

See [docs/design.md](docs/design.md) for the full design.

## Repository layout

| Folder | Contents |
|---|---|
| `server/` | Rust server: HTTP API, SQLite database, file storage, admin commands |
| `plugin/` | Obsidian plugin for desktop and mobile ([details](plugin/README.md)) |
| `docs/` | [Design](docs/design.md) and [deployment guide](docs/deployment.md) |
| `deploy/` | Example Docker Compose file and settings |

## Server

### Run it locally

Requires a recent stable Rust toolchain.

```bash
cd server
cargo run -- user add alice --admin   # prompts for a password
cargo run -- serve                    # listens on http://127.0.0.1:8080
```

Check it's up at `http://127.0.0.1:8080/api/v1/health`.

### Commands

| Command | Purpose |
|---|---|
| `notesync-server serve` | Run the server |
| `notesync-server user add <name> [--admin]` | Create a user |
| `notesync-server user passwd <name>` | Set a new password and sign the user out everywhere |
| `notesync-server user disable <name>` / `enable <name>` | Block or unblock a user |
| `notesync-server user make-admin <name>` | Give a user admin rights |
| `notesync-server user unlock <name>` | Clear a lockout after too many wrong passwords |
| `notesync-server user list` | List users, their signed-in devices and any lockout |
| `notesync-server vault list --user <name> [--deleted]` | List a user's vaults, or the deleted ones that can be restored |
| `notesync-server vault restore --user <name> <id>` | Restore a deleted vault |
| `notesync-server gc` | Remove expired file versions and deleted vaults now |

Commands that take a password prompt for it; add `--password-stdin` to read it from
standard input instead.

### Admin page

Open `/admin` on the server (for example `http://127.0.0.1:8080/admin`) and sign in with an
admin account. It does what the user and vault commands do, plus a server overview: add
users, set passwords, disable or enable accounts, grant admin rights, clear lockouts, sign
out devices, restore deleted vaults and run cleanup. Create the first admin with
`user add <name> --admin`.

### Configuration

| Variable | Default | Purpose |
|---|---|---|
| `NOTESYNC_BIND_ADDR` | `127.0.0.1:8080` | Listen address |
| `NOTESYNC_DATA_DIR` | `./data` | Database and stored files |
| `NOTESYNC_MAX_FILE_MB` | `100` | Largest file accepted |
| `NOTESYNC_HISTORY_DAYS` | `30` | How long old versions and deleted vaults are kept |
| `NOTESYNC_TOKEN_DAYS` | `30` | A device's sign-in expires if it doesn't sync for this long. Devices renew it about once a day. |
| `NOTESYNC_LOGIN_BURST` / `NOTESYNC_LOGIN_EVERY_SECONDS` | `10` / `5` | Sign-in attempts per username: 10 at once, then one every 5 seconds |
| `NOTESYNC_SYNC_BURST` / `NOTESYNC_SYNC_EVERY_SECONDS` | `10` / `5` | Sync runs per device, the same way |
| `NOTESYNC_REQUEST_BURST` / `NOTESYNC_REQUESTS_PER_SECOND` | `600` / `20` | All other requests per device (a sync makes one per file it uploads or downloads) |
| `NOTESYNC_ADMIN_PAGE` | `on` | Serve the admin web page at `/admin`; `off` removes it |
| `NOTESYNC_LOG` | `info` | Log level (`error`, `warn`, `info`, `debug`) |

### Tests

```bash
cd server
cargo test
```

Unit tests cover path rules, password and token handling, rate limiting and file storage.
The API tests in `server/tests/` run the whole server in-process against a temporary
database: accounts, isolation between users, uploads, commits and conflicts, change feeds,
history and cleanup.

## Deployment

The server speaks plain HTTP. **Don't expose it directly to a network**: put it behind
something that provides HTTPS and access control, such as
[Tailscale Serve or Services](https://tailscale.com/docs/features/tailscale-services) or a
reverse proxy like Caddy.

Releases publish the image as `ghcr.io/<owner>/notesync-server:<version>`, or build it
yourself:

```bash
docker build -t notesync-server server
```

Then adapt [deploy/compose.example.yaml](deploy/compose.example.yaml), and create the first
user inside the container:

```bash
docker exec -it <container> notesync-server user add alice --admin
```

Back up the data folder (`notesync.db` and `blobs/`) like any other database.

[docs/deployment.md](docs/deployment.md) covers the whole setup, including TrueNAS,
HTTPS, installing the plugin on each device and updating.

## License

[MIT](LICENSE)
