# notesync Obsidian plugin

Syncs an Obsidian vault with a notesync server, on desktop and mobile. See
[docs/deployment.md](../docs/deployment.md) for installing and connecting it.

## Using it

- **Settings > notesync:** server URL, sign in, choose or create a server vault, and when to
  sync. The password is only used to sign in; the device token is kept in Obsidian's secret
  storage (Obsidian 1.11.4 or newer).
- **Syncs automatically** when Obsidian starts, a few seconds after you stop editing, every
  few minutes, and when you come back to the app on mobile. All of these can be changed.
- **Sync now:** the ribbon icon, the status bar item on desktop, or the "Sync now" command.
- **Show what the last sync did:** a command that lists downloads, uploads, merges,
  conflict copies and skipped files.
- **Status:** the status bar on desktop. On mobile the ribbon icon spins while syncing and
  turns into a warning icon after an error.

## Layout

| Folder | Contents |
|---|---|
| `src/engine/` | The sync engine, in plain TypeScript with no Obsidian imports |
| `src/api/` | HTTP client for the server, with a swappable transport (`fetch` in tests, Obsidian's `requestUrl` in the plugin) |
| `src/sync/` | When to sync, status and messages, and ignoring the sync's own file events (no Obsidian imports) |
| `src/obsidian/` | Obsidian glue: vault file access, saved state, settings screen, dialogs |
| `src/main.ts` | Plugin entry point |
| `tests/` | Unit tests, engine tests against an in-memory server, and end-to-end tests against the real server |

The engine talks to three small interfaces (`src/engine/types.ts`): `VaultFs` for files,
`ServerApi` for the server and `StateStore` for what this device remembers between syncs.
The sync record is saved in `sync-state.json` in the plugin's folder.

## How a sync works

1. **Pull:** fetch server changes since the last sync and apply them. Downloads, trashes and
   renames only happen if the local file hasn't changed since the scan.
2. **Merge where both sides changed:** Markdown and text files are merged line by line;
   overlapping edits and other file types keep the server version in place and save this
   device's version as `Name (conflict YYYY-MM-DD HHMM Device).ext`. Edits always win over
   deletes.
3. **Push:** upload new content, then commit all local changes in one all-or-nothing request.
   If another device committed in between, the whole run starts again (up to three times).

Safety checks: `.obsidian/`, `.trash/` and `.git/` are never synced; files with names the
server rejects, or above the size limit, are skipped and never deleted; files deleted by a
sync go to the vault's `.trash/` folder; and a sync that would delete many files asks first.

## Development

```bash
npm install
npm run typecheck
npm run lint
npm test
npm run build      # writes main.js
npm run dev        # rebuilds main.js on every change
```

To try it in Obsidian, copy or link `main.js`, `manifest.json` and `styles.css` into
`<test vault>/.obsidian/plugins/notesync/`. Use a copy of a vault, not your real one.

The end-to-end tests in `tests/e2e.test.ts` start the real server, so build it first
(`cargo build` in `../server`). They're skipped if the server binary is missing, unless
`NOTESYNC_E2E=1` is set, as in CI.

## Releasing

Every change gets a new version, so a device's Settings > notesync > Status shows whether it
has the fix. Set the same version in `manifest.json`, `package.json`, `versions.json` and
`../server/Cargo.toml`, add an entry to `../CHANGELOG.md`, then push a tag with that version (for example `0.1.1`). The release
workflow checks the numbers match, attaches the plugin files to a GitHub release and
publishes the server image.
