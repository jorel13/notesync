# Deploying notesync

This guide runs the server in Docker behind an HTTPS front end, then installs the plugin on
each device. Replace anything in `<angle brackets>` with your own values.

```mermaid
flowchart LR
  subgraph Devices["Each device (Windows, Android)"]
    OBS["Obsidian<br/>+ notesync plugin"]
    TSC["Tailscale client"]
    OBS --> TSC
  end

  subgraph Host["Server host (e.g. TrueNAS)"]
    TSD["Tailscale<br/>serve / Services<br/>HTTPS :443"]
    subgraph Container["notesync container (user 568)"]
      SRV["notesync-server<br/>sync API :8080"]
      ADM["admin page :8081"]
    end
    subgraph Dataset["Dataset, e.g. apps/notesync"]
      DB[("notesync.db")]
      BLOBS[("blobs/")]
    end
    SNAP["Periodic<br/>snapshots"]
    TSD -- "svc:notes<br/>127.0.0.1:30080" --> SRV
    TSD -- "svc:notes-admin<br/>127.0.0.1:30081" --> ADM
    SRV --> DB
    SRV --> BLOBS
    SNAP -.-> Dataset
  end

  TSC -- "https://notes.&lt;tailnet&gt;.ts.net<br/>https://notes-admin.&lt;tailnet&gt;.ts.net<br/>(WireGuard)" --> TSD
```

The server listens on two ports: the **sync API** (8080) for every device, and the **admin
page** (8081) for admins. Both are bound to the host's loopback address. Tailscale (or a
reverse proxy) adds HTTPS and decides who can reach each one; notesync's own accounts decide
whose vaults they see. Publishing them separately means people who only sync can't even reach
the admin sign-in.

## 1. Get the server image

Each release publishes `ghcr.io/<owner>/notesync-server:<version>` (see
[.github/workflows/release.yml](../.github/workflows/release.yml)). If the repository is
private, make the package public in GitHub (Packages > notesync-server > Package settings),
or log in to `ghcr.io` on the host first.

To build it yourself instead:

```bash
docker build -t notesync-server server
```

## 2. Run the server

1. Create a folder for its data, owned by the user the container runs as (568:568 by
   default, which matches the `apps` user on TrueNAS).
2. Start it with [deploy/compose.example.yaml](../deploy/compose.example.yaml), adapted to
   your paths. The example binds both ports to `127.0.0.1` (30080 for sync, 30081 for the
   admin page) so they're reachable only through the HTTPS front end on the same machine.
3. Check it's up from the host:

   ```bash
   curl http://127.0.0.1:30080/api/v1/health
   ```

   `http://127.0.0.1:30081/admin` should return the admin page's HTML.

### On TrueNAS

1. Create a dataset such as `<pool>/apps/notesync` and give user `apps` (568) read and
   write access.
2. Apps > Discover Apps > ⋮ > **Install via YAML**, and paste the compose example with the
   image, port and `/mnt/<pool>/apps/notesync` path filled in. TrueNAS doesn't read `.env`
   files, so write the values directly instead of using `${...}`.

## 3. Put HTTPS in front

The plugin needs an `https://` address, and so does the admin page (its sign-in cookie is
marked secure). Publish the two ports as two names:

- **Tailscale Services (recommended):** on a tagged host, define two Services, `notes` and
  `notes-admin`, then run

  ```bash
  tailscale serve --service=svc:notes --https=443 http://127.0.0.1:30080
  ```
  ```bash
  tailscale serve --service=svc:notes-admin --https=443 http://127.0.0.1:30081
  ```

  This gives `https://notes.<tailnet>.ts.net` and `https://notes-admin.<tailnet>.ts.net`. Limit
  `notes-admin` to yourself with grants in the tailnet policy, for example:

```jsonc
"grants": [
  // You: everything, including the admin page.
  {"src": ["you@example.com"], "dst": ["*"], "ip": ["*"]},
  {"src": ["you@example.com"], "dst": ["svc:notes", "svc:notes-admin"], "ip": ["tcp:443"]},
  // Everyone in the tailnet: their own devices and the sync API, nothing else.
  {"src": ["autogroup:member"], "dst": ["autogroup:self"], "ip": ["*"]},
  {"src": ["autogroup:member"], "dst": ["svc:notes"], "ip": ["tcp:443"]},
],
```

  Remove Tailscale's default allow-everything rule (`"src": ["*"], "dst": ["*:*"]`), or it
  overrides this. Check the result in the policy editor's **Preview rules** tab.
- **Caddy or another reverse proxy**, with a certificate for your own domain. Put the admin
  port on a name only you can reach, for example one that only answers on your LAN.

Serve each at the root of its own name; neither supports sub-paths. The sync address has no
admin page: `https://notes.<tailnet>.ts.net/admin` returns Not Found.

## 4. Create accounts

Open a shell in the container (on TrueNAS: Apps > notesync > Shell) and run:

```bash
notesync-server user add <name> --admin
```

It asks for the password. You can add the other users from the admin page (below) or with
`notesync-server user add <name>`. `notesync-server user list` shows them; see the main README
for the other commands.

### The admin page

Open `https://notes-admin.<tailnet>.ts.net` and sign in with an admin account. From there you
can add and delete users, set passwords, disable accounts, grant admin rights, clear
lockouts, sign out lost devices, restore deleted vaults, see storage use and run cleanup.

- It's on its own port (8081 in the container, `NOTESYNC_ADMIN_BIND_ADDR`), never on the
  sync port, so you can publish it to fewer people.
- Only admin accounts can sign in. A session ends after 30 minutes without use, or 12 hours
  after signing in.
- Deleting a user, setting someone's password and granting admin rights ask for your own
  password again.
- Every action is logged with your username.
- It needs HTTPS (or `localhost`), because the sign-in cookie is marked secure.
- To turn it off, set `NOTESYNC_ADMIN_PAGE=off`; the command line still works.

## 5. Install the plugin

The plugin isn't in Obsidian's community directory. Install it on each device one of these ways.

**With BRAT (easiest, also gets updates):** install the community plugin "BRAT", then
use its **Add beta plugin** command with the repository's GitHub address. BRAT reads the
`main.js`, `manifest.json` and `styles.css` attached to the latest release. It's simplest
with a public repository; for a private one, check BRAT's documentation for token setup.

**By hand:** download `main.js`, `manifest.json` and `styles.css` from a release (or build
them with `npm run build` in `plugin/`), and copy them into
`<vault>/.obsidian/plugins/notesync/`. On Android, the vault is usually in
`Documents/<vault name>`. Use a file manager or a USB cable, and show hidden folders to see
`.obsidian`. Then restart Obsidian and turn the plugin on in Settings > Community plugins.

The plugin needs Obsidian 1.11.4 or newer.

## 6. Connect each device

1. Settings > notesync: enter the server URL (for example `https://notes.<tailnet>.ts.net`),
   your username and password, then **Sign in**. The password is used once and isn't saved;
   the device's token goes in Obsidian's secret storage.
2. **Choose a vault** or create one. On the first device, create a new server vault. On
   other devices, choose the same one.
3. Confirm the first sync. It combines both sides and never deletes anything: files with the
   same path and different contents keep the server version, plus a conflict copy.

**Tip for the second device:** start from an empty vault and let the first sync download
everything. That avoids conflict copies for files that differ only slightly.

## Managing people and devices

Someone needs two things to sync: a way to **reach** the server (with Tailscale, a login on
your tailnet) and a **notesync account**. They're separate, and removing either one cuts off
access. Do notesync tasks in the admin page or with the command line in the container.

### Sharing with family or friends

With Tailscale, each person **joins your tailnet as a user**. Tailscale's "share a machine"
feature doesn't work here: it can't share tagged devices, and a Services host must be
tagged. The free Personal plan allows 6 users with unlimited devices.

1. **Tailscale:** invite them from the admin console's **Users** page, as a **Member** (not an
   Admin). They install the Tailscale app on each device and sign in with the invited
   account. Your grants decide what they reach: with the example above, only `notes` and
   their own devices. Add other Services to the `autogroup:member` grant if you want to share
   them; never `notes-admin`.
2. **notesync:** in the admin page, **Add user** with a temporary password (leave Admin off),
   and give it to them privately.
3. **Their devices:** install the plugin, sign in with `https://notes.<tailnet>.ts.net`, and
   create a vault. Then have them change the password in Settings > notesync.

Each person's vaults are private to their account. Admins see vault names and sizes in the
admin page, not the notes.

### Common tasks

| Task | Admin page | Command line | Tailscale |
|---|---|---|---|
| Add a person | **Add user** | `user add <name>` | Invite them on the Users page |
| Forgotten password | the user > **Set password** (signs them out everywhere) | `user passwd <name>` | – |
| Locked out after wrong passwords | the user > **Unlock sign-in** | `user unlock <name>` | – |
| New device | – (they just sign in) | – | They sign in to the Tailscale app on it |
| Lost or stolen device | the user > Signed-in devices > **Sign out** | – (or `user passwd`, which signs out all) | Machines page > the device > **Remove** |
| Block someone, keep their notes | the user > **Disable** | `user disable <name>` | Optionally remove them on the Users page |
| Remove someone and their notes | the user > **Delete user** | `user delete <name>` | Remove them on the Users page |
| Restore a deleted vault | the user > Deleted vaults > **Restore** | `vault restore --user <name> <id>` | – |
| Another admin | the user > **Make admin** | `user make-admin <name>` | Add them to the `notes-admin` grant |

**Deleting a user** permanently removes their account, vaults, history and stored files. It
can't be undone, and Restore doesn't apply. Their devices keep their local copies. The only
admin can't be deleted, and an admin can't delete their own account from the admin page.

## Backups

Back up the data folder: `notesync.db` and `blobs/`. Stop the container first, or take a
filesystem snapshot (on TrueNAS, a periodic snapshot of the dataset). Each device's vault is
also a full copy of the notes.

## Limits and maintenance

**Limits**

| What | Limit |
|---|---|
| One file | `NOTESYNC_MAX_FILE_MB`, 100 MB by default. Larger files are skipped by the plugin and listed in its last-sync report. Uploads that large also need the HTTPS front end to allow them. |
| Total storage | None built in, and no per-user quota. The dataset's free space is the limit. |
| Users, vaults, devices | No fixed limit. |
| Sign-in attempts | 10 per username, then one every 5 seconds. 10 *wrong* passwords within 15 minutes lock the username for 15 minutes (`user unlock` clears it). |
| Syncs | 10 per device, then one every 5 seconds. Normal use is far below this. |
| Other requests | 600 per device, then 20 per second. A large first sync is slowed down, not refused. |
| Sign-in lifetime | A device that doesn't sync for `NOTESYNC_TOKEN_DAYS` (30) must sign in again. Devices that sync renew it about once a day. |

All of these are environment variables; see the [README](../README.md#configuration).

**Storage use.** Every version of every file stays on the server for `NOTESYNC_HISTORY_DAYS`
(30 by default) after it's replaced or deleted. A vault that changes a lot can therefore use
noticeably more space than its current size, especially with large attachments that change.
Lower the history period to save space, or raise it to keep more history.

**What the server does by itself**
- Once a day: removes file versions and deleted files older than the history period, uploads
  that were never used, and vaults deleted more than the history period ago. Run it any time
  with `notesync-server gc`.
- On start: applies database updates.

**What it doesn't do**
- Back itself up: see [Backups](#backups).
- Shrink the database file: SQLite reuses freed space instead. The database holds only
  accounts and the list of changes, so it stays small (a few MB for tens of thousands of
  changes); file contents live in `blobs/`.
- Trim the list of changes: it's kept forever, which is what lets every device catch up
  however long it's been offline.

**How many users.** Not load-tested yet; these are estimates from the design. A device checks
in every few minutes and after edits, so dozens of users with several devices each is light
work, and a few hundred light users should be fine. The database accepts one commit at a time
across all vaults; commits are short, so that only matters with many devices saving at the
same moment. Disk space and upload speed for large files are the practical limits. It isn't
designed for an organization with thousands of users.

**Things to check now and then**
- Free space on the dataset, and the size of `blobs/`.
- That snapshots are being taken.
- The server logs for repeated errors, `failed sign-in` and `account locked` lines.
- `notesync-server user list` (or the admin page) for lockouts and for accounts that should
  be disabled.

**If a device is lost or a token may have leaked:** sign that device out from another
device or from the admin page, or reset the password with `notesync-server user passwd <name>`, which signs out
every device. An unused token stops working by itself after `NOTESYNC_TOKEN_DAYS`.

**If a vault was deleted by mistake:** restore it from the user's page in the admin page,
or use the command line: `notesync-server vault list --user <name> --deleted` shows it and how many days are left, and `notesync-server vault restore --user <name> <id>`
brings it back.

## Updating

1. Update the server image tag and redeploy. Database changes run automatically on start.
2. Update the plugin on each device, through BRAT or by copying the new files. Obsidian only
   loads the new code after a restart: close and reopen it, or run **Reload app without
   saving** from the command palette.

The server rejects plugin versions older than it supports, with a message asking you to
update the plugin. The minimum only goes up when the protocol changes or an old plugin has a
security problem; the changelog says when. **Update the server first, then the plugin on
each device:** a newer plugin works with the same server version, but an older plugin may be
refused by a newer server.

**Upgrading to 0.2.0:** 0.2.0 servers refuse 0.1.x plugins (they don't check the paths the
server sends, and can't renew their sign-in), so update every device's plugin right after
the server. Devices stay signed in: existing sign-ins get 30 days from the upgrade and then
renew themselves.

**Upgrading to 0.3.0:** adds the admin page. 0.2.0 plugins keep working, so the plugin
update can wait. The database change (a table for admin page sign-ins) runs on start.

**Upgrading to 0.4.0:** the admin page moves from `/admin` on the sync port to its own port,
8081 in the container. Add the second port to your compose file (`127.0.0.1:30081:8081` in
the example), publish it as its own name (`svc:notes-admin`), and limit that name to admins
(see [Put HTTPS in front](#3-put-https-in-front)). Plugins don't need updating.

**Check which versions are running**
- **Plugin:** Settings > notesync > Status shows "Plugin version x.y.z", the build Obsidian
  actually loaded. Settings > Community plugins shows it too.
- **Server:** `https://notes.<tailnet>.ts.net/api/v1/health` returns its version.

Every change to the plugin or server gets a new version number (see
[CHANGELOG.md](../CHANGELOG.md)), so this tells you whether a fix has reached a device.
